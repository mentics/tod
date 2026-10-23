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
use tod_store::activity::{
    ActivityRepo, KIND_GATE_CHECK, KIND_INCOMING_CHECK, KIND_PROTOCOL_PREFIX, KIND_RUN, Lease,
};
use tod_store::conversation::ProtocolKind;
use tod_store::deferrals::DeferralRepo;
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
    /// The node's autopilot run: deciding, checking gates, and the turns it
    /// drives.
    Runner,
    /// Processing whose lease kind this version does not know. Still work:
    /// the lease proves it, so it is shown rather than dropped.
    Other,
}

impl Work {
    /// What a lease `kind` (`tod_store::activity`) is.
    pub fn from_kind(kind: &str) -> Self {
        if let Some(protocol) = kind.strip_prefix(KIND_PROTOCOL_PREFIX) {
            return ProtocolKind::parse(protocol).map(Self::Conversation).unwrap_or(Self::Other);
        }
        match kind {
            KIND_RUN => Self::Runner,
            KIND_GATE_CHECK => Self::GateCheck,
            KIND_INCOMING_CHECK => Self::IncomingCheck,
            _ => Self::Other,
        }
    }

    /// The lease kind a conversation turn under `protocol` holds.
    pub fn conversation_kind(protocol: ProtocolKind) -> String {
        format!("{KIND_PROTOCOL_PREFIX}{}", protocol.as_str())
    }
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
    /// The last look failed and another is due: what failed, and how many
    /// times. Only while retrying; once the retries are spent the node is
    /// `You::Failed` with the error instead.
    pub retry: Option<Retry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub attempt: u32,
    pub max: u32,
    pub error: String,
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
    /// The node's activity leases (`tod_store::activity`): the only proof
    /// that anything is processing it.
    pub leases: &'a [Lease],
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
    let mut leases: HashMap<Uuid, Vec<Lease>> = HashMap::new();
    for lease in ActivityRepo::new(conn).list_for_nodes(node_ids)? {
        leases.entry(lease.node_id).or_default().push(lease);
    }
    // The stored deferrals, over which the caller's own (tests, previews) win.
    let mut deferrals: HashMap<Uuid, Deferral> = HashMap::new();
    for row in DeferralRepo::new(conn).list_for_nodes(node_ids)? {
        deferrals.insert(row.node_id, Deferral { until: row.until, deferred_at: row.deferred_at });
    }
    deferrals.extend(inputs.deferrals.iter().map(|(id, d)| (*id, *d)));
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
            leases: leases.get(&node_id).map(Vec::as_slice).unwrap_or(&[]),
            run: inputs.runs.get(&node_id).unwrap_or(&no_run),
            deferral: deferrals.get(&node_id).copied(),
            now_ms: inputs.now_ms,
        });
        out.insert(node_id, standing);
    }
    Ok(out)
}

/// Every node's standing, as the app and the daemon read it: the store's
/// facts plus each node's saved run (`autopilot/<node>.json`). Blocking
/// (store and file reads): never call it on the UI thread.
///
/// A saved run that cannot be read is not skipped: the node shows it as a
/// failure, with the reason.
pub fn compute(fleet: &tod_store::fleet::FleetStore, data_root: &std::path::Path) -> Result<HashMap<Uuid, NodeStanding>> {
    use crate::autopilot::{AutopilotState, state_path};
    let now_ms = tod_store::outline::uuid_blob::now_ms();
    fleet.read(|conn| {
        let ids: Vec<Uuid> = NodeRepo::new(conn).list_all()?.into_iter().map(|n| n.id).collect();
        let with_lifecycle = NodeRepo::new(conn).get_lifecycle_for_nodes(&ids)?;
        let mut inputs = Inputs { now_ms, ..Default::default() };
        for node in with_lifecycle.keys() {
            let path = state_path(data_root, *node);
            if !path.exists() {
                continue;
            }
            let at = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(now_ms, |d| d.as_millis() as i64);
            let outcome = match AutopilotState::load(data_root, *node) {
                Ok(state) => state.outcome,
                Err(err) => Some(Outcome::Stopped { reason: format!("{FAILED_PREFIX}{err:#}") }),
            };
            if let Some(outcome) = outcome {
                inputs.runs.insert(*node, RunFacts { last_outcome: Some((outcome, at)), ..Default::default() });
            }
        }
        for_nodes(conn, &ids, &inputs)
    })
}

