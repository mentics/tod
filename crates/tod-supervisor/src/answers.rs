//! The user's answers to the questions this supervisor and the watchdog ask
//! when they stop a node (`tod_core::stop_questions`), read at the start of
//! every wake.
//!
//! The node's latest such question governs: answered "Leave it stopped" /
//! "Leave it asleep", the wake does nothing (no work, no new question, no
//! wake scheduled); answered to continue, an answer not yet acted on is acted
//! on once — after the budget ran out, that grants another budget of the
//! configured size — and the wake goes on. Still pending, the wake goes on
//! too (it still takes context changes), and the autopilot stops again on
//! the pending decision or the spent budget; [`still_asking`] keeps the
//! supervisor from asking a second time.
//!
//! What was acted on is kept beside the autopilot's state
//! (`stop-answers.json` in the state directory).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tod_core::autopilot::Budget;
use tod_core::stop_questions::{self, Answer};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

const FILE: &str = "stop-answers.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Acted {
    /// `Latest::answer_key`s acted on.
    handled: Vec<String>,
    /// Budgets granted beyond the first.
    budget_grants: u32,
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

fn load(state_dir: &Path) -> Result<Acted> {
    let file = path(state_dir);
    match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("{} is not readable ({e}); fix or remove it", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Acted::default()),
        Err(e) => Err(anyhow::anyhow!("read {}: {e}", file.display())),
    }
}

fn save(state_dir: &Path, acted: &Acted) -> Result<()> {
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(path(state_dir), serde_json::to_vec(acted)?)?;
    Ok(())
}

/// What the wake does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Before {
    /// The user said to leave it: stop here.
    LeaveStopped(String),
    /// Go on, with this budget.
    Go(Budget),
}

/// `budget` granted `grants` more times.
fn granted(budget: Budget, grants: u32) -> Budget {
    let times = grants.saturating_add(1);
    Budget {
        max_sessions: budget.max_sessions.saturating_mul(times),
        max_duration: budget.max_duration.checked_mul(times).unwrap_or(Duration::MAX),
    }
}

/// See the module docs.
pub fn before_wake(fleet: &FleetStore, node: Uuid, state_dir: &Path, budget: Budget) -> Result<Before> {
    before_wake_with(fleet, node, state_dir, budget, false)
}

/// [`before_wake`]; with `run_requested` (the user asked to run the node, a
/// request for a new session) an answer to leave it stopped is read as an
/// answer to continue, once: it is recorded as handled (granting another
/// budget after the budget ran out, as a continue answer does), so the next
/// wake does not go back to leaving it stopped. The caller finds the request;
/// this module reads no file but its own.
pub fn before_wake_with(
    fleet: &FleetStore,
    node: Uuid,
    state_dir: &Path,
    budget: Budget,
    run_requested: bool,
) -> Result<Before> {
    before_wake_requested(fleet, node, state_dir, budget, run_requested, &[])
}

/// [`before_wake_with`]; `fresh_budget` are the pending `fresh_budget`
/// requests (`tod_store::node_requests`), each of which grants another budget,
/// once: its id is kept as handled, so a wake that leaves before the request
/// is consumed does not grant it again. A budget the node's own stop question
/// granted in this same call counts for it (one grant, not two).
pub fn before_wake_requested(
    fleet: &FleetStore,
    node: Uuid,
    state_dir: &Path,
    budget: Budget,
    run_requested: bool,
    fresh_budget: &[Uuid],
) -> Result<Before> {
    let mut granted_now = false;
    let latest = fleet.read(|conn| stop_questions::latest(conn, node))?;
    let mut acted = load(state_dir)?;
    if let Some(latest) = latest {
        let mut answer = latest.answer;
        if answer == Answer::Stop && run_requested {
            tracing::info!(kind = %latest.kind, "a request to run overrode the answer to leave it stopped");
            answer = Answer::Continue;
        }
        match answer {
            Answer::Stop => {
                return Ok(Before::LeaveStopped(format!(
                    "left stopped: {}",
                    stop_questions::left_stopped_reason(&latest.kind)
                )));
            }
            Answer::Continue => {
                let key = latest.answer_key.unwrap_or_default();
                if !acted.handled.contains(&key) {
                    if latest.kind == stop_questions::BUDGET {
                        acted.budget_grants += 1;
                        granted_now = true;
                        tracing::info!(grants = acted.budget_grants, "the user granted another budget");
                    } else {
                        tracing::info!(kind = %latest.kind, "the user said to keep going");
                    }
                    acted.handled.push(key);
                    save(state_dir, &acted)?;
                }
            }
            Answer::Pending => {}
        }
    }
    for id in fresh_budget {
        let key = format!("request:{id}");
        if acted.handled.contains(&key) {
            continue;
        }
        if !granted_now {
            acted.budget_grants += 1;
            granted_now = true;
            tracing::info!(grants = acted.budget_grants, "the user asked for a fresh budget");
        }
        acted.handled.push(key);
        save(state_dir, &acted)?;
    }
    Ok(Before::Go(granted(budget, acted.budget_grants)))
}

/// Whether the node's latest stop question is still unanswered, so a new one
/// would only repeat it.
pub fn still_asking(fleet: &FleetStore, node: Uuid) -> Result<bool> {
    let latest = fleet.read(|conn| stop_questions::latest(conn, node))?;
    Ok(latest.is_some_and(|l| l.answer == Answer::Pending))
}
