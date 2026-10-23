//! Who has to act for a node to move: the one answer every view reads
//! (`doc/lifecycle/next-actor.md`).
//!
//! Only nodes with the Lifecycle capability (a `node_lifecycle` row) have
//! one. For those, [`for_nodes`] derives a [`NodeStanding`] from what is
//! durable (the node's requests, its pending waits, its lifecycle state) and
//! from what the caller knows about its run ([`RunFacts`]), and [`derive`] is
//! the pure rule that turns those into one [`NextActor`]:
//!
//! 1. `done` → [`NextActor::Done`].
//! 2. A pending permission → `You`.
//! 3. Automated processing live → `Working`.
//! 4. Open requests → `You`.
//! 5. The last run failed, stopped, or was paused → `You`.
//! 6. A pending wait on something outside → `Others`.
//! 7. Otherwise → `You` (`NotStarted`): nothing is ever idle.
//!
//! Derived, never stored, so it cannot drift from what it is derived from.
//! A user's deferral ([`Deferral`]) is not an actor: the node stays `You`
//! and the deferral rides beside it, ending on its own when it expires or
//! when something new reaches the user.

use crate::attention::{self, NodeAttention};
use crate::autopilot::local::{FAILED_PREFIX, stopped_by_user};
use crate::autopilot::{BudgetLimit, NeedsHuman, Outcome};
use crate::runner_status::{DONE_STATE, format_elapsed};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use tod_store::conversation::ProtocolKind;
use tod_store::outline::repos::NodeRepo;
use tod_store::waits::{Wait, WaitRepo};
use uuid::Uuid;

/// Longest an `Others` wait may go unobserved past its next check, at least,
/// before it is shown as overdue (see [`ExternalWait::overdue`]).
pub const OVERDUE_GRACE_MS: i64 = 2 * 60 * 1000;

/// Who has to act for the node to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextActor {
    /// Automated processing for the node is in progress (an agent turn, a
    /// gate check, a runner step). Nothing for the user to do.
    Working { what: Work, since: i64 },
    /// The user has to do something; the node will not move until they do.
    You { why: YouReason, since: i64 },
    /// Someone or something else has to act. Resolves without the user.
    Others { wait: ExternalWait },
    /// Terminal: the `done` phase has this one state.
    Done,
}

/// What the processing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// A conversation turn under this protocol (a phase agent, Implement,
    /// Verify, Review, Fix, the pull request babysitter, an evaluator).
    Conversation(ProtocolKind),
    /// The app checking a gate.
    GateCheck,
    /// The app checking incoming changes.
    IncomingCheck,
}

/// Why the user is the one who has to act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YouReason {
    /// Open requests (`crate::attention`): decisions, handed-back plan
    /// steps, findings. The items themselves come from `attention`.
    Request { count: usize },
    /// An agent asked for a permission nobody has granted.
    Permission { title: String },
    /// The last run failed, or the app itself did.
    Failed { error: String },
    /// The run stopped for a reason that is not a request (its budget, a step
    /// that changed nothing, an agent stuck).
    Stopped { reason: String },
    /// The user paused the runner.
    Paused,
    /// Work remains and nothing is running, asked, or waited on.
    NotStarted,
}

/// What the node waits on that is outside the user's hands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalWait {
    pub kind: WaitKind,
    /// Whom to ask, when known (a reviewer's login). Not recorded yet.
    pub who: Option<String>,
    /// What the wait is for, in words, as of the last look.
    pub detail: Option<String>,
    /// When the wait began (ms since the epoch).
    pub since: i64,
    /// When GitHub (or whatever it is) is looked at next (ms).
    pub next_check_ms: Option<i64>,
    /// The next look is well past due and nothing has been observed since:
    /// the thing that should be looking is not. Derived, never stored.
    pub overdue: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// A person's review of the pull request.
    HumanReview,
    /// The merge.
    Merge,
    /// A release that names the pull request.
    Release,
    /// Any other pending wait (a time, a command, an event we do not name).
    Other,
}

