//! The questions an autonomous node's cloud supervisor and the watchdog ask
//! the user when they stop it, and how their answers are read.
//!
//! Each is an ordinary pending decision on the node (it reaches the user
//! through the decisions panel like any other). What marks it as one of
//! these is its `protocol` column, which holds one of [`KINDS`] instead of a
//! conversation protocol — no schema of its own:
//!
//! - [`FAILURES`]: the agent failed too many times in a row
//!   ([`KEEP_GOING`] / [`LEAVE_STOPPED`]);
//! - [`BUDGET`]: the autopilot spent its budget (same options; keep going
//!   grants another budget of the same size);
//! - [`WATCHDOG`]: the watchdog let the node's sandbox sleep
//!   ([`WAKE_AGAIN`] / [`LEAVE_ASLEEP`]).
//!
//! The node's **latest** such question governs ([`latest`]): pending, the
//! node stays stopped (the autopilot stops on any pending decision); answered
//! with a "leave" option, it stays stopped, asks nothing, and schedules no
//! wake, until the user answers it again (answers are append-only, the last
//! one counts); answered otherwise (an option to continue, or free text), it
//! carries on. The orchestrator pokes the node when an answer to one of these
//! arrives (`tod_orchestrator::answers`).

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use tod_store::decisions::DecisionRepo;
use tod_store::outline::uuid_blob::uuid_to_blob;
use uuid::Uuid;

pub const FAILURES: &str = "supervisor:failures";
pub const BUDGET: &str = "supervisor:budget";
pub const WATCHDOG: &str = "watchdog";
pub const KINDS: [&str; 3] = [FAILURES, BUDGET, WATCHDOG];

pub const KEEP_GOING: &str = "Keep going";
pub const LEAVE_STOPPED: &str = "Leave it stopped";
pub const WAKE_AGAIN: &str = "Wake it again";
pub const LEAVE_ASLEEP: &str = "Leave it asleep";

/// The supervisor's options, in order.
pub const SUPERVISOR_OPTIONS: [&str; 2] = [KEEP_GOING, LEAVE_STOPPED];
/// The watchdog's options, in order.
pub const WATCHDOG_OPTIONS: [&str; 2] = [WAKE_AGAIN, LEAVE_ASLEEP];

/// Set on a `tod-cli decisions ask` to file the decision as one of [`KINDS`]
/// (the orchestrator asks the watchdog's through `tod-cli`). Not for agents.
pub const KIND_ENV: &str = "TOD_DECISION_KIND";

pub fn is_kind(protocol: Option<&str>) -> bool {
    protocol.is_some_and(|p| KINDS.contains(&p))
}

/// What the user said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Pending,
    Continue,
    Stop,
}

/// The node's latest stop question and its latest answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    pub decision: Uuid,
    pub kind: String,
    pub answer: Answer,
    /// Identifies the answer (`<decision>:<answered_at>`), so it is acted on
    /// once; `None` while pending.
    pub answer_key: Option<String>,
}

/// See the module docs. Withdrawn questions are skipped.
pub fn latest(conn: &Connection, node: Uuid) -> Result<Option<Latest>> {
    let id: Option<Vec<u8>> = conn
        .query_row(
            "SELECT id FROM decisions
             WHERE node_id = ?1 AND protocol IN (?2, ?3, ?4) AND status != 'withdrawn'
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![uuid_to_blob(node), KINDS[0], KINDS[1], KINDS[2]],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id.and_then(|b| Uuid::from_slice(&b).ok()) else {
        return Ok(None);
    };
    let Some(found) = DecisionRepo::new(conn).get_with_answers(id)? else {
        return Ok(None);
    };
    let kind = found.decision.protocol.clone().unwrap_or_default();
    let Some(last) = found.answers.iter().max_by_key(|a| (a.answered_at, a.id)) else {
        return Ok(Some(Latest { decision: id, kind, answer: Answer::Pending, answer_key: None }));
    };
    let chosen = last
        .option
        .and_then(|i| usize::try_from(i - 1).ok())
        .and_then(|i| found.decision.options.get(i))
        .map(String::as_str);
    let answer = match chosen {
        Some(LEAVE_STOPPED) | Some(LEAVE_ASLEEP) => Answer::Stop,
        _ => Answer::Continue,
    };
    Ok(Some(Latest {
        decision: id,
        kind,
        answer,
        answer_key: Some(format!("{id}:{}", last.answered_at)),
    }))
}

/// The node `decision` is on, when it is
/// one of these questions.
pub fn node_of(conn: &Connection, decision: Uuid) -> Result<Option<Uuid>> {
    Ok(DecisionRepo::new(conn)
        .get(decision)?
        .filter(|d| is_kind(d.protocol.as_deref()))
        .map(|d| d.node_id))
}
