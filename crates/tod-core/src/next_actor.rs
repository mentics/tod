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
//! 5. The last run failed, stopped, or was paused → `You`. For a node in the
//!    cloud the last run is what its supervisor left in its idle heartbeat
//!    (`cloud_activity.stopped`), the answer "leave it stopped" to its latest
//!    stop question, or a sandbox that is gone.
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
    ActivityRepo, KIND_CLOUD_REPAIR, KIND_CLOUD_START, KIND_GATE_CHECK, KIND_INCOMING_CHECK, KIND_PROTOCOL_PREFIX, KIND_RUN, Lease,
};
use tod_store::cloud_activity::CloudBeat;
use tod_store::run_state::{Phase, RunState};
use tod_store::conversation::ProtocolKind;
use tod_store::deferrals::DeferralRepo;
use tod_store::outline::repos::NodeRepo;
use tod_store::node_problems::{NodeProblem, NodeProblemRepo};
use tod_store::waits::{GiveUp, Wait, WaitRepo};
use uuid::Uuid;

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
    /// The node's supervisor, in its cloud sandbox, is running the autopilot:
    /// proved by its synced heartbeat (`tod_store::cloud_activity`).
    Cloud,
    /// The app is sending the node to the cloud (making or waking its
    /// sandbox, syncing) for the user, and waiting to hear its supervisor
    /// begin. Set the moment the user clicks, so the node never reads as
    /// waiting on them while their own request is being carried out.
    CloudStart,
    /// The app is replacing the node's lost cloud sandbox, or reinstalling
    /// one left on an older build, so its supervisor can run again.
    CloudRepair,
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
            KIND_CLOUD_START => Self::CloudStart,
            KIND_CLOUD_REPAIR => Self::CloudRepair,
            KIND_GATE_CHECK => Self::GateCheck,
            KIND_INCOMING_CHECK => Self::IncomingCheck,
            _ => Self::Other,
        }
    }

    /// The lease kind a conversation turn under `protocol` holds.
    ///
    /// The plain chats (the outline chat, the chat panel, visual design) are
    /// the user's own conversations, not the node's lifecycle processing:
    /// their lease is a `chat` one, which feeds the tree's chat decorator and
    /// never the node's actor.
    pub fn conversation_kind(protocol: ProtocolKind) -> String {
        match protocol {
            ProtocolKind::Outline | ProtocolKind::Chat | ProtocolKind::VisualDesign => {
                tod_store::activity::KIND_CHAT.to_string()
            }
            _ => format!("{KIND_PROTOCOL_PREFIX}{}", protocol.as_str()),
        }
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
    /// Always false now: a wait well past due is the user's
    /// (`NextActor::You`, by the one `Wait::expired_by` check), never
    /// `Others`. Kept for the views that read it.
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
    /// A review bot's review of the current head; `who` is the bot.
    BotReview,
    /// The head's checks to finish.
    Checks,
    /// The merge.
    Merge,
    /// A release that names the pull request.
    Release,
    /// Any other pending wait (a time, a command, an event we do not name).
    Other,
}

/// The spec of the wait for a person's review of pull request `number`:
/// `github:pr:<n>:review`, or `github:pr:<n>:review:<login,login>` when
/// GitHub says whom it asked (teams as `org/team`). Without a number (the
/// pull request is not known) `github:pr:review`.
pub fn review_spec(number: Option<i64>, reviewers: &[String]) -> String {
    let mut spec = match number {
        Some(n) => format!("github:pr:{n}:review"),
        None => "github:pr:review".to_string(),
    };
    if !reviewers.is_empty() {
        spec.push(':');
        spec.push_str(&reviewers.join(","));
    }
    spec
}

/// Whether `spec` is a wait for a person's review, in either form of
/// [`review_spec`].
pub fn is_review_spec(spec: &str) -> bool {
    spec.starts_with("github:pr:") && (spec.ends_with(":review") || spec.contains(":review:"))
}

/// The reviewers a [`review_spec`] names, comma separated; `None` for the
/// form that names nobody (and for a spec that is not a review's).
pub fn review_spec_reviewers(spec: &str) -> Option<&str> {
    if !is_review_spec(spec) {
        return None;
    }
    spec.split_once(":review:").map(|(_, who)| who).filter(|who| !who.is_empty())
}