/// The nodes whose stored deferral is spent and should be cleared: it
/// expired, or something new reached the user (the node is `You` and no
/// longer deferred), or the node no longer has a standing. Without this a
/// node that woke would fall back under its old deferral the next time it
/// needs the user.
pub fn spent_deferrals(
    rows: &[tod_store::deferrals::DeferralRow],
    standings: &HashMap<Uuid, NodeStanding>,
    now_ms: i64,
) -> Vec<Uuid> {
    rows.iter()
        .filter(|row| {
            let expired = row.until.is_some_and(|until| until <= now_ms);
            expired
                || match standings.get(&row.node_id) {
                    None => true,
                    Some(standing) => {
                        matches!(standing.actor, NextActor::You { .. }) && standing.deferred.is_none()
                    }
                }
        })
        .map(|row| row.node_id)
        .collect()
}

/// A name for this process in a lease's `owner` (`tod`, `tod-agentd`, …), so
/// a lost lease says who held it.
pub fn this_process() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "tod".to_string());
    format!("{exe}:{}", std::process::id())
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
    // Working is only ever what a live lease says. Lifecycle leases only: a
    // chat turn is the chat decorator's.
    let lifecycle_leases = || facts.leases.iter().filter(|l| l.is_lifecycle());
    if let Some(lease) = lifecycle_leases().find(|l| l.is_live(facts.now_ms)) {
        return (
            NextActor::Working {
                what: Work::from_kind(&lease.kind),
                since: lease.started_at,
            },
            0,
        );
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
    // Processing that stopped answering: its owner died or hung. Said, not
    // left to look like nothing is happening.
    if let Some(lease) = lifecycle_leases().find(|l| !l.is_live(facts.now_ms)) {
        let error = format!(
            "processing was lost: {} held by {} stopped responding {} ago",
            lease.kind,
            lease.owner,
            format_elapsed(facts.now_ms - lease.heartbeat_at),
        );
        return (
            NextActor::You {
                why: YouReason::Failed { error },
                since: lease.heartbeat_at,
            },
            lease.heartbeat_at,
        );
    }
    // A look at the outside that failed as often as it may (or in a way
    // asking again cannot fix) is the user's, with the error.
    if let Some(wait) = facts.waits.iter().find(|w| w.exhausted()) {
        return (
            NextActor::You {
                why: YouReason::Failed {
                    error: wait.last_error.clone().unwrap_or_default(),
                },
                since: wait.updated_at,
            },
            wait.updated_at,
        );
    }
    // A failing look shows before the waits that are fine: it is the news.
    if let Some(wait) = facts
        .waits
        .iter()
        .find(|w| w.last_error.is_some())
        .or(facts.waits.first())
    {
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
                    retry: None,
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
        retry: wait.last_error.as_ref().map(|error| Retry {
            attempt: wait.attempt,
            max: wait.max_attempts.unwrap_or(tod_store::waits::DEFAULT_MAX_ATTEMPTS),
            error: error.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::{AttentionItem, AttentionKind, RequestReason};
    use tod_store::fleet::schema;
    use tod_store::outline::repos::NodeRepo;
    use tod_store::activity::KIND_CHAT;
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
            attempt: 0,
            max_attempts: None,
            last_error: None,
        }
    }

    /// A lease whose last heartbeat was `ago_ms` before `NOW`.
    fn lease(kind: &str, started_at: i64, ago_ms: i64) -> Lease {
        Lease {
            id: Uuid::new_v4(),
            node_id: Uuid::nil(),
            kind: kind.into(),
            owner: "daemon-1".into(),
            started_at,
            heartbeat_at: NOW - ago_ms,
        }
    }

    fn live_lease(kind: &str, started_at: i64) -> Lease {
        lease(kind, started_at, 1_000)
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
        leases: Vec<Lease>,
        run: RunFacts,
        deferral: Option<Deferral>,
    }

    fn case(lifecycle: &'static str) -> Case {
        Case {
            lifecycle,
            attention: NodeAttention::default(),
            waits: Vec::new(),
            leases: Vec::new(),
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
            leases: &c.leases,
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
        c.leases = vec![live_lease(KIND_GATE_CHECK, 20)];
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
        let kind = Work::conversation_kind(ProtocolKind::Implementation);
        c.leases = vec![live_lease(&kind, 30)];
        assert_eq!(
            standing(&c).actor,
            NextActor::Working { what: Work::Conversation(ProtocolKind::Implementation), since: 30 }
        );
    }

    #[test]
    fn lease_kinds_round_trip_and_an_unknown_one_is_still_work() {
        for protocol in [ProtocolKind::Implementation, ProtocolKind::Review, ProtocolKind::Pr] {
            assert_eq!(
                Work::from_kind(&Work::conversation_kind(protocol)),
                Work::Conversation(protocol)
            );
        }
        assert_eq!(Work::from_kind(KIND_GATE_CHECK), Work::GateCheck);
        assert_eq!(Work::from_kind(KIND_INCOMING_CHECK), Work::IncomingCheck);
        assert_eq!(Work::from_kind("something-new"), Work::Other);
        assert_eq!(Work::from_kind("protocol:nonsense"), Work::Other);
    }

    #[test]
    fn a_lease_that_stopped_beating_is_a_loss_not_working_and_not_idle() {
        let mut c = case("active");
        c.leases = vec![lease("protocol:implementation", 30, 60_000)];
        let s = standing(&c);
        let NextActor::You { why: YouReason::Failed { error }, since } = s.actor else {
            panic!("a lost lease must be the user's: {:?}", s.actor)
        };
        assert!(error.contains("processing was lost"), "{error}");
        assert!(error.contains("protocol:implementation"), "{error}");
        assert!(error.contains("daemon-1"), "{error}");
        assert_eq!(since, NOW - 60_000);
    }

    #[test]
    fn a_live_lease_wins_over_a_lost_one_on_the_same_node() {
        let mut c = case("active");
        c.leases = vec![lease(KIND_GATE_CHECK, 1, 60_000), live_lease(KIND_INCOMING_CHECK, 2)];
        assert!(matches!(standing(&c).actor, NextActor::Working { what: Work::IncomingCheck, .. }));
    }

    #[test]
    fn a_chat_lease_never_makes_the_node_work_or_lost() {
        let mut c = case("active");
        c.leases = vec![live_lease(KIND_CHAT, 5), lease(KIND_CHAT, 1, 60_000)];
        assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500));
    }

    #[test]
    fn a_wait_that_failed_and_will_retry_is_others_with_the_error() {
        let mut c = case("pr");
        let mut w = wait("github:pr:read", NOW + 30_000, NOW);
        w.attempt = 2;
        w.max_attempts = Some(5);
        w.last_error = Some("GitHub is unavailable (HTTP 503)".into());
        c.waits = vec![w];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert_eq!(
            wait.retry,
            Some(Retry { attempt: 2, max: 5, error: "GitHub is unavailable (HTTP 503)".into() })
        );
    }

    #[test]
    fn a_wait_whose_retries_are_spent_is_the_users_with_the_error() {
        let mut c = case("pr");
        let mut w = wait("github:pr:read", NOW + 30_000, NOW - 5);
        w.attempt = 5;
        w.max_attempts = Some(5);
        w.last_error = Some("GitHub rejected the token (HTTP 401)".into());
        c.waits = vec![w];
        assert_eq!(
            standing(&c).actor,
            you(YouReason::Failed { error: "GitHub rejected the token (HTTP 401)".into() }, NOW - 5)
        );
    }

    #[test]
    fn a_pending_permission_beats_working() {
        let mut c = case("active");
        c.leases = vec![live_lease(&Work::conversation_kind(ProtocolKind::Implementation), 30)];
        c.run.permission = Some(("Run `rm`".into(), 40));
        assert_eq!(standing(&c).actor, you(YouReason::Permission { title: "Run `rm`".into() }, 40));
    }

    #[test]
    fn working_beats_open_requests_and_a_stale_outcome() {
        let mut c = case("review");
        c.attention = requests(Uuid::nil(), &[10]);
        c.run.last_outcome = Some((Outcome::Stopped { reason: "budget".into() }, 5));
        c.leases = vec![live_lease(KIND_GATE_CHECK, 60)];
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
    fn for_nodes_reads_leases_and_takes_deferrals_per_node() {
        let fx = fixture();
        let running = node(&fx.conn, "running", Some("active"));
        let parked = node(&fx.conn, "parked", Some("proposed"));
        let mut inputs = Inputs { now_ms: NOW, ..Default::default() };
        ActivityRepo::new(&fx.conn)
            .start(Uuid::new_v4(), running, KIND_INCOMING_CHECK, "app-1", NOW - 500)
            .unwrap();
        inputs.deferrals.insert(parked, Deferral { until: None, deferred_at: 9 });

        let got = for_nodes(&fx.conn, &[running, parked], &inputs).unwrap();
        assert_eq!(got[&running].actor, NextActor::Working { what: Work::IncomingCheck, since: NOW - 500 });
        assert!(got[&parked].deferred.is_some());
        assert!(!got[&parked].on_you());
    }

    #[test]
    fn a_held_guard_makes_the_node_working_and_dropping_it_ends_that() {
        use tod_store::fleet::FleetStore;
        use tod_store::outline::OutlineMutation;

        let root = std::env::temp_dir().join(format!("tod-next-actor-guard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = std::sync::Arc::new(FleetStore::open(&root).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node_id = fleet.flatten_outline(list_id).unwrap()[0].node.id;
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![tod_store::outline::Capability::Lifecycle],
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        let now = || tod_store::outline::uuid_blob::now_ms();
        let actor = |fleet: &FleetStore| {
            fleet.reload_if_stale().unwrap();
            let inputs = Inputs { now_ms: now(), ..Default::default() };
            fleet
                .read(|conn| for_nodes(conn, &[node_id], &inputs))
                .unwrap()
                .remove(&node_id)
                .expect("the node has Lifecycle")
                .actor
        };

        assert_eq!(actor(&fleet), NextActor::You { why: YouReason::NotStarted, since: actor_since(&actor(&fleet)) });
        let guard = tod_store::activity::ActivityGuard::start(&fleet, node_id, KIND_RUN, "test:1").unwrap();
        fleet.writer().flush().unwrap();
        assert!(matches!(actor(&fleet), NextActor::Working { what: Work::Runner, .. }));
        drop(guard);
        fleet.writer().flush().unwrap();
        assert!(matches!(actor(&fleet), NextActor::You { why: YouReason::NotStarted, .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn actor_since(actor: &NextActor) -> i64 {
        match actor {
            NextActor::You { since, .. } => *since,
            other => panic!("expected You, got {other:?}"),
        }
    }

    #[test]
    fn for_nodes_reads_stored_deferrals_and_spent_ones_are_found() {
        let fx = fixture();
        let held = node(&fx.conn, "held", Some("proposed"));
        let woken = node(&fx.conn, "woken", Some("proposed"));
        let expired = node(&fx.conn, "expired", Some("proposed"));
        let gone = node(&fx.conn, "gone", None);
        let repo = DeferralRepo::new(&fx.conn);
        repo.set(held, Some(NOW + 60_000), 10).unwrap(); // entered at 7: holds
        repo.set(woken, None, 3).unwrap(); // entered at 7 > 3: woken
        repo.set(expired, Some(NOW - 1), 10).unwrap();
        repo.set(gone, None, 1).unwrap();

        let ids = [held, woken, expired, gone];
        let got = for_nodes(&fx.conn, &ids, &Inputs { now_ms: NOW, ..Default::default() }).unwrap();
        assert!(got[&held].deferred.is_some());
        assert!(got[&woken].deferred.is_none());
        assert!(got[&expired].deferred.is_none());

        let rows = repo.list_all().unwrap();
        let mut spent = spent_deferrals(&rows, &got, NOW);
        spent.sort();
        let mut want = vec![woken, expired, gone];
        want.sort();
        assert_eq!(spent, want, "the held one stays");
    }

    #[test]
    fn for_nodes_with_no_ids_reads_nothing() {
        let fx = fixture();
        assert!(for_nodes(&fx.conn, &[], &Inputs::default()).unwrap().is_empty());
    }
}