impl WaitKind {
    /// The kind of an `event` wait from its `match_spec`
    /// (`autopilot::github_wait`, `autopilot::babysit`).
    pub fn from_spec(spec: &str) -> Self {
        if spec.starts_with("github:release:") {
            Self::Release
        } else if spec.starts_with("github:pr:") && spec.ends_with(":merged") {
            Self::Merge
        } else if spec.starts_with("github:pr:") && spec.ends_with("review") {
            Self::HumanReview
        } else {
            Self::Other
        }
    }
}

/// The user took the node off their list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deferral {
    /// When it returns by itself; `None` until the user brings it back.
    pub until: Option<i64>,
    /// When the user deferred it. Anything that reaches the user after this
    /// wakes the node.
    pub deferred_at: i64,
}

/// A node's actor, and whether the user has deferred it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStanding {
    pub node_id: Uuid,
    pub actor: NextActor,
    /// Set only while the deferral holds: the node is `You` and nothing new
    /// has reached the user since.
    pub deferred: Option<Deferral>,
}

impl NodeStanding {
    /// On the user's list now: `You`, and not deferred.
    pub fn on_you(&self) -> bool {
        matches!(self.actor, NextActor::You { .. }) && self.deferred.is_none()
    }

    /// The `since` the "waiting longest" order sorts by.
    pub fn since(&self) -> Option<i64> {
        match &self.actor {
            NextActor::Working { since, .. } | NextActor::You { since, .. } => Some(*since),
            NextActor::Others { wait } => Some(wait.since),
            NextActor::Done => None,
        }
    }
}

/// What the caller knows about a node's run that is not in the store: the
/// daemon's or the app's. Absent means nothing is known.
#[derive(Debug, Clone, Default)]
pub struct RunFacts {
    /// Automated processing is live, proved by a fresh activity lease. The
    /// caller must not set this from anything weaker.
    pub working: Option<(Work, i64)>,
    /// A permission an agent asked for, and since when.
    pub permission: Option<(String, i64)>,
    /// How the last run ended, and when.
    pub last_outcome: Option<(Outcome, i64)>,
}

/// What [`for_nodes`] needs besides the store.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub runs: HashMap<Uuid, RunFacts>,
    pub deferrals: HashMap<Uuid, Deferral>,
    pub now_ms: i64,
}

/// Everything [`derive`] decides from, for one node.
pub struct Facts<'a> {
    pub node_id: Uuid,
    pub lifecycle: &'a str,
    /// When the node entered `lifecycle` (ms).
    pub entered_at: i64,
    pub attention: &'a NodeAttention,
    pub waits: &'a [Wait],
    pub run: &'a RunFacts,
    pub deferral: Option<Deferral>,
    pub now_ms: i64,
}

/// Every node in `node_ids` that has the Lifecycle capability, with who has
/// to act for it, batched (a handful of queries, not one per node).
pub fn for_nodes(
    conn: &Connection,
    node_ids: &[Uuid],
    inputs: &Inputs,
) -> Result<HashMap<Uuid, NodeStanding>> {
    if node_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let lifecycles = NodeRepo::new(conn).get_lifecycle_entered_for_nodes(node_ids)?;
    let attention = attention::for_nodes(conn, node_ids)?;
    let mut waits: HashMap<Uuid, Vec<Wait>> = HashMap::new();
    for wait in WaitRepo::new(conn).list_pending_for_nodes(node_ids)? {
        waits.entry(wait.node_id).or_default().push(wait);
    }
    let no_run = RunFacts::default();
    let no_attention = NodeAttention::default();
    let mut out = HashMap::new();
    for &node_id in node_ids {
        let Some((lifecycle, entered_at)) = lifecycles.get(&node_id) else {
            continue;
        };
        let standing = derive(&Facts {
            node_id,
            lifecycle,
            entered_at: *entered_at,
            attention: attention.get(&node_id).unwrap_or(&no_attention),
            waits: waits.get(&node_id).map(Vec::as_slice).unwrap_or(&[]),
            run: inputs.runs.get(&node_id).unwrap_or(&no_run),
            deferral: inputs.deferrals.get(&node_id).copied(),
            now_ms: inputs.now_ms,
        });
        out.insert(node_id, standing);
    }
    Ok(out)
}