impl WaitKind {
    /// The kind of an `event` wait from its `match_spec`
    /// (`autopilot::github_wait`, `autopilot::babysit`).
    pub fn from_spec(spec: &str) -> Self {
        // Agents write `github:pr 2810 checks`; the supervisor writes colons.
        let spec = &tod_store::waits::normalize_spec(spec);
        if spec.starts_with("github:release:") {
            Self::Release
        } else if spec.starts_with("github:pr:") && spec.contains(":bot-review:") {
            Self::BotReview
        } else if is_review_spec(spec) {
            // Before checks and merge: a reviewer's name ends the spec.
            Self::HumanReview
        } else if spec.starts_with("github:pr:") && spec.ends_with(":checks") {
            Self::Checks
        } else if spec.starts_with("github:pr:") && spec.ends_with(":merged") {
            Self::Merge
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

/// A cloud node whose sandbox is gone (`cloud_nodes.lost_at`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudLost {
    /// When the orchestrator found it gone (ms).
    pub since: i64,
    /// The user deleted it (this machine's `deleted_by_user`).
    pub deleted_by_user: bool,
}

/// The user's answer to a cloud node's latest stop question, when it is a
/// "leave it" one (`crate::stop_questions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAnswer {
    /// The question's kind (`stop_questions::KINDS`).
    pub kind: String,
    /// When it was answered (ms).
    pub at: i64,
}

/// What [`for_nodes`] needs besides the store.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// Cloud nodes whose sandbox the user deleted: that is a file on this
    /// machine (`cloud_sync::sandbox_deleted_by_user`), not the store.
    pub deleted_sandboxes: std::collections::HashSet<Uuid>,
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
    /// The node's supervisor's heartbeat from the cloud, when it has one.
    pub cloud: Option<&'a CloudBeat>,
    /// The node's cloud sandbox is gone.
    pub cloud_lost: Option<CloudLost>,
    /// What the node's runner last said it was doing (`tod_store::run_state`),
    /// when it said anything: a supervisor or app built before the table
    /// writes none, and the heartbeat and saved run speak instead.
    pub run_state: Option<&'a RunState>,
    /// The node's latest stop question was answered "leave it".
    pub stop_answer: Option<&'a StopAnswer>,
    pub run: &'a RunFacts,
    /// The node's recorded background problems (`tod_store::node_problems`).
    pub problems: &'a [NodeProblem],
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
    let run_states: HashMap<Uuid, RunState> =
        tod_store::run_state::list_for_nodes(conn, node_ids)?.into_iter().map(|r| (r.node_id, r)).collect();
    let mut cloud: HashMap<Uuid, CloudBeat> = HashMap::new();
    for beat in tod_store::cloud_activity::list_for_nodes(conn, node_ids)? {
        cloud.insert(beat.node_id, beat);
    }
    // Cloud nodes: a sandbox that is gone, and the answer to the latest stop
    // question (only for a node whose sandbox is there to be stopped).
    let mut lost: HashMap<Uuid, CloudLost> = HashMap::new();
    let mut stop_answers: HashMap<Uuid, StopAnswer> = HashMap::new();
    for row in tod_store::cloud_nodes::list(conn)?.into_iter().filter(|r| node_ids.contains(&r.node_id)) {
        if let Some(since) = row.lost_at {
            lost.insert(row.node_id, CloudLost { since, deleted_by_user: inputs.deleted_sandboxes.contains(&row.node_id) });
        } else if let Some(latest) = crate::stop_questions::latest(conn, row.node_id)?
            && latest.answer == crate::stop_questions::Answer::Stop
        {
            stop_answers.insert(
                row.node_id,
                StopAnswer { kind: latest.kind, at: latest.answered_at.unwrap_or(row.accepted_at) },
            );
        }
    }
    // The stored deferrals, over which the caller's own (tests, previews) win.
    let mut deferrals: HashMap<Uuid, Deferral> = HashMap::new();
    for row in DeferralRepo::new(conn).list_for_nodes(node_ids)? {
        deferrals.insert(row.node_id, Deferral { until: row.until, deferred_at: row.deferred_at });
    }
    deferrals.extend(inputs.deferrals.iter().map(|(id, d)| (*id, *d)));
    let mut problems: HashMap<Uuid, Vec<NodeProblem>> = HashMap::new();
    for problem in NodeProblemRepo::new(conn).list_for_nodes(node_ids)? {
        problems.entry(problem.node_id).or_default().push(problem);
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
            leases: leases.get(&node_id).map(Vec::as_slice).unwrap_or(&[]),
            cloud: cloud.get(&node_id),
            cloud_lost: lost.get(&node_id).copied(),
            run_state: run_states.get(&node_id),
            stop_answer: stop_answers.get(&node_id),
            run: inputs.runs.get(&node_id).unwrap_or(&no_run),
            problems: problems.get(&node_id).map(Vec::as_slice).unwrap_or(&[]),
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
        inputs.deleted_sandboxes = crate::cloud_sync::CloudSyncState::load(data_root)?
            .deleted_by_user
            .iter()
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect();
        // A node whose supervisor runs in the cloud has no run of this
        // machine's to speak of: the file here is the last local run's, from
        // before the node moved (a failed start, say), and would read as the
        // node's failure for as long as it stays.
        let in_cloud: std::collections::HashSet<Uuid> = tod_store::cloud_nodes::list(conn)?
            .into_iter()
            .filter(|row| row.lost_at.is_none())
            .map(|row| row.node_id)
            .collect();
        for node in with_lifecycle.keys().filter(|n| !in_cloud.contains(n)) {
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

/// How long a `starting` row holds the node as working before it is a failure:
/// a sandbox to boot, a copy to seed, an autopilot to start
/// (`cloud_sync::SUPERVISOR_REPORTS_WITHIN`).
const START_REPORTS_WITHIN_MS: i64 = 150_000;
/// How much later than a row another word may be dated and still be the same
/// run's end (the supervisor writes the row, then its idle heartbeat).
const RUN_STATE_SKEW_MS: i64 = 5_000;

/// A `done` node is `Done`, except while its sandbox could not be deleted
/// (`tod_core::sandbox_retire`; the problem is on the node,
/// `tod_store::node_problems::KIND_SANDBOX_RETIRE`). That leaves a running
/// sandbox, which costs money and holds the node's work, so it must show
/// without opening a log; but only the part that is the user's to fix is on
/// the user's list:
///
/// - **While the failure looks temporary** (`problem.transient`: Blaxel not
///   ready, a timeout; the daemon's next pass, every minute, tries again) the
///   node is `Others`: nothing for the user to do, the error is shown in the
///   wait's detail, and it does not wake the `You` list for each failed pass.
/// - **When it is permanent** (nothing retries it) it is `You::Failed` with the
///   error, once: the record does not change again until something does (the
///   user deletes the sandbox, the setting or location changes, the daemon
///   restarts), so `newest` is fixed and a deferral holds (the user can set it
///   aside); Delete sandbox in the task panel's menu is the action, and it
///   clears the record.
///
/// Every other `done` node, and every other kind of problem, is `Done`: those
/// are shown by the task panel only, and never count as someone's turn.
fn done_actor(facts: &Facts<'_>) -> (NextActor, i64) {
    use tod_store::node_problems::KIND_SANDBOX_RETIRE;
    let Some(problem) = facts.problems.iter().find(|p| p.kind == KIND_SANDBOX_RETIRE) else {
        return (NextActor::Done, 0);
    };
    if !problem.transient {
        return (
            NextActor::You { why: YouReason::Failed { error: problem.message.clone() }, since: problem.first_at },
            problem.updated_at,
        );
    }
    let wait = ExternalWait {
        kind: WaitKind::Other,
        who: None,
        detail: Some(format!("deleting its sandbox failed; the daemon tries again: {}", problem.message)),
        since: problem.first_at,
        next_check_ms: None,
        overdue: false,
        retry: None,
    };
    (NextActor::Others { wait }, 0)
}

/// The actor, and when the newest thing reaching the user arrived (for the
/// deferral's wake rule).
fn actor_for(facts: &Facts<'_>) -> (NextActor, i64) {
    if facts.lifecycle == DONE_STATE {
        return done_actor(facts);
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
    if let Some(beat) = facts.cloud.filter(|b| b.is_live(facts.now_ms)) {
        return (NextActor::Working { what: Work::Cloud, since: beat.started_at }, 0);
    }
    let requests = facts.attention;
    if let Some(waiting_since) = requests.waiting_since {
        let newest = requests.items.iter().map(|i| i.since).max().unwrap_or(waiting_since);
        // Only permissions the user has not granted: that is a different
        // thing to do than answering a question, so it is its own reason.
        // Mixed with anything else it is one more request.
        let why = match requests.items.as_slice() {
            [first, rest @ ..]
                if first.permission
                    && first.reason == crate::attention::RequestReason::Access
                    && rest.iter().all(|i| i.permission && i.reason == crate::attention::RequestReason::Access) =>
            {
                YouReason::Permission { title: first.summary.clone() }
            }
            _ => YouReason::Request { count: requests.count },
        };
        return (
            NextActor::You {
                why,
                since: waiting_since,
            },
            newest,
        );
    }
    // The runner's own word (`tod_store::run_state`). Only a heartbeat or a
    // saved run newer than the row can supersede it: a supervisor built
    // before the table writes no row, and its beat is then the later word.
    let run_state = facts.run_state;
    // The app sent the node to the cloud and nothing has reported in yet:
    // starting, for as long as a supervisor may take to boot and seed, then
    // a failure that says so (never the silence of "not started").
    if let Some(row) = run_state.filter(|r| r.phase == Phase::Starting)
        && facts.cloud.is_none_or(|b| b.heartbeat_at <= row.updated_at)
    {
        let waited = facts.now_ms - row.updated_at;
        if waited <= START_REPORTS_WITHIN_MS {
            return (NextActor::Working { what: Work::CloudStart, since: row.since }, 0);
        }
        let error = format!(
            "the cloud supervisor has not reported in {} after the node was sent ({}); \
             Start sends it again",
            format_elapsed(waited),
            row.reason_text,
        );
        return (NextActor::You { why: YouReason::Failed { error }, since: row.since }, row.since);
    }
    // A node in the cloud: its sandbox gone, a stop question the user
    // answered "leave it", or a wake that ended needing someone (what the
    // supervisor left in its idle heartbeat). Each says why and what to do;
    // none is "not started".
    if let Some(lost) = facts.cloud_lost {
        let reason = if lost.deleted_by_user {
            "its sandbox was deleted; Start makes a new one".to_string()
        } else {
            "its sandbox is gone; a new one is made at the next sync, or Start makes one now".to_string()
        };
        return (NextActor::You { why: YouReason::Stopped { reason }, since: lost.since }, lost.since);
    }
    if let Some(answer) = facts.stop_answer {
        let reason = format!("stopped: {}", crate::stop_questions::left_stopped_reason(&answer.kind));
        return (NextActor::You { why: YouReason::Stopped { reason }, since: answer.at }, answer.at);
    }
    // The run stopped and said why, as a row. Skipped when a later wake (an
    // old supervisor's beat) or a later saved run speaks after it.
    if let Some(row) = run_state
        && let Some(why) = crate::run_state::you_reason(row)
        && facts.cloud.is_none_or(|b| b.heartbeat_at <= row.updated_at + RUN_STATE_SKEW_MS)
        && run.last_outcome.as_ref().is_none_or(|(_, at)| *at <= row.updated_at + RUN_STATE_SKEW_MS)
    {
        return (NextActor::You { why, since: row.since }, row.since);
    }
    if let Some(beat) = facts.cloud.filter(|b| b.is_idle())
        && let Some(reason) = &beat.stopped
    {
        return (
            NextActor::You { why: YouReason::Stopped { reason: reason.clone() }, since: beat.heartbeat_at },
            beat.heartbeat_at,
        );
    }
    // A row written at or after the saved run is the later word: when it did
    // not stop the node (above), the saved run's old stop no longer holds.
    if let Some((outcome, at)) = &run.last_outcome
        && !run_state.is_some_and(|row| row.updated_at + RUN_STATE_SKEW_MS >= *at)
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
    // The same for a supervisor in the cloud that stopped beating without
    // ending its run: its sandbox or process died mid-run.
    if let Some(beat) = facts.cloud.filter(|b| !b.is_idle() && !b.is_live(facts.now_ms)) {
        let error = format!(
            "the cloud supervisor stopped responding {} ago (last heard while {})",
            format_elapsed(facts.now_ms - beat.heartbeat_at),
            if beat.what.is_empty() { "working" } else { &beat.what },
        );
        return (NextActor::You { why: YouReason::Failed { error }, since: beat.heartbeat_at }, beat.heartbeat_at);
    }
    // A wait recorded in a stay the node has left no longer holds anything
    // (the move that ended it may not have reached this copy of the
    // database): it is ignored here and ended by the sweep.
    let waits: Vec<&Wait> = facts
        .waits
        .iter()
        .filter(|w| {
            let ends = GiveUp::for_kind(&w.kind).is_some_and(GiveUp::ends_with_stay);
            !(ends && w.stay.is_some_and(|stay| stay != facts.entered_at))
        })
        .collect();
    // A look at the outside that failed as often as it may (or in a way
    // asking again cannot fix) is the user's, with the error.
    if let Some(wait) = waits.iter().copied().find(|w| w.exhausted()) {
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
    // A wait long past its time that nothing is looking at is the user's:
    // the wake that should have served it is not coming.
    if let Some((wait, overdue_ms)) = waits
        .iter()
        .copied()
        .filter_map(|w| w.expired_by(facts.now_ms, facts.cloud.is_some()).map(|late| (w, late)))
        .max_by_key(|(_, late)| *late)
    {
        let on = wait.detail.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| wait.match_spec.clone());
        return (
            NextActor::You {
                why: YouReason::Stopped {
                    reason: format!(
                        "its wait on {on} was due {} ago and nothing is looking at it",
                        format_elapsed(overdue_ms)
                    ),
                },
                since: wait.due_at,
            },
            wait.due_at,
        );
    }
    // A failing look shows before the waits that are fine: it is the news.
    if let Some(wait) = waits.iter().copied().find(|w| w.last_error.is_some()).or(waits.first().copied()) {
        return (NextActor::Others { wait: external_wait(wait) }, 0);
    }
    // A run that ended to wait, with no wait row left to say so, is still a
    // wait on something outside, and says so.
    if let Some((Outcome::Waiting { what, due_at_ms }, at)) = &run.last_outcome {
        // The time it was to be looked at again has long passed: the same
        // fault as a wait row past due, by the same check.
        let as_wait = Wait {
            id: Uuid::nil(),
            node_id: facts.node_id,
            kind: tod_store::waits::KIND_EVENT.into(),
            match_spec: String::new(),
            every_secs: None,
            due_at: *due_at_ms,
            state: tod_store::waits::WAIT_PENDING.into(),
            created_at: *at,
            updated_at: *at,
            detail: None,
            attempt: 0,
            max_attempts: None,
            last_error: None,
            stay: None,
        };
        if let Some(late) = as_wait.expired_by(facts.now_ms, facts.cloud.is_some()) {
            return (
                NextActor::You {
                    why: YouReason::Stopped {
                        reason: format!(
                            "its wait on {what} was due {} ago and nothing is looking at it",
                            format_elapsed(late)
                        ),
                    },
                    since: *due_at_ms,
                },
                *due_at_ms,
            );
        }
        return (
            NextActor::Others {
                wait: ExternalWait {
                    kind: WaitKind::Other,
                    who: None,
                    detail: Some(what.clone()),
                    since: *at,
                    next_check_ms: Some(*due_at_ms),
                    overdue: false,
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
pub(crate) fn outcome_reason(outcome: &Outcome) -> Option<YouReason> {
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
                BudgetLimit::Sessions { used, .. } => format!("budget spent: {used} sessions"),
                BudgetLimit::Time { elapsed_secs, .. } => {
                    format!("budget spent: {} of work", format_elapsed(*elapsed_secs as i64 * 1000))
                }
            },
        }),
    }
}

/// An `Others` wait. A wait past due is never shown this way: that is the
/// user's (`Wait::expired_by`), so `overdue` is only ever false here.
fn external_wait(wait: &Wait) -> ExternalWait {
    ExternalWait {
        kind: WaitKind::from_spec(&wait.match_spec),
        who: wait
            .match_spec
            .split_once(":bot-review:")
            .map(|(_, bot)| bot)
            .or_else(|| review_spec_reviewers(&wait.match_spec))
            .filter(|who| !who.is_empty())
            .map(str::to_string),
        detail: wait.detail.clone().filter(|d| !d.trim().is_empty()),
        since: wait.created_at,
        next_check_ms: Some(wait.due_at),
        overdue: false,
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
            stay: None,
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
                permission: false,
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
        problems: Vec<NodeProblem>,
        run: RunFacts,
        deferral: Option<Deferral>,
        cloud: Option<CloudBeat>,
        cloud_lost: Option<CloudLost>,
        stop_answer: Option<StopAnswer>,
        run_state: Option<RunState>,
    }

    fn case(lifecycle: &'static str) -> Case {
        Case {
            lifecycle,
            attention: NodeAttention::default(),
            waits: Vec::new(),
            leases: Vec::new(),
            problems: Vec::new(),
            cloud: None,
            cloud_lost: None,
            stop_answer: None,
            run_state: None,
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
            cloud: c.cloud.as_ref(),
            cloud_lost: c.cloud_lost,
            run_state: c.run_state.as_ref(),
            stop_answer: c.stop_answer.as_ref(),
            run: &c.run,
            problems: &c.problems,
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

    fn retire_problem(transient: bool, updated_at: i64) -> NodeProblem {
        NodeProblem {
            node_id: Uuid::nil(),
            kind: tod_store::node_problems::KIND_SANDBOX_RETIRE.to_string(),
            message: "Deleting the sandbox s failed: blaxel said no".to_string(),
            transient,
            first_at: 100,
            updated_at,
        }
    }

    #[test]
    fn a_done_node_whose_sandbox_delete_is_transient_is_others_with_the_error() {
        let mut c = case("done");
        c.problems = vec![retire_problem(true, 300)];
        let s = standing(&c);
        let NextActor::Others { wait } = &s.actor else { panic!("{:?}", s.actor) };
        assert!(wait.detail.as_deref().unwrap().contains("blaxel said no"));
        assert_eq!(wait.since, 100);
        assert!(!s.on_you(), "a retry the daemon does by itself is not the user's turn");
    }

    #[test]
    fn a_done_node_whose_delete_failed_permanently_is_you_failed_once_and_a_deferral_holds() {
        let mut c = case("done");
        c.problems = vec![retire_problem(false, 300)];
        let s = standing(&c);
        let NextActor::You { why: YouReason::Failed { error }, since } = &s.actor else { panic!("{:?}", s.actor) };
        assert!(error.contains("blaxel said no"));
        assert_eq!(*since, 100);
        assert!(s.on_you());
        // Deferred: it stays off the list (nothing newer reaches the user).
        c.deferral = Some(Deferral { until: None, deferred_at: 400 });
        assert!(!standing(&c).on_you());
        // A new failure after a daemon restart wakes it once.
        c.problems = vec![retire_problem(false, 500)];
        assert!(standing(&c).on_you());
    }

    #[test]
    fn other_problems_and_a_cleared_one_leave_a_done_node_done() {
        let mut c = case("done");
        let mut other = retire_problem(true, 1);
        other.kind = tod_store::node_problems::KIND_ADOPT.to_string();
        c.problems = vec![other];
        assert_eq!(standing(&c).actor, NextActor::Done);
        c.problems.clear();
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

    fn beat(started_at: i64, ago_ms: i64) -> CloudBeat {
        CloudBeat { node_id: Uuid::nil(), started_at, heartbeat_at: NOW - ago_ms, what: "active".into(), stopped: None }
    }

    #[test]
    fn a_supervisor_that_is_beating_in_the_cloud_is_working() {
        let mut c = case("active");
        c.cloud = Some(beat(100, 5_000));
        assert_eq!(standing(&c).actor, NextActor::Working { what: Work::Cloud, since: 100 });
        // Working wins over a stale outcome, as a local lease does.
        c.attention = requests(Uuid::nil(), &[10]);
        assert_eq!(standing(&c).actor, NextActor::Working { what: Work::Cloud, since: 100 });
    }

    #[test]
    fn a_supervisor_that_stopped_beating_is_a_loss_the_user_is_told_of() {
        let mut c = case("active");
        c.cloud = Some(beat(100, tod_store::cloud_activity::STALE_AFTER_MS + 5_000));
        match standing(&c).actor {
            NextActor::You { why: YouReason::Failed { error }, .. } => {
                assert!(error.contains("cloud supervisor stopped responding"), "{error}");
            }
            other => panic!("not a failure: {other:?}"),
        }
    }

    #[test]
    fn the_app_sending_a_node_to_the_cloud_is_work_from_the_click() {
        let mut c = case("active");
        c.leases = vec![live_lease(KIND_CLOUD_START, 40)];
        assert_eq!(standing(&c).actor, NextActor::Working { what: Work::CloudStart, since: 40 });
        assert_eq!(Work::from_kind(KIND_CLOUD_START), Work::CloudStart);
    }

    #[test]
    fn the_app_repairing_a_nodes_sandbox_is_work() {
        let mut c = case("pr_draft");
        c.leases = vec![live_lease(KIND_CLOUD_REPAIR, 40)];
        assert_eq!(standing(&c).actor, NextActor::Working { what: Work::CloudRepair, since: 40 });
        assert_eq!(Work::from_kind(KIND_CLOUD_REPAIR), Work::CloudRepair);
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

    fn permission_request(node: Uuid, since: i64, summary: &str) -> AttentionItem {
        AttentionItem {
            kind: AttentionKind::Decision,
            id: Uuid::new_v4(),
            node_id: node,
            summary: summary.into(),
            options: Vec::new(),
            since,
            reason: RequestReason::Access,
            permission: true,
        }
    }

    fn attention_of(node: Uuid, mut items: Vec<AttentionItem>) -> NodeAttention {
        items.sort_by_key(|i| i.since);
        NodeAttention {
            node_id: node,
            waiting_since: items.first().map(|i| i.since),
            count: items.len(),
            items,
        }
    }

    #[test]
    fn a_pending_permission_is_its_own_reason() {
        let mut c = case("active");
        c.attention = attention_of(Uuid::nil(), vec![permission_request(Uuid::nil(), 40, "Run `rm`")]);
        assert_eq!(standing(&c).actor, you(YouReason::Permission { title: "Run `rm`".into() }, 40));
    }

    #[test]
    fn a_permission_mixed_with_another_request_stays_a_request() {
        let mut c = case("active");
        let mut items = vec![permission_request(Uuid::nil(), 40, "Run `rm`")];
        items.extend(requests(Uuid::nil(), &[50]).items);
        c.attention = attention_of(Uuid::nil(), items);
        assert_eq!(standing(&c).actor, you(YouReason::Request { count: 2 }, 40));
        // An access request that is not a permission is a request too.
        let mut plain = permission_request(Uuid::nil(), 40, "needs a token");
        plain.permission = false;
        c.attention = attention_of(Uuid::nil(), vec![plain]);
        assert_eq!(standing(&c).actor, you(YouReason::Request { count: 1 }, 40));
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
            Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 6, elapsed_secs: 0 } },
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
        assert_eq!(WaitKind::from_spec("github:pr:7:bot-review:greptile"), WaitKind::BotReview);
        assert_eq!(WaitKind::from_spec("github:pr:7:checks"), WaitKind::Checks);
        assert_eq!(WaitKind::from_spec(""), WaitKind::Other);
        assert_eq!(WaitKind::from_spec("github:pr 12 checks"), WaitKind::Checks, "the space form agents write");
    }

    #[test]
    fn a_review_spec_round_trips_and_both_forms_are_a_human_review() {
        let reviewers = vec!["maria".to_string(), "team:checks".to_string()];
        let spec = review_spec(Some(7), &reviewers);
        assert_eq!(spec, "github:pr:7:review:maria,team:checks");
        assert_eq!(WaitKind::from_spec(&spec), WaitKind::HumanReview);
        assert_eq!(review_spec_reviewers(&spec), Some("maria,team:checks"));
        assert_eq!(review_spec(Some(7), &[]), "github:pr:7:review");
        assert_eq!(review_spec(None, &[]), "github:pr:review");
        assert_eq!(WaitKind::from_spec("github:pr:7:review"), WaitKind::HumanReview);
        assert_eq!(review_spec_reviewers("github:pr:7:review"), None);
        assert_eq!(review_spec_reviewers("github:pr:7:merged"), None);
    }

    #[test]
    fn a_human_review_wait_names_the_requested_reviewers() {
        let mut c = case("pr");
        c.waits = vec![wait("github:pr:7:review:maria,li", NOW + 60_000, NOW)];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert_eq!((wait.kind, wait.who.as_deref()), (WaitKind::HumanReview, Some("maria,li")));
        c.waits = vec![wait_at(NOW + 60_000, NOW)];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert_eq!((wait.kind, wait.who), (WaitKind::HumanReview, None));
    }

    #[test]
    fn a_bot_review_wait_names_the_bot() {
        let mut c = case("pr");
        c.waits = vec![wait("github:pr:7:bot-review:greptile", NOW + 60_000, NOW)];
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert_eq!((wait.kind, wait.who.as_deref()), (WaitKind::BotReview, Some("greptile")));
    }

    #[test]
    fn a_wait_past_due_is_a_fault_never_others_and_not_yet_due_is_others() {
        let interval = 60_000;
        let mut c = case("pr");
        c.cloud = Some(beat(100, tod_store::cloud_activity::STALE_AFTER_MS + 5_000));
        c.cloud.as_mut().unwrap().what = tod_store::cloud_activity::IDLE_WHAT.into();
        // Looked at 10 minutes ago, due again 9 minutes ago, 1 minute interval.
        c.waits = vec![wait("github:pr:7:review", NOW - 9 * 60_000, NOW - 9 * 60_000 - interval)];
        assert!(
            matches!(standing(&c).actor, NextActor::You { why: YouReason::Stopped { .. }, .. }),
            "past due beyond its grace is the user's"
        );
        // The same wait on a node nothing schedules: 9 minutes is still within the grace.
        c.cloud = None;
        let NextActor::Others { wait } = standing(&c).actor else { panic!("not others") };
        assert!(!wait.overdue, "an Others wait is never overdue");
        // Due a moment ago is just the poller about to run.
        c.waits = vec![wait_at(NOW - 5_000, NOW - 65_000)];
        assert!(matches!(standing(&c).actor, NextActor::Others { .. }));
    }

    #[test]
    fn a_run_that_ended_waiting_long_past_its_time_with_no_row_is_the_users() {
        let mut c = case("pr");
        c.run.last_outcome = Some((Outcome::Waiting { what: "a review".into(), due_at_ms: NOW - 30 * 60_000 }, 50));
        let NextActor::You { why: YouReason::Stopped { reason }, .. } = standing(&c).actor else {
            panic!("not the user's")
        };
        assert!(reason.contains("a review") && reason.contains("nothing is looking"), "{reason}");
    }

    #[test]
    fn a_wait_of_a_stay_the_node_left_is_ignored() {
        let mut c = case("pr");
        let mut stale = wait("github:pr:7:review", NOW + 60_000, NOW);
        stale.stay = Some(500 - 1);
        c.waits = vec![stale.clone()];
        let NextActor::You { why: YouReason::NotStarted, .. } = standing(&c).actor else {
            panic!("a wait of an ended stay held the node")
        };
        stale.stay = Some(500);
        c.waits = vec![stale.clone()];
        assert!(matches!(standing(&c).actor, NextActor::Others { .. }));
        // An `until` wait (a usage limit) outlives the stay.
        stale.stay = Some(500 - 1);
        stale.kind = "until".into();
        c.waits = vec![stale];
        assert!(matches!(standing(&c).actor, NextActor::Others { .. }));
    }

    #[test]
    fn a_wait_long_past_with_nothing_looking_at_it_is_the_users_not_others() {
        let h = 3_600_000;
        // An agent's 24h deadline, long gone, on a node with no supervisor.
        let mut c = case("planning");
        c.waits = vec![wait("github:pr 2810 checks", NOW - 11 * 60_000, NOW - 25 * h)];
        let NextActor::You { why: YouReason::Stopped { reason }, since } = standing(&c).actor else {
            panic!("a stale wait is the user's, not others'")
        };
        assert!(reason.contains("was due 11m ago and nothing is looking at it"), "{reason}");
        assert_eq!(since, NOW - 11 * 60_000);

        // The poller about to run (a few minutes late) is still others'.
        c.waits = vec![wait("github:pr:7:review", NOW - 5 * 60_000, NOW - 65 * 60_000)];
        assert!(matches!(standing(&c).actor, NextActor::Others { .. }));

        // A cloud node's wake comes from the orchestrator: two intervals of grace.
        let mut cloud = case("pr_draft");
        cloud.cloud = Some(beat(100, tod_store::cloud_activity::STALE_AFTER_MS + 5_000));
        cloud.cloud.as_mut().unwrap().what = tod_store::cloud_activity::IDLE_WHAT.into();
        cloud.waits = vec![wait("github:pr:7:review", NOW - 59 * 60_000, NOW - 2 * h)];
        assert!(matches!(standing(&cloud).actor, NextActor::Others { .. }), "within 2 intervals");
        cloud.waits = vec![wait("github:pr:7:review", NOW - 3 * h, NOW - 4 * h)];
        assert!(matches!(standing(&cloud).actor, NextActor::You { why: YouReason::Stopped { .. }, .. }));
    }

    #[test]
    fn the_space_form_of_a_spec_is_classified_like_the_colon_form() {
        assert_eq!(WaitKind::from_spec("github:pr 2810 checks"), WaitKind::Checks);
        assert_eq!(WaitKind::from_spec("github:pr 5 merged"), WaitKind::Merge);
        assert_eq!(WaitKind::from_spec("github:pr 5 bot-review greptile"), WaitKind::BotReview);
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
    fn for_nodes_reads_a_stored_sandbox_retire_problem_for_a_done_node() {
        let fx = fixture();
        let done = node(&fx.conn, "done", Some("done"));
        let inputs = Inputs { now_ms: NOW, ..Default::default() };
        assert_eq!(for_nodes(&fx.conn, &[done], &inputs).unwrap()[&done].actor, NextActor::Done);
        let mut p = retire_problem(true, 5);
        p.node_id = done;
        NodeProblemRepo::new(&fx.conn).record(&p).unwrap();
        let got = for_nodes(&fx.conn, &[done], &inputs).unwrap();
        assert!(matches!(&got[&done].actor, NextActor::Others { .. }), "{:?}", got[&done].actor);
        NodeProblemRepo::new(&fx.conn).clear(done, tod_store::node_problems::KIND_SANDBOX_RETIRE).unwrap();
        assert_eq!(for_nodes(&fx.conn, &[done], &inputs).unwrap()[&done].actor, NextActor::Done);
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
    fn a_gate_check_holds_a_lease_while_it_runs_and_not_after() {
        use tod_store::fleet::FleetStore;
        let root = std::env::temp_dir().join(format!("tod-next-actor-lease-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        // The lease needs a node row; the node need not have Lifecycle.
        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::CreateNode {
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
        let node = fleet.flatten_outline(list_id).unwrap()[0].node.id;

        let seen = tod_store::activity::with_lease(&fleet, node, KIND_GATE_CHECK, "test:1", || {
            fleet.writer().flush().unwrap();
            fleet.read(|conn| ActivityRepo::new(conn).list_for_nodes(&[node])).unwrap()
        })
        .unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(Work::from_kind(&seen[0].kind), Work::GateCheck);
        fleet.writer().flush().unwrap();
        assert!(fleet.read(|conn| ActivityRepo::new(conn).list_for_nodes(&[node])).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_local_failed_run_does_not_speak_for_a_node_that_runs_in_the_cloud() {
        let fx = fixture();
        let local = node(&fx.conn, "local", Some("active"));
        let cloud = node(&fx.conn, "cloud", Some("active"));
        tod_store::cloud_nodes::upsert(&fx.conn, cloud, "node-cloud", "alice", 1).unwrap();
        for id in [local, cloud] {
            let mut state = crate::autopilot::AutopilotState::fresh();
            state.outcome = Some(Outcome::NeedsHuman {
                reason: NeedsHuman::AgentFailed { error: "not signed in".into() },
            });
            state.save(&fx.dir, id).unwrap();
        }
        let fleet = tod_store::fleet::FleetStore::open(&fx.dir).unwrap();
        let standings = compute(&fleet, &fx.dir).unwrap();
        assert!(
            matches!(standings[&local].actor, NextActor::You { why: YouReason::Failed { .. }, .. }),
            "a node run here keeps its failure: {:?}",
            standings[&local].actor
        );
        assert!(
            !matches!(standings[&cloud].actor, NextActor::You { why: YouReason::Failed { .. }, .. }),
            "the cloud node is not failed by a stale local run: {:?}",
            standings[&cloud].actor
        );
    }

    fn idle_beat(stopped: Option<&str>) -> CloudBeat {
        CloudBeat {
            node_id: Uuid::nil(),
            started_at: 100,
            heartbeat_at: NOW - 3 * 86_400_000,
            what: tod_store::cloud_activity::IDLE_WHAT.into(),
            stopped: stopped.map(str::to_string),
        }
    }

    #[test]
    fn a_cloud_wake_that_stopped_leaves_its_reason_not_not_started() {
        let mut c = case("planning");
        c.cloud = Some(idle_beat(None));
        assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500), "an idle beat without a reason is as before");
        c.cloud = Some(idle_beat(Some("the pull request needs a person: threads")));
        let heard = NOW - 3 * 86_400_000;
        assert_eq!(
            standing(&c).actor,
            you(YouReason::Stopped { reason: "the pull request needs a person: threads".into() }, heard)
        );
        assert!(standing(&c).on_you());
        // A request still comes first, and a live beat is working.
        c.attention = requests(Uuid::nil(), &[10]);
        assert!(matches!(standing(&c).actor, NextActor::You { why: YouReason::Request { .. }, .. }));
        c.attention = NodeAttention::default();
        c.cloud = Some(beat(100, 5_000));
        assert!(matches!(standing(&c).actor, NextActor::Working { what: Work::Cloud, .. }));
    }

    #[test]
    fn a_stop_question_answered_leave_it_stopped_names_the_question_and_what_resumes_it() {
        let mut c = case("planning");
        c.stop_answer = Some(StopAnswer { kind: crate::stop_questions::BUDGET.into(), at: 700 });
        let NextActor::You { why: YouReason::Stopped { reason }, since } = standing(&c).actor else {
            panic!("{:?}", standing(&c).actor)
        };
        assert_eq!(since, 700);
        assert!(reason.contains("its budget") && reason.contains("Leave it stopped") && reason.contains("Keep going"), "{reason}");
        // It outranks an older reason left in the beat and a wait.
        c.cloud = Some(idle_beat(Some("budget spent")));
        c.waits = vec![wait("github:pr:1:review", 5, 4)];
        assert!(matches!(standing(&c).actor, NextActor::You { since: 700, .. }));
    }

    #[test]
    fn a_supervised_node_the_user_paused_or_stopped_is_paused_and_a_spent_budget_says_so() {
        let supervisor = |outcome: &Outcome| {
            let words = crate::run_state::Words { budget: &crate::run_state::budget_text, stopped_by: Some("the supervisor") };
            let draft = crate::run_state::for_outcome(outcome, words);
            row(draft.phase, &draft.reason_kind, &draft.reason_text, 800, 800)
        };
        let mut c = case("planning");
        for reason in [crate::autopilot::local::PAUSED, crate::autopilot::local::STOPPED] {
            c.run_state = Some(supervisor(&Outcome::Stopped { reason: reason.into() }));
            assert_eq!(standing(&c).actor, you(YouReason::Paused, 800), "{reason}");
        }
        c.run_state = Some(supervisor(&Outcome::BudgetExhausted {
            limit: BudgetLimit::Sessions { used: 6, elapsed_secs: 60 },
        }));
        assert_eq!(
            standing(&c).actor,
            you(YouReason::Stopped { reason: "budget spent: 6 sessions".into() }, 800)
        );
    }

    fn row(phase: Phase, kind: &str, text: &str, since: i64, updated_at: i64) -> RunState {
        RunState {
            node_id: Uuid::nil(),
            phase,
            reason_kind: kind.into(),
            reason_text: text.into(),
            since,
            resumable_by: tod_store::run_state::RESUMABLE_BY_YOU.into(),
            updated_at,
            writer: "t".into(),
        }
    }

    #[test]
    fn a_starting_row_is_working_from_the_click_then_a_failure_that_says_so() {
        let mut c = case("planning");
        c.run_state = Some(row(Phase::Starting, crate::run_state::kind::SENDING, "sending the node", NOW - 10_000, NOW - 10_000));
        assert_eq!(standing(&c).actor, NextActor::Working { what: Work::CloudStart, since: NOW - 10_000 });
        // Past what a boot and a seed may take, with nothing heard.
        c.run_state = Some(row(Phase::Starting, crate::run_state::kind::SENDING, "sending the node", 100, 100));
        let NextActor::You { why: YouReason::Failed { error }, since } = standing(&c).actor else {
            panic!("{:?}", standing(&c).actor)
        };
        assert_eq!(since, 100);
        assert!(error.contains("has not reported in") && error.contains("Start sends it again"), "{error}");
        // An older supervisor's later beat is the later word: the row is ignored.
        c.cloud = Some(idle_beat(None));
        c.run_state = Some(row(Phase::Starting, crate::run_state::kind::SENDING, "x", NOW - 4 * 86_400_000, NOW - 4 * 86_400_000));
        assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500));
    }

    #[test]
    fn a_stopped_row_says_why_for_cloud_and_local_and_an_older_one_gives_way() {
        use crate::run_state::kind;
        let mut c = case("planning");
        c.run_state = Some(row(Phase::Stopped, kind::PR_STUCK, "the pull request keeps needing work", 800, 800));
        assert_eq!(
            standing(&c).actor,
            you(YouReason::Stopped { reason: "the pull request keeps needing work".into() }, 800)
        );
        assert!(standing(&c).on_you());
        c.run_state = Some(row(Phase::Stopped, kind::FAILED, "boom", 800, 800));
        assert_eq!(standing(&c).actor, you(YouReason::Failed { error: "boom".into() }, 800));
        c.run_state = Some(row(Phase::Stopped, kind::PAUSED, "you paused it", 800, 800));
        assert_eq!(standing(&c).actor, you(YouReason::Paused, 800));
        // The same row beats an idle beat's older reason.
        c.run_state = Some(row(Phase::Stopped, kind::STOPPED, "from the row", 800, 800));
        c.cloud = Some(CloudBeat { heartbeat_at: 801, ..idle_beat(Some("from the beat")) });
        assert_eq!(standing(&c).actor, you(YouReason::Stopped { reason: "from the row".into() }, 800));
        // An old supervisor woke after the row and ended without a reason: the row is stale.
        c.cloud = Some(CloudBeat { heartbeat_at: 800 + 60_000, ..idle_beat(None) });
        assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500));
        // Local: a saved run newer than the row wins; a row newer than the saved run hides it.
        c.cloud = None;
        c.run.last_outcome = Some((Outcome::Stopped { reason: "old".into() }, 900_000));
        assert_eq!(standing(&c).actor, you(YouReason::Stopped { reason: "old".into() }, 900_000));
        c.run_state = Some(row(Phase::Waiting, kind::WAITING_OUTSIDE, "a review", 950_000, 950_000));
        assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500), "the row is newer and says it only waits");
    }

    #[test]
    fn a_working_waiting_or_done_row_asks_nothing_of_the_user_itself() {
        use crate::run_state::kind;
        let mut c = case("planning");
        for phase in [Phase::Working, Phase::Waiting, Phase::Done] {
            c.run_state = Some(row(phase, kind::WORKING, "x", 800, 800));
            assert_eq!(standing(&c).actor, you(YouReason::NotStarted, 500), "{phase:?}");
        }
    }

    #[test]
    fn a_cloud_node_whose_sandbox_is_gone_says_so() {
        let mut c = case("planning");
        c.cloud_lost = Some(CloudLost { since: 900, deleted_by_user: true });
        c.cloud = Some(beat(100, tod_store::cloud_activity::STALE_AFTER_MS + 5_000));
        assert_eq!(
            standing(&c).actor,
            you(YouReason::Stopped { reason: "its sandbox was deleted; Start makes a new one".into() }, 900)
        );
        c.cloud_lost = Some(CloudLost { since: 900, deleted_by_user: false });
        let NextActor::You { why: YouReason::Stopped { reason }, .. } = standing(&c).actor else { panic!() };
        assert!(reason.contains("sandbox is gone"), "{reason}");
    }

    /// The app's `starting` row, then a runner's stop, each written the way
    /// the app and the runners write them and read back through `compute`.
    #[test]
    fn compute_reads_the_run_state_the_app_and_runners_wrote() {
        let fx = fixture();
        let sent = node(&fx.conn, "sent", Some("planning"));
        let stopped = node(&fx.conn, "stopped", Some("planning"));
        let local = node(&fx.conn, "local", Some("planning"));
        let fleet = tod_store::fleet::FleetStore::open(&fx.dir).unwrap();
        crate::cloud_sync::record_starting(&fleet, &sent.to_string()).unwrap();
        let stop = crate::run_state::for_outcome(
            &Outcome::NeedsHuman { reason: NeedsHuman::NoProgress { step: "verify".into() } },
            crate::run_state::Words::local(),
        );
        crate::run_state::record(&fleet, stopped, stop.clone(), "supervisor").unwrap();
        let paused = crate::run_state::for_outcome(&Outcome::Stopped { reason: crate::autopilot::local::PAUSED.into() }, crate::run_state::Words::local());
        crate::run_state::record(&fleet, local, paused, "local").unwrap();
        let standings = compute(&fleet, &fx.dir).unwrap();
        assert!(matches!(standings[&sent].actor, NextActor::Working { what: Work::CloudStart, .. }), "{:?}", standings[&sent].actor);
        assert_eq!(
            standings[&stopped].actor.clone(),
            NextActor::You { why: YouReason::Stopped { reason: stop.reason_text }, since: match &standings[&stopped].actor {
                NextActor::You { since, .. } => *since,
                other => panic!("{other:?}"),
            } }
        );
        assert!(matches!(standings[&local].actor, NextActor::You { why: YouReason::Paused, .. }), "{:?}", standings[&local].actor);
    }

    #[test]
    fn compute_reads_the_stop_answer_and_the_lost_row_from_the_store() {
        use crate::stop_questions as sq;
        let fx = fixture();
        let stopped = node(&fx.conn, "stopped", Some("planning"));
        let lost = node(&fx.conn, "lost", Some("planning"));
        let deleted = node(&fx.conn, "deleted", Some("planning"));
        let left = node(&fx.conn, "left", Some("planning"));
        for id in [stopped, lost, deleted, left] {
            tod_store::cloud_nodes::upsert(&fx.conn, id, "sb", "alice", 1).unwrap();
        }
        tod_store::cloud_nodes::mark_lost(&fx.conn, lost, 50).unwrap();
        tod_store::cloud_nodes::mark_lost(&fx.conn, deleted, 60).unwrap();
        // `left` had a wake that ended needing someone.
        tod_store::cloud_activity::beat(&fx.conn, left, "planning", 10).unwrap();
        tod_store::cloud_activity::idle_stopped(&fx.conn, left, Some("no progress"), 20).unwrap();
        crate::cloud_sync::set_sandbox_deleted_by_user(&fx.dir, &deleted.to_string(), true).unwrap();
        let fleet = tod_store::fleet::FleetStore::open(&fx.dir).unwrap();
        fleet
            .interview(
                tod_store::interview::ACTOR_AGENT,
                tod_store::interview::InterviewCommand::AskDecision {
                    node_id: stopped,
                    conversation_id: None,
                    protocol: Some(sq::BUDGET.into()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Keep going?".into(),
                        options: sq::SUPERVISOR_OPTIONS.iter().map(|o| o.to_string()).collect(),
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        let id = fleet.read(|c| sq::latest(c, stopped)).unwrap().unwrap().decision;
        let reason_of = |standings: &HashMap<Uuid, NodeStanding>, n: Uuid| match &standings[&n].actor {
            NextActor::You { why: YouReason::Stopped { reason }, .. } => Some(reason.clone()),
            _ => None,
        };
        let standings = compute(&fleet, &fx.dir).unwrap();
        assert!(matches!(standings[&stopped].actor, NextActor::You { why: YouReason::Request { .. }, .. }), "pending is a request");
        fleet
            .interview(
                tod_store::interview::ACTOR_USER,
                tod_store::interview::InterviewCommand::AnswerDecision { decision_id: id, option: Some(2), text: None },
            )
            .unwrap();
        let standings = compute(&fleet, &fx.dir).unwrap();
        assert!(reason_of(&standings, stopped).unwrap().contains("its budget"));
        assert!(reason_of(&standings, lost).unwrap().contains("sandbox is gone"));
        assert!(reason_of(&standings, deleted).unwrap().contains("was deleted"));
        assert_eq!(reason_of(&standings, left).as_deref(), Some("no progress"));
        // Answered to continue: nothing stops it any more.
        fleet
            .interview(
                tod_store::interview::ACTOR_USER,
                tod_store::interview::InterviewCommand::AnswerDecision { decision_id: id, option: Some(1), text: None },
            )
            .unwrap();
        let standings = compute(&fleet, &fx.dir).unwrap();
        assert_eq!(reason_of(&standings, stopped), None);
    }

    #[test]
    fn for_nodes_with_no_ids_reads_nothing() {
        let fx = fixture();
        assert!(for_nodes(&fx.conn, &[], &Inputs::default()).unwrap().is_empty());
    }
}
