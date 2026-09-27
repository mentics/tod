//! Checking nodes against their incoming changes
//! (`doc/conversation/incoming-changes.md` §5, §8): one entity the shell
//! shares between the lifecycle panel's **Check now** and the task tree's
//! multi-select "Check incoming changes", so a check started in either
//! shows its progress and its summary in both.
//!
//! The work is `tod_core::incoming::IncomingRunner`: one short-lived agent
//! session per node, at most `max_parallel_agent_sessions` at once. Starting
//! and collecting a session runs git, Docker, and `tod-cli`, so each tick
//! takes the runner to the background executor, where the shared agent is
//! locked only for each provider call; the UI shows what it last knew of the
//! run meanwhile, and never waits on it. Afterwards the summary lists the nodes whose
//! verdict sends them back, and **Move back all** moves each through the
//! same path as the lifecycle panel's orange callout
//! (`lifecycle_validity::regression`, then `LifecycleController::revert_to`).
//!
//! A gate check on a node at `ready` or later with pending entries waits on
//! this first ([`IncomingCheck::hold_gate_check`], §5 "Before a gate
//! check"): the node is checked, and only when nothing it inherits affects
//! it does [`IncomingCheckEvent::GateReady`] tell the shell to run the gate
//! check. Otherwise the lifecycle controller's status says why it did not
//! run, which both the conversation view and the lifecycle panel show.
//!
//! The timer below is not a poll of stored state. It pumps the agent
//! provider's in-memory run state (`AgentProvider::poll_run`, through
//! `ConversationDriver::tick`) exactly as the conversation view does for its
//! own drivers; the provider has no completion callback to wait on. The
//! store is read once when a node's session starts and once when it ends.

use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use crate::views::lifecycle_control::LifecycleController;
use gpui::{Context, Entity, EventEmitter, Task};
use std::sync::Arc;
use std::time::Duration;
use tod_core::conversation::{ConversationConfig, SharedAgentAccess};
use tod_core::incoming::{
    BeforeGate, IncomingRunner, NodeOutcome, NodeResult, needs_check_before_gate,
};
use tod_core::lifecycle_validity::regression;
use tod_store::fleet::FleetStore;
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

pub enum IncomingCheckEvent {
    /// `node`'s incoming changes are checked and affect nothing: run the
    /// gate check that was waiting on them.
    GateReady(Uuid),
}