/// The rule: see the module comment for the order.
pub fn derive(facts: &Facts<'_>) -> NodeStanding {
    let (actor, newest) = actor_for(facts);
    let deferred = match (&actor, facts.deferral) {
        (NextActor::You { .. }, Some(d)) if deferral_holds(&d, newest, facts.now_ms) => Some(d),
        _ => None,
    };
    NodeStanding {
        node_id: facts.node_id,
        actor,
        deferred,
    }
}

/// A deferral holds until it expires, or until something reaches the user
/// that did not exist when they deferred it.
fn deferral_holds(deferral: &Deferral, newest: i64, now_ms: i64) -> bool {
    let expired = deferral.until.is_some_and(|until| until <= now_ms);
    !expired && newest <= deferral.deferred_at
}

/// The actor, and when the newest thing reaching the user arrived (for the
/// deferral's wake rule).
fn actor_for(facts: &Facts<'_>) -> (NextActor, i64) {
    if facts.lifecycle == DONE_STATE {
        return (NextActor::Done, 0);
    }
    let run = facts.run;
    if let Some((title, since)) = &run.permission {
        return (
            NextActor::You {
                why: YouReason::Permission { title: title.clone() },
                since: *since,
            },
            *since,
        );
    }
    if let Some((what, since)) = run.working {
        return (NextActor::Working { what, since }, 0);
    }
    let requests = facts.attention;
    if let Some(waiting_since) = requests.waiting_since {
        let newest = requests.items.iter().map(|i| i.since).max().unwrap_or(waiting_since);
        return (
            NextActor::You {
                why: YouReason::Request { count: requests.count },
                since: waiting_since,
            },
            newest,
        );
    }
    if let Some((outcome, at)) = &run.last_outcome
        && let Some(why) = outcome_reason(outcome)
    {
        return (NextActor::You { why, since: *at }, *at);
    }
    if let Some(wait) = facts.waits.first() {
        return (
            NextActor::Others {
                wait: external_wait(wait, facts.now_ms),
            },
            0,
        );
    }
    // A run that ended to wait, with no wait row left to say so, is still a
    // wait on something outside, and says so.
    if let Some((Outcome::Waiting { what, due_at_ms }, at)) = &run.last_outcome {
        return (
            NextActor::Others {
                wait: ExternalWait {
                    kind: WaitKind::Other,
                    who: None,
                    detail: Some(what.clone()),
                    since: *at,
                    next_check_ms: Some(*due_at_ms),
                    overdue: *due_at_ms + OVERDUE_GRACE_MS < facts.now_ms,
                },
            },
            0,
        );
    }
    (
        NextActor::You {
            why: YouReason::NotStarted,
            since: facts.entered_at,
        },
        facts.entered_at,
    )
}

/// What a run's last outcome asks of the user, when it asks anything.
/// Requests are `attention`'s: an outcome that stopped for one is stale once
/// none is left (the run continues by itself), so it asks nothing here.
fn outcome_reason(outcome: &Outcome) -> Option<YouReason> {
    match outcome {
        Outcome::Done | Outcome::Waiting { .. } => None,
        outcome if stopped_by_user(outcome) => Some(YouReason::Paused),
        Outcome::NeedsHuman { reason } => match reason {
            NeedsHuman::AgentFailed { error } => Some(YouReason::Failed { error: error.clone() }),
            reason if reason.is_request() => None,
            reason => Some(YouReason::Stopped { reason: reason.describe() }),
        },
        Outcome::Stopped { reason } => Some(match reason.strip_prefix(FAILED_PREFIX) {
            Some(error) => YouReason::Failed { error: error.to_string() },
            None => YouReason::Stopped { reason: reason.clone() },
        }),
        Outcome::BudgetExhausted { limit } => Some(YouReason::Stopped {
            reason: match limit {
                BudgetLimit::Sessions { used } => format!("budget spent: {used} sessions"),
                BudgetLimit::Time { elapsed_secs } => {
                    format!("budget spent: {} of work", format_elapsed(*elapsed_secs as i64 * 1000))
                }
            },
        }),
    }
}

