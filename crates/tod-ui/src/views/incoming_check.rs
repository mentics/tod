//! Checking nodes against their incoming changes
//! (`doc/conversation/incoming-changes.md` §5, §8): one entity the shell
//! shares between the lifecycle panel's **Check now** and the task tree's
//! multi-select "Check incoming changes", so a check started in either
//! shows its progress and its summary in both.
//!
//! The work is `tod_core::incoming::IncomingRunner`: one short-lived agent
//! session per node, at most `max_parallel_agent_sessions` at once. It is
//! advanced from a timer that only takes the agent lock when it is free, so
//! the UI never waits on it. Afterwards the summary lists the nodes whose
//! verdict sends them back, and **Move back all** moves each through the
//! same path as the lifecycle panel's orange callout
//! (`lifecycle_validity::regression`, then `LifecycleController::revert_to`).

use crate::interview::agent::SharedAgent;
use crate::views::lifecycle_control::LifecycleController;
use gpui::{Context, Entity, Task};
use std::sync::Arc;
use std::time::Duration;
use tod_core::conversation::ConversationConfig;
use tod_core::incoming::{IncomingRunner, NodeOutcome, NodeResult};
use tod_core::lifecycle_validity::regression;
use tod_store::fleet::FleetStore;
use crate::interview::{TodPaths, TodSettings};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// A node the check says should go back, and where to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Affected {
    pub node: Uuid,
    pub title: String,
    pub affects: String,
    pub target: &'static str,
    pub note: String,
}

pub struct IncomingCheck {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    lifecycle: Entity<LifecycleController>,
    runner: Option<IncomingRunner>,
    /// The last finished check's results, until dismissed or replaced.
    results: Vec<NodeResult>,
    /// Why the last check could not start.
    error: Option<String>,
    /// Move back all was pressed once and waits for its confirmation.
    move_back_armed: bool,
    /// What Move back all did, shown in place of the summary's list.
    moved: Option<String>,
    _poll: Option<Task<()>>,
}

impl IncomingCheck {
    pub fn new(
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        lifecycle: Entity<LifecycleController>,
    ) -> Self {
        Self {
            fleet,
            agent,
            lifecycle,
            runner: None,
            results: Vec::new(),
            error: None,
            move_back_armed: false,
            moved: None,
            _poll: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.runner.is_some()
    }

    /// `(finished, total)` while a check runs.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.runner.as_ref().map(|r| (r.finished(), r.total()))
    }

    /// Whether the running check includes `node` and has not finished it.
    pub fn covers(&self, node: Uuid) -> bool {
        self.runner.as_ref().is_some_and(|r| r.covers(node))
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The last finished check's results; empty once dismissed.
    pub fn results(&self) -> &[NodeResult] {
        &self.results
    }

    pub fn has_summary(&self) -> bool {
        !self.results.is_empty() || self.error.is_some()
    }

    pub fn move_back_armed(&self) -> bool {
        self.move_back_armed
    }

    pub fn moved(&self) -> Option<&str> {
        self.moved.as_deref()
    }

    /// The nodes in the last check whose verdict sends them back.
    pub fn affected(&self) -> Vec<Affected> {
        self.results
            .iter()
            .filter_map(|r| match &r.outcome {
                NodeOutcome::Verdict {
                    affects,
                    note,
                    target: Some(target),
                } => Some(Affected {
                    node: r.node,
                    title: r.title.clone(),
                    affects: affects.clone(),
                    target,
                    note: note.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Check `nodes` against their pending incoming changes. Nodes without
    /// any are cleared without an agent. Refused while a check runs.
    pub fn start(&mut self, nodes: Vec<Uuid>, cx: &mut Context<Self>) {
        if self.runner.is_some() || nodes.is_empty() {
            return;
        }
        self.results.clear();
        self.moved = None;
        self.move_back_armed = false;
        let (config, cap) = match driver_config(&self.fleet) {
            Ok(found) => found,
            Err(err) => {
                self.error = Some(err);
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.runner = Some(IncomingRunner::new(config, cap, nodes));
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let Ok(running) = this.update(cx, |this, cx| this.poll(cx)) else {
                    break;
                };
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Advance the runner when the agent is free. `false` once it is done.
    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(runner) = self.runner.as_mut() else {
            return false;
        };
        let changed = match self.agent.try_lock() {
            Ok(mut agent) => runner.tick(&self.fleet, agent.as_mut()),
            Err(_) => false,
        };
        if runner.is_done() {
            let runner = self.runner.take().expect("checked above");
            self.results = runner.results().to_vec();
            cx.notify();
            return false;
        }
        if changed {
            cx.notify();
        }
        true
    }

    /// Hide the summary.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.results.clear();
        self.error = None;
        self.moved = None;
        self.move_back_armed = false;
        cx.notify();
    }

    /// First press arms, second press moves every affected node back to the
    /// latest state that still holds — the orange callout's judgement, not
    /// the verdict's own target, so a node already moved back (or never
    /// that far) is left alone.
    pub fn move_back_all(&mut self, cx: &mut Context<Self>) {
        if !self.move_back_armed {
            self.move_back_armed = true;
            cx.notify();
            return;
        }
        self.move_back_armed = false;
        let mut moved = Vec::new();
        let mut unchanged = 0;
        for affected in self.affected() {
            let found = self
                .fleet
                .read(|conn| regression(conn, affected.node))
                .ok()
                .flatten();
            let Some(found) = found else {
                unchanged += 1;
                continue;
            };
            let id = affected.node.to_string();
            let ok = self
                .lifecycle
                .update(cx, |c, cx| c.revert_to(&id, found.target, cx));
            if ok {
                moved.push(format!("{} → {}", affected.title, found.target));
            } else {
                unchanged += 1;
            }
        }
        let mut text = if moved.is_empty() {
            "No node moved.".to_string()
        } else {
            format!("Moved back: {}.", moved.join("; "))
        };
        if unchanged > 0 {
            text.push_str(&format!(
                " {unchanged} already held no state it should leave, or could not move."
            ));
        }
        self.moved = Some(text);
        cx.notify();
    }

    pub fn cancel_move_back(&mut self, cx: &mut Context<Self>) {
        self.move_back_armed = false;
        cx.notify();
    }
}

/// The evaluation sessions' settings, and the parallel-session cap.
fn driver_config(fleet: &FleetStore) -> Result<(ConversationConfig, usize), String> {
    let paths = TodPaths::discover().map_err(|e| format!("{e:#}"))?;
    let settings = TodSettings::load(&paths).unwrap_or_default();
    let media = tod_core::media::MediaPaths::discover().map_err(|e| format!("Media bundle: {e}"))?;
    Ok((
        ConversationConfig {
            data_root: fleet.paths().root().to_path_buf(),
            media,
            launch: settings.interview_launch_options(),
            context: settings.interview_context.clone(),
        },
        settings.parallel_agent_sessions(),
    ))
}

/// One line per node for a summary: what the check concluded.
pub fn outcome_line(result: &NodeResult) -> String {
    match &result.outcome {
        NodeOutcome::Cleared => format!("{}: the changes net to nothing; cleared.", result.title),
        NodeOutcome::Verdict {
            affects,
            note,
            target: Some(target),
        } => format!("{}: affects {affects} → back to {target}. {note}", result.title),
        NodeOutcome::Verdict { note, .. } => format!("{}: not affected. {note}", result.title),
        NodeOutcome::Failed(err) => format!("{}: check failed — {err}", result.title),
    }
}