pub struct IncomingCheck {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    lifecycle: Entity<LifecycleController>,
    run: Option<Run>,
    /// The last finished check's results, until dismissed or replaced.
    results: Vec<NodeResult>,
    /// Why the last check could not start.
    error: Option<String>,
    /// Move back all was pressed once and waits for its confirmation.
    move_back_armed: bool,
    /// What Move back all did, shown in place of the summary's list.
    moved: Option<String>,
    /// Nodes whose gate check waits on the running check.
    before_gate: Vec<Uuid>,
    /// The running check was started only for gate checks: its results are
    /// reported through the gate check, not the summary card.
    gate_only: bool,
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
            run: None,
            results: Vec::new(),
            error: None,
            move_back_armed: false,
            moved: None,
            before_gate: Vec::new(),
            gate_only: false,
            _poll: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// `(finished, total)` while a check runs.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.run.as_ref().map(|r| (r.finished, r.total))
    }

    /// Whether the running check includes `node` and has not finished it.
    pub fn covers(&self, node: Uuid) -> bool {
        self.run.as_ref().is_some_and(|r| r.covers(node))
    }

    /// Whether a gate check on `node` waits on the running check.
    pub fn holds_gate_check(&self, node: Uuid) -> bool {
        self.before_gate.contains(&node)
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
        if self.run.is_some() || nodes.is_empty() {
            return;
        }
        self.results.clear();
        self.moved = None;
        self.move_back_armed = false;
        self.error = self.start_run(nodes, false, cx).err();
        cx.notify();
    }

    /// Whether a gate check on `node` has to wait for its incoming changes
    /// to be checked. When it does, the check is started (or the running
    /// one, if it covers `node`, is waited on) and the gate check runs on
    /// [`IncomingCheckEvent::GateReady`]; the lifecycle controller shows the
    /// progress, or why the gate check did not run.
    pub fn hold_gate_check(&mut self, node: Uuid, cx: &mut Context<Self>) -> bool {
        let needs = self
            .fleet
            .read(|conn| needs_check_before_gate(conn, node))
            .unwrap_or(false);
        if !needs {
            return false;
        }
        if self.before_gate.contains(&node) {
            return true;
        }
        let task_id = node.to_string();
        let joined = match &self.run {
            Some(run) => run.covers(node),
            None => match self.start_run(vec![node], true, cx) {
                Ok(()) => true,
                Err(err) => {
                    let report = BeforeGate::Failed(err).report();
                    self.lifecycle
                        .update(cx, |c, cx| c.report_before_gate(&task_id, None, report, cx));
                    return true;
                }
            },
        };
        if !joined {
            self.lifecycle.update(cx, |c, cx| {
                c.report_before_gate(
                    &task_id,
                    None,
                    Some(
                        "Gate check not run: an incoming-changes check on other nodes is \
                         running. Run the gate check again when it finishes."
                            .to_string(),
                    ),
                    cx,
                )
            });
            return true;
        }
        self.before_gate.push(node);
        self.lifecycle.update(cx, |c, cx| {
            c.report_before_gate(
                &task_id,
                Some("Checking its incoming changes before the gate check…".to_string()),
                None,
                cx,
            )
        });
        cx.notify();
        true
    }

    fn start_run(
        &mut self,
        nodes: Vec<Uuid>,
        gate_only: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (config, cap) = driver_config(&self.fleet)?;
        self.gate_only = gate_only;
        self.run_with(IncomingRunner::new(config, cap, nodes), cx);
        Ok(())
    }

    /// Run `runner`, a tick at a time on the background executor.
    fn run_with(&mut self, runner: IncomingRunner, cx: &mut Context<Self>) {
        self.run = Some(Run::new(runner));
        let fleet = self.fleet.clone();
        let agent = self.agent.clone();
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let Ok(Some(mut runner)) = this.update(cx, |this, _| this.take_runner()) else {
                    break;
                };
                let fleet = fleet.clone();
                let agent = agent.clone();
                let (runner, changed) = cx
                    .background_executor()
                    .spawn(async move {
                        let changed = runner.tick(&fleet, &mut SharedAgentAccess(&agent));
                        (runner, changed)
                    })
                    .await;
                let Ok(running) = this.update(cx, |this, cx| this.ticked(runner, changed, cx))
                else {
                    break;
                };
                if !running {
                    break;
                }
            }
        }));
    }

    /// The runner, taken to be ticked off the main thread.
    fn take_runner(&mut self) -> Option<IncomingRunner> {
        self.run.as_mut().and_then(|run| run.runner.take())
    }

    /// The runner is back from a tick. `false` once it is done.
    fn ticked(&mut self, runner: IncomingRunner, changed: bool, cx: &mut Context<Self>) -> bool {
        let Some(run) = self.run.as_mut() else {
            return false;
        };
        if runner.is_done() {
            self.run = None;
            for node in std::mem::take(&mut self.before_gate) {
                self.release_gate_check(node, runner.results(), cx);
            }
            if !std::mem::take(&mut self.gate_only) {
                self.results = in_tree_order(&self.fleet, runner.results().to_vec());
            }
            cx.notify();
            return false;
        }
        run.put_back(runner);
        if changed {
            cx.notify();
        }
        true
    }

    /// Settle the gate check that waited on `node`'s check: run it when
    /// nothing inherited affects the node, report why not otherwise.
    fn release_gate_check(&mut self, node: Uuid, results: &[NodeResult], cx: &mut Context<Self>) {
        let gate = match results.iter().find(|r| r.node == node) {
            Some(result) => BeforeGate::from_outcome(&result.outcome),
            None => BeforeGate::Failed("the check did not reach this node".to_string()),
        };
        let task_id = node.to_string();
        let report = gate.report();
        let proceed = report.is_none();
        self.lifecycle
            .update(cx, |c, cx| c.report_before_gate(&task_id, None, report, cx));
        if proceed {
            cx.emit(IncomingCheckEvent::GateReady(node));
        }
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

impl EventEmitter<IncomingCheckEvent> for IncomingCheck {}

/// A running check: its runner, away on the background executor while a tick
/// runs, and what the view last knew of it.
struct Run {
    runner: Option<IncomingRunner>,
    finished: usize,
    total: usize,
    unfinished: Vec<Uuid>,
}

impl Run {
    fn new(runner: IncomingRunner) -> Self {
        let mut run = Self {
            runner: None,
            finished: 0,
            total: 0,
            unfinished: Vec::new(),
        };
        run.put_back(runner);
        run
    }

    fn covers(&self, node: Uuid) -> bool {
        self.unfinished.contains(&node)
    }

    fn put_back(&mut self, runner: IncomingRunner) {
        self.finished = runner.finished();
        self.total = runner.total();
        self.unfinished = runner.unfinished();
        self.runner = Some(runner);
    }
}

/// `results` in the outline's order, so the summary reads like the tree
/// rather than in whichever order the sessions happened to finish. Nodes
/// the outline no longer lists keep their place at the end.
fn in_tree_order(fleet: &FleetStore, mut results: Vec<NodeResult>) -> Vec<NodeResult> {
    let order = tree_order(fleet);
    results.sort_by_key(|r| order.get(&r.node).copied().unwrap_or(usize::MAX));
    results
}

/// Each node's position in the outline, list by list, depth first.
fn tree_order(fleet: &FleetStore) -> std::collections::HashMap<Uuid, usize> {
    let mut order = std::collections::HashMap::new();
    for list in fleet.list_outline_lists().unwrap_or_default() {
        for row in fleet.flatten_outline(list.id).unwrap_or_default() {
            let next = order.len();
            order.entry(row.node.id).or_insert(next);
        }
    }
    order
}

/// The evaluation sessions' settings, and the parallel-session cap.
fn driver_config(fleet: &FleetStore) -> Result<(ConversationConfig, usize), String> {
    let paths = TodPaths::discover().map_err(|e| format!("{e:#}"))?;
    let settings = TodSettings::load(&paths).unwrap_or_default();
    let media =
        tod_core::media::MediaPaths::discover().map_err(|e| format!("Media bundle: {e}"))?;
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
        } => format!(
            "{}: affects {affects} → back to {target}. {note}",
            result.title
        ),
        NodeOutcome::Verdict { note, .. } => format!("{}: not affected. {note}", result.title),
        NodeOutcome::Failed(err) => format!("{}: check failed — {err}", result.title),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{AppContext as _, TestAppContext};
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;

    /// A tick of the check runs git, Docker, and `tod-cli`, so it never runs
    /// on the main thread: the runner goes to the background executor, and
    /// meanwhile the UI still reads the check's progress from what it last
    /// knew of it.
    #[gpui::test]
    fn an_incoming_check_ticks_off_the_main_thread(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node = fixture.node_id;
        let store = fixture.store.clone();
        let check = cx.new(|cx| {
            let agent: SharedAgent = Arc::new(Mutex::new(Box::new(MockAgentProvider::new())));
            let lifecycle = cx.new(|_| LifecycleController::new(store.clone()));
            IncomingCheck::new(store.clone(), agent, lifecycle)
        });
        let config = ConversationConfig {
            data_root: fixture.store.paths().root().to_path_buf(),
            media: tod_core::media::MediaPaths::discover().expect("media paths"),
            launch: tod_agent::AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
            context: Default::default(),
        };
        check.update(cx, |check, cx| {
            check.run_with(IncomingRunner::new(config, 1, vec![node]), cx)
        });
        check.read_with(cx, |check, _| {
            assert!(check.is_running());
            assert_eq!(check.progress(), Some((0, 1)));
            assert!(check.covers(node));
        });

        // Away for a tick: the check still reads as running, where it was.
        let runner = check.update(cx, |check, _| check.take_runner());
        let runner = runner.expect("the runner was here");
        check.read_with(cx, |check, _| {
            assert!(check.is_running());
            assert_eq!(check.progress(), Some((0, 1)));
            assert!(check.covers(node), "the node is still being checked");
        });
        assert!(
            check.update(cx, |check, _| check.take_runner()).is_none(),
            "a tick already away is not started again"
        );
        check.update(cx, |check, cx| check.ticked(runner, false, cx));

        // The node has no pending changes, so the timer's first tick clears
        // it without an agent, on the background executor.
        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
        check.read_with(cx, |check, _| {
            assert!(!check.is_running(), "{:?}", check.error());
            assert_eq!(check.results().len(), 1);
            assert_eq!(check.results()[0].outcome, NodeOutcome::Cleared);
        });
    }
}