fn external_wait(wait: &Wait, now_ms: i64) -> ExternalWait {
    // The poller reschedules a wait it has just looked at, so the gap between
    // the last update and the next check is the interval it keeps.
    let interval = (wait.due_at - wait.updated_at).max(0);
    let grace = (2 * interval).max(OVERDUE_GRACE_MS);
    ExternalWait {
        kind: WaitKind::from_spec(&wait.match_spec),
        who: None,
        detail: wait.detail.clone().filter(|d| !d.trim().is_empty()),
        since: wait.created_at,
        next_check_ms: Some(wait.due_at),
        overdue: wait.due_at + grace < now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::{AttentionItem, AttentionKind, RequestReason};
    use tod_store::fleet::schema;
    use tod_store::outline::repos::NodeRepo;
    use tod_store::waits::NewWait;

    const NOW: i64 = 1_000_000;

    fn wait(spec: &str, due_at: i64, updated_at: i64) -> Wait {
        Wait {
            id: Uuid::new_v4(),
            node_id: Uuid::nil(),
            kind: "event".into(),
            match_spec: spec.into(),
            every_secs: None,
            due_at,
            state: "pending".into(),
            created_at: 100,
            updated_at,
            detail: Some("a review of the pull request".into()),
        }
    }

    fn requests(node: Uuid, sinces: &[i64]) -> NodeAttention {
        let items: Vec<AttentionItem> = sinces
            .iter()
            .map(|&since| AttentionItem {
                kind: AttentionKind::Decision,
                id: Uuid::new_v4(),
                node_id: node,
                summary: "which?".into(),
                options: Vec::new(),
                since,
                reason: RequestReason::Intent,
            })
            .collect();
        let mut sorted = items;
        sorted.sort_by_key(|i| i.since);
        NodeAttention {
            node_id: node,
            waiting_since: sorted.first().map(|i| i.since),
            count: sorted.len(),
            items: sorted,
        }
    }

    struct Case {
        lifecycle: &'static str,
        attention: NodeAttention,
        waits: Vec<Wait>,
        run: RunFacts,
        deferral: Option<Deferral>,
    }

    fn case(lifecycle: &'static str) -> Case {
        Case {
            lifecycle,
            attention: NodeAttention::default(),
            waits: Vec::new(),
            run: RunFacts::default(),
            deferral: None,
        }
    }

    fn standing(c: &Case) -> NodeStanding {
        derive(&Facts {
            node_id: Uuid::nil(),
            lifecycle: c.lifecycle,
            entered_at: 500,
            attention: &c.attention,
            waits: &c.waits,
            run: &c.run,
            deferral: c.deferral,
            now_ms: NOW,
        })
    }

    fn you(why: YouReason, since: i64) -> NextActor {
        NextActor::You { why, since }
    }

    #[test]
    fn done_is_terminal_whatever_else_is_recorded() {
        let mut c = case("done");
        c.attention = requests(Uuid::nil(), &[10]);
        c.run.working = Some((Work::GateCheck, 20));
        assert_eq!(standing(&c).actor, NextActor::Done);
    }

    #[test]
    fn a_node_with_nothing_is_not_started_and_never_idle() {
        let c = case("proposed");
        let s = standing(&c);
        assert_eq!(s.actor, you(YouReason::NotStarted, 500));
        assert!(s.on_you());
    }

    #[test]
    fn processing_with_a_lease_is_working() {
        let mut c = case("active");
        c.run.working = Some((Work::Conversation(ProtocolKind::Implementation), 30));
        assert_eq!(
            standing(&c).actor,
            NextActor::Working { what: Work::Conversation(ProtocolKind::Implementation), since: 30 }
        );
    }

    #[test]
    fn a_pending_permission_beats_working() {
        let mut c = case("active");
        c.run.working = Some((Work::Conversation(ProtocolKind::Implementation), 30));
        c.run.permission = Some(("Run `rm`".into(), 40));
        assert_eq!(standing(&c).actor, you(YouReason::Permission { title: "Run `rm`".into() }, 40));
    }

    #[test]
    fn working_beats_open_requests_and_a_stale_outcome() {
        let mut c = case("review");
        c.attention = requests(Uuid::nil(), &[10]);
        c.run.last_outcome = Some((Outcome::Stopped { reason: "budget".into() }, 5));
        c.run.working = Some((Work::GateCheck, 60));
        assert!(matches!(standing(&c).actor, NextActor::Working { .. }));
    }

    #[test]
    fn open_requests_are_you_since_the_oldest() {
        let mut c = case("design");
        c.attention = requests(Uuid::nil(), &[300, 200]);
        assert_eq!(standing(&c).actor, you(YouReason::Request { count: 2 }, 200));
    }

    #[test]
    fn requests_beat_a_pending_wait() {
        let mut c = case("pr");
        c.attention = requests(Uuid::nil(), &[10]);
        c.waits = vec![wait("github:pr:1:review", NOW + 1000, NOW)];
        assert!(matches!(standing(&c).actor, NextActor::You { why: YouReason::Request { .. }, .. }));
    }

    #[test]
    fn a_failed_run_is_you_with_the_error_even_with_a_wait_pending() {
        let mut c = case("pr");
        c.waits = vec![wait("github:pr:1:review", NOW + 1000, NOW)];
        c.run.last_outcome = Some((
            Outcome::NeedsHuman { reason: NeedsHuman::AgentFailed { error: "boom".into() } },
            70,
        ));
        assert_eq!(standing(&c).actor, you(YouReason::Failed { error: "boom".into() }, 70));
    }

    #[test]
    fn an_app_failure_names_the_error_not_the_prefix() {
        let mut c = case("active");
        c.run.last_outcome =
            Some((Outcome::Stopped { reason: format!("{FAILED_PREFIX}store locked") }, 80));
        assert_eq!(standing(&c).actor, you(YouReason::Failed { error: "store locked".into() }, 80));
    }

    #[test]
    fn stopped_paused_and_budget_are_you() {
        let mut c = case("active");
        c.run.last_outcome = Some((Outcome::Stopped { reason: "paused".into() }, 1));
        assert_eq!(standing(&c).actor, you(YouReason::Paused, 1));

        c.run.last_outcome = Some((
            Outcome::NeedsHuman { reason: NeedsHuman::NoPlan },
            2,
        ));
        assert!(matches!(standing(&c).actor, NextActor::You { why: YouReason::Stopped { .. }, since: 2 }));

        c.run.last_outcome = Some((
            Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 6 } },
            3,
        ));
        assert_eq!(
            standing(&c).actor,
            you(YouReason::Stopped { reason: "budget spent: 6 sessions".into() }, 3)
        );
    }

    #[test]
    fn an_outcome_that_stopped_for_a_request_is_stale_once_none_is_left() {
        let mut c = case("planning");
        c.run.last_outcome = Some((
            Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending: 1 } },
            9,
        ));
        // Nothing waits on the user any more: the run continues by itself, so
        // the node is simply not started again rather than "stopped".
        assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500));
    }

    #[test]
    fn a_pending_wait_is_others_with_its_kind_and_next_check() {
        let mut c = case("pr");
        c.waits = vec![wait("github:pr:7:review", NOW + 60_000, NOW - 1000)];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert_eq!(wait.kind, WaitKind::HumanReview);
        assert_eq!(wait.next_check_ms, Some(NOW + 60_000));
        assert_eq!(wait.since, 100);
        assert_eq!(wait.detail.as_deref(), Some("a review of the pull request"));
        assert!(!wait.overdue);
    }

    #[test]
    fn wait_kinds_come_from_the_spec() {
        assert_eq!(WaitKind::from_spec("github:pr:7:merged"), WaitKind::Merge);
        assert_eq!(WaitKind::from_spec("github:release:published"), WaitKind::Release);
        assert_eq!(WaitKind::from_spec("github:pr:review"), WaitKind::HumanReview);
        assert_eq!(WaitKind::from_spec(""), WaitKind::Other);
        assert_eq!(WaitKind::from_spec("github:pr 12 checks"), WaitKind::Other);
    }

    #[test]
    fn a_wait_whose_next_check_is_long_past_is_overdue() {
        let interval = 60_000;
        let mut c = case("pr");
        // Looked at 10 minutes ago, due again 9 minutes ago, 1 minute interval.
        c.waits = vec![wait("github:pr:7:review", NOW - 9 * 60_000, NOW - 9 * 60_000 - interval)];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert!(wait.overdue);

        // Due a moment ago is just the poller about to run.
        c.waits = vec![wait_at(NOW - 5_000, NOW - 65_000)];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert!(!wait.overdue);
    }

    fn wait_at(due_at: i64, updated_at: i64) -> Wait {
        wait("github:pr:7:review", due_at, updated_at)
    }

    #[test]
    fn a_run_that_ended_to_wait_with_no_wait_row_still_shows_what_it_waits_on() {
        let mut c = case("pr");
        c.run.last_outcome = Some((
            Outcome::Waiting { what: "a review".into(), due_at_ms: NOW + 5_000 },
            50,
        ));
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert_eq!(wait.kind, WaitKind::Other);
        assert_eq!(wait.detail.as_deref(), Some("a review"));
        assert_eq!(wait.since, 50);
    }

    #[test]
    fn a_deferral_holds_a_you_node_off_the_list() {
        let mut c = case("design");
        c.attention = requests(Uuid::nil(), &[100]);
        c.deferral = Some(Deferral { until: Some(NOW + 1000), deferred_at: 200 });
        let s = standing(&c);
        assert!(matches!(s.actor, NextActor::You { .. }));
        assert!(s.deferred.is_some());
        assert!(!s.on_you());
    }

    #[test]
    fn a_deferral_ends_at_its_time_and_not_before() {
        let mut c = case("design");
        c.attention = requests(Uuid::nil(), &[100]);
        c.deferral = Some(Deferral { until: Some(NOW), deferred_at: 200 });
        assert!(standing(&c).deferred.is_none(), "until == now has expired");
        c.deferral = Some(Deferral { until: None, deferred_at: 200 });
        assert!(standing(&c).deferred.is_some(), "no end holds until the user returns it");
    }

    #[test]
    fn a_new_request_wakes_a_deferred_node_but_the_ones_already_seen_do_not() {
        let mut c = case("design");
        c.deferral = Some(Deferral { until: None, deferred_at: 200 });
        c.attention = requests(Uuid::nil(), &[100, 150]);
        assert!(standing(&c).deferred.is_some());
        c.attention = requests(Uuid::nil(), &[100, 250]);
        let s = standing(&c);
        assert!(s.deferred.is_none());
        assert!(s.on_you());
    }

    #[test]
    fn a_new_failure_wakes_a_deferred_node() {
        let mut c = case("active");
        c.deferral = Some(Deferral { until: None, deferred_at: 200 });
        c.run.last_outcome = Some((Outcome::Stopped { reason: "budget".into() }, 150));
        assert!(standing(&c).deferred.is_some());
        c.run.last_outcome = Some((Outcome::Stopped { reason: format!("{FAILED_PREFIX}x") }, 250));
        assert!(standing(&c).deferred.is_none());
    }

    #[test]
    fn a_node_that_moves_on_to_a_new_state_the_user_must_act_in_wakes_a_deferred_node() {
        // The node entered its state at 500. Deferred after that, it holds;
        // deferred before that (the state is new since), it wakes.
        let mut c = case("proposed");
        c.deferral = Some(Deferral { until: None, deferred_at: 600 });
        assert!(standing(&c).deferred.is_some());
        c.deferral = Some(Deferral { until: None, deferred_at: 400 });
        assert!(standing(&c).deferred.is_none());
    }

    #[test]
    fn only_you_can_be_deferred() {
        let mut c = case("pr");
        c.waits = vec![wait("github:pr:7:review", NOW + 60_000, NOW)];
        c.deferral = Some(Deferral { until: None, deferred_at: 0 });
        let s = standing(&c);
        assert!(matches!(s.actor, NextActor::Others { .. }));
        assert!(s.deferred.is_none());
    }

    struct Fixture {
        dir: std::path::PathBuf,
        conn: Connection,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!("tod-next-actor-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        Fixture { dir, conn }
    }

    fn node(conn: &Connection, slug: &str, lifecycle: Option<&str>) -> Uuid {
        let id = Uuid::new_v4();
        NodeRepo::new(conn).create_with_id(id, slug, slug).unwrap();
        if let Some(state) = lifecycle {
            conn.execute(
                "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, 7)",
                rusqlite::params![tod_store::outline::uuid_blob::uuid_to_blob(id), state],
            )
            .unwrap();
        }
        id
    }

    #[test]
    fn for_nodes_skips_nodes_without_lifecycle_and_reads_the_store() {
        let fx = fixture();
        let plain = node(&fx.conn, "plain", None);
        let fresh = node(&fx.conn, "fresh", Some("proposed"));
        let waiting = node(&fx.conn, "waiting", Some("pr"));
        let finished = node(&fx.conn, "finished", Some("done"));
        WaitRepo::new(&fx.conn)
            .create(waiting, &NewWait::event("github:pr:3:review", NOW + 60_000).with_detail("a review of o/r#3"))
            .unwrap();

        let ids = [plain, fresh, waiting, finished];
        let got = for_nodes(&fx.conn, &ids, &Inputs { now_ms: NOW, ..Default::default() }).unwrap();

        assert!(!got.contains_key(&plain));
        assert_eq!(got[&fresh].actor, you(YouReason::NotStarted, 7));
        assert_eq!(got[&finished].actor, NextActor::Done);
        let NextActor::Others { wait } = &got[&waiting].actor else { panic!("not others") };
        assert_eq!(wait.kind, WaitKind::HumanReview);
        assert_eq!(wait.detail.as_deref(), Some("a review of o/r#3"));
    }

    #[test]
    fn for_nodes_takes_run_facts_and_deferrals_per_node() {
        let fx = fixture();
        let running = node(&fx.conn, "running", Some("active"));
        let parked = node(&fx.conn, "parked", Some("proposed"));
        let mut inputs = Inputs { now_ms: NOW, ..Default::default() };
        inputs.runs.insert(
            running,
            RunFacts { working: Some((Work::IncomingCheck, 11)), ..Default::default() },
        );
        inputs.deferrals.insert(parked, Deferral { until: None, deferred_at: 9 });

        let got = for_nodes(&fx.conn, &[running, parked], &inputs).unwrap();
        assert_eq!(got[&running].actor, NextActor::Working { what: Work::IncomingCheck, since: 11 });
        assert!(got[&parked].deferred.is_some());
        assert!(!got[&parked].on_you());
    }

    #[test]
    fn for_nodes_with_no_ids_reads_nothing() {
        let fx = fixture();
        assert!(for_nodes(&fx.conn, &[], &Inputs::default()).unwrap().is_empty());
    }
}
