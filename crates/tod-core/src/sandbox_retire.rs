//! Deleting a node's sandbox on its own when the node is done.
//!
//! A node whose Lifecycle config says "Delete sandbox" (on unless a node
//! turned it off, [`tod_store::lifecycle_config::resolve_delete_sandbox`])
//! loses its sandbox when it reaches the final phase
//! ([`FINAL_PHASE`], `done`). Nobody clicks anything: the daemon calls
//! [`retire_due`] when it starts (so a node that reached `done` while it was
//! down is caught up) and on a timer. The delete is the runner menu's own
//! ([`delete_node_sandbox`](lost::delete_node_sandbox): checkpoint the
//! uncommitted work, delete, forget the Files location), so there is one
//! deletion path.
//!
//! Whether a node is *already retired* needs no record of its own: the delete
//! forgets the node's Files location, the one record of its sandbox, and a
//! node with no location names no sandbox, so it is not due. Running twice
//! deletes once.
//!
//! A failure is never swallowed: it is logged, put on the node's problems and
//! the app's log, and the node is tried again on a bounded schedule
//! ([`BACKOFF`]); after the last attempt the report says it gave up and that
//! the task panel's Delete sandbox still works. A node whose setting is off
//! is never touched.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tod_store::fleet::FleetStore;
use tod_store::lifecycle_config::{self, FINAL_PHASE};

use crate::cloud_sync::lost;

/// Waits between attempts on a node whose delete failed. One entry per retry;
/// the attempts are the first try plus one per entry, then it stops (until the
/// daemon restarts).
pub const BACKOFF: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(5 * 60),
    std::time::Duration::from_secs(15 * 60),
    std::time::Duration::from_secs(60 * 60),
];

/// What the decision reads about one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The node's lifecycle state.
    pub lifecycle: String,
    /// What "Delete sandbox" resolves to for the node.
    pub delete_enabled: bool,
    /// The sandbox the node's Files location records, if any.
    pub sandbox: Option<String>,
    /// Why its runner is active, when it is.
    pub runner_blocker: Option<String>,
}

/// What to do with a node now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Delete its sandbox.
    Retire,
    /// Leave it, for this reason.
    Keep(Keep),
}

/// Why a node is left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    /// Not yet in the final phase.
    NotDone,
    /// The user turned "Delete sandbox" off for it.
    SettingOff,
    /// Its Files location names no sandbox (none made, or already retired).
    NoSandbox,
    /// Its runner is active; tried again when it ends.
    Running(String),
}

/// The pure decision.
pub fn decide(facts: &Facts) -> Decision {
    let final_rank = crate::task::model::lifecycle_rank(FINAL_PHASE);
    if crate::task::model::lifecycle_rank(&facts.lifecycle) < final_rank {
        return Decision::Keep(Keep::NotDone);
    }
    if !facts.delete_enabled {
        return Decision::Keep(Keep::SettingOff);
    }
    if facts.sandbox.is_none() {
        return Decision::Keep(Keep::NoSandbox);
    }
    if let Some(why) = &facts.runner_blocker {
        return Decision::Keep(Keep::Running(why.clone()));
    }
    Decision::Retire
}

/// A node whose sandbox should be deleted now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub node_id: String,
    pub title: String,
    pub sandbox: String,
}

/// The nodes whose sandbox should be deleted now: every Files location that
/// names a sandbox is read, and [`decide`] says [`Decision::Retire`].
pub fn due(fleet: &FleetStore) -> Result<Vec<Due>> {
    let located: Vec<tod_store::fleet::FilesLocation> = fleet
        .read(|c| tod_store::fleet::repos::files_location::FilesLocationRepo::new(c).list_all())?
        .into_iter()
        .filter(|l| l.sandbox().is_some())
        .collect();
    let mut out = Vec::new();
    for location in located {
        let node = location.node_id.clone();
        let Some(task) = fleet.get_node(&node)? else { continue };
        let mut facts = Facts {
            lifecycle: task.lifecycle.clone(),
            delete_enabled: true,
            sandbox: location.sandbox().map(str::to_string),
            runner_blocker: None,
        };
        // The cheap checks first: the setting and the runner are only read
        // for a node that is otherwise due.
        if decide(&facts) != Decision::Retire {
            continue;
        }
        let uuid = uuid::Uuid::parse_str(&node).with_context(|| format!("node id {node}"))?;
        facts.delete_enabled = fleet.read(|c| lifecycle_config::resolve_delete_sandbox(c, uuid))?.enabled;
        if decide(&facts) != Decision::Retire {
            continue;
        }
        facts.runner_blocker = lost::runner_blocker(fleet, &node)?;
        if decide(&facts) == Decision::Retire {
            out.push(Due { node_id: node, title: task.title, sandbox: facts.sandbox.unwrap_or_default() });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct Failing {
    /// Failed attempts so far.
    failures: usize,
    /// Not before this (ms since the epoch).
    next_at_ms: i64,
}

static FAILING: Mutex<Option<HashMap<String, Failing>>> = Mutex::new(None);

fn failing() -> std::sync::MutexGuard<'static, Option<HashMap<String, Failing>>> {
    FAILING.lock().unwrap_or_else(|e| e.into_inner())
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Retired {
    /// The delete's own message for each node whose sandbox went.
    pub deleted: Vec<String>,
    /// Nodes whose delete failed on this pass (reported).
    pub failed: usize,
}

/// Deletes the sandbox of every due node, through [`lost::delete_node_sandbox`].
/// Blocks on the network: never on the UI thread.
pub fn retire_due(fleet: &FleetStore) -> Result<Retired> {
    retire_due_with(fleet, tod_store::outline::uuid_blob::now_ms(), &mut |node| lost::delete_node_sandbox(fleet, node))
}

/// [`retire_due`] with the clock and the delete given.
pub fn retire_due_with(
    fleet: &FleetStore,
    now_ms: i64,
    delete: &mut dyn FnMut(&str) -> Result<String>,
) -> Result<Retired> {
    use tod_store::node_problems::{KIND_SANDBOX_RETIRE, NodeProblem, NodeProblemRepo};
    let mut report = Retired::default();
    let live: Vec<Due> = due(fleet)?;
    // A node that is no longer due (retired, setting turned off, moved back, its
    // sandbox deleted from the menu) starts afresh: its schedule and its
    // recorded problem go.
    if let Some(map) = failing().as_mut() {
        map.retain(|node, _| live.iter().any(|d| &d.node_id == node));
    }
    let recorded: Vec<NodeProblem> = fleet
        .read(|c| NodeProblemRepo::new(c).list_all())?
        .into_iter()
        .filter(|p| p.kind == KIND_SANDBOX_RETIRE)
        .collect();
    // The first failure to write the node's record is returned after the pass,
    // so one bad write does not leave the other nodes unattended.
    let mut write_error: Option<anyhow::Error> = None;
    let mut note = |result: Result<()>| {
        if let Err(err) = result {
            write_error.get_or_insert(err);
        }
    };
    for stale in recorded.iter().filter(|p| !live.iter().any(|d| d.node_id == p.node_id.to_string())) {
        note(crate::node_problems::clear(fleet, &stale.node_id.to_string(), KIND_SANDBOX_RETIRE));
    }
    for item in live {
        let state = failing().get_or_insert_with(Default::default).get(&item.node_id).copied();
        if let Some(state) = state {
            if state.failures > BACKOFF.len() || now_ms < state.next_at_ms {
                continue;
            }
        }
        match delete(&item.node_id) {
            Ok(message) => {
                failing().get_or_insert_with(Default::default).remove(&item.node_id);
                tracing::info!("sandbox retire: {} reached {FINAL_PHASE}: {message}", item.title);
                report.deleted.push(message);
                note(crate::node_problems::clear(fleet, &item.node_id, KIND_SANDBOX_RETIRE));
            }
            Err(err) => {
                report.failed += 1;
                let failures = state.map_or(0, |s| s.failures) + 1;
                let retry = BACKOFF.get(failures - 1);
                let next_at_ms = retry.map_or(i64::MAX, |d| now_ms.saturating_add(d.as_millis() as i64));
                failing()
                    .get_or_insert_with(Default::default)
                    .insert(item.node_id.clone(), Failing { failures, next_at_ms });
                let next = match retry {
                    Some(d) => format!("It is tried again in {} minute(s).", d.as_secs() / 60),
                    None => "It was tried several times and is not tried again until the daemon restarts; \
                             Delete sandbox in the task panel's menu still works."
                        .to_string(),
                };
                let message = format!(
                    "Deleting the sandbox {} of {} (it reached {FINAL_PHASE}) failed: {err:#}. {next}",
                    item.sandbox, item.title
                );
                crate::logging::buffer::record(
                    crate::logging::buffer::Level3::Error,
                    crate::logging::buffer::LogSource::App,
                    message.clone(),
                );
                // The node's own record: durable, shown in its task panel, and
                // counted by `next_actor` (see there for how a `done` node reads).
                match uuid::Uuid::parse_str(&item.node_id) {
                    Ok(node) => {
                        let first_at = recorded.iter().find(|p| p.node_id == node).map_or(now_ms, |p| p.first_at);
                        note(crate::node_problems::record(
                            fleet,
                            NodeProblem {
                                node_id: node,
                                kind: KIND_SANDBOX_RETIRE.to_string(),
                                message,
                                attempts: failures as u32,
                                max_attempts: Some(BACKOFF.len() as u32 + 1),
                                next_retry_at: retry.map(|_| next_at_ms),
                                gave_up: retry.is_none(),
                                first_at,
                                updated_at: now_ms,
                            },
                        ));
                    }
                    Err(e) => note(Err(anyhow::anyhow!("node id {}: {e}", item.node_id))),
                }
            }
        }
    }
    match write_error {
        Some(err) => Err(err),
        None => Ok(report),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::FleetMutation;
    use tod_store::outline::OutlineMutation;
    use uuid::Uuid;

    fn facts() -> Facts {
        Facts { lifecycle: "done".into(), delete_enabled: true, sandbox: Some("s".into()), runner_blocker: None }
    }

    #[test]
    fn the_decision_table() {
        assert_eq!(decide(&facts()), Decision::Retire);
        for state in ["proposed", "active", "review", "pr_review", "approved", "merged", "released", "learn"] {
            let f = Facts { lifecycle: state.into(), ..facts() };
            assert_eq!(decide(&f), Decision::Keep(Keep::NotDone), "{state}");
        }
        assert_eq!(decide(&Facts { delete_enabled: false, ..facts() }), Decision::Keep(Keep::SettingOff));
        assert_eq!(decide(&Facts { sandbox: None, ..facts() }), Decision::Keep(Keep::NoSandbox));
        let busy = Facts { runner_blocker: Some("its runner is active".into()), ..facts() };
        assert!(matches!(decide(&busy), Decision::Keep(Keep::Running(_))));
        // The setting off wins over a running node: it never runs.
        let both = Facts { delete_enabled: false, ..busy };
        assert_eq!(decide(&both), Decision::Keep(Keep::SettingOff));
    }

    struct Fx {
        root: std::path::PathBuf,
        fleet: FleetStore,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fx() -> Fx {
        let root = std::env::temp_dir().join(format!("tod-retire-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
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
        let node = fleet.flatten_outline(list_id).unwrap()[0].node.id;
        Fx { root, fleet, node }
    }

    impl Fx {
        fn id(&self) -> String {
            self.node.to_string()
        }

        fn locate(&self, sandbox: &str) {
            self.fleet
                .enqueue(FleetMutation::RecordFilesLocation {
                    location: tod_store::fleet::FilesLocation {
                        node_id: self.id(),
                        source_node_id: self.id(),
                        recipe: String::new(),
                        repo: None,
                        container: None,
                        worktree_path: None,
                        worktree_lease_id: None,
                        worktree_lease_holder: None,
                        sandbox: Some(sandbox.into()),
                        created_at: 0,
                    },
                })
                .unwrap();
            self.fleet.writer().flush().unwrap();
        }

        fn set_lifecycle(&self, state: &str) {
            self.fleet
                .enqueue(FleetMutation::UpdateTaskLifecycle { id: self.id(), lifecycle: state.into() })
                .unwrap();
            self.fleet.writer().flush().unwrap();
            self.fleet.reload_if_stale().unwrap();
        }

        fn delete_setting(&self, value: Option<bool>) {
            self.fleet
                .enqueue_outline(OutlineMutation::EnableCapabilities {
                    node_id: self.node,
                    capabilities: vec![tod_store::outline::Capability::LifecycleConfig],
                })
                .unwrap();
            self.fleet.writer().flush().unwrap();
            self.fleet
                .enqueue_outline(OutlineMutation::SetNodeLifecycleDeleteSandbox {
                    node_id: self.node,
                    delete_sandbox: value,
                })
                .unwrap();
            self.fleet.writer().flush().unwrap();
            self.fleet.reload_if_stale().unwrap();
        }

        /// The node's recorded sandbox-retire problem, if any.
        fn problem(&self) -> Option<tod_store::node_problems::NodeProblem> {
            self.fleet.reload_if_stale().unwrap();
            crate::node_problems::for_node(&self.fleet, self.node)
                .unwrap()
                .into_iter()
                .find(|p| p.kind == tod_store::node_problems::KIND_SANDBOX_RETIRE)
        }

        fn has_location(&self) -> bool {
            tod_store::fleet::provision::node_location(&self.fleet, &self.id()).unwrap().is_some()
        }

        /// Like the real delete: the location is forgotten.
        fn fake_delete(&self, calls: &std::cell::RefCell<Vec<String>>, node: &str) -> Result<String> {
            calls.borrow_mut().push(node.to_string());
            self.fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node.to_string() })?;
            self.fleet.writer().flush()?;
            self.fleet.reload_if_stale()?;
            Ok("deleted".into())
        }
    }

    #[test]
    fn a_done_node_is_retired_once_and_never_again() {
        let fx = fx();
        fx.locate("sb-1");
        fx.set_lifecycle("done");
        let calls = std::cell::RefCell::new(Vec::new());
        let first = retire_due_with(&fx.fleet, 1_000, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert_eq!(first.deleted, ["deleted"]);
        assert!(!fx.has_location());
        let second = retire_due_with(&fx.fleet, 2_000, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert!(second.deleted.is_empty() && second.failed == 0);
        assert_eq!(calls.borrow().len(), 1, "deleted once");
    }

    #[test]
    fn a_node_before_done_is_left() {
        let fx = fx();
        fx.locate("sb-2");
        fx.set_lifecycle("review");
        let calls = std::cell::RefCell::new(Vec::new());
        retire_due_with(&fx.fleet, 1, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert!(calls.borrow().is_empty());
        assert!(fx.has_location());
    }

    #[test]
    fn a_done_node_without_a_sandbox_is_left() {
        let fx = fx();
        fx.set_lifecycle("done");
        let calls = std::cell::RefCell::new(Vec::new());
        retire_due_with(&fx.fleet, 1, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn the_setting_off_never_deletes() {
        let fx = fx();
        fx.locate("sb-3");
        fx.set_lifecycle("done");
        fx.delete_setting(Some(false));
        let calls = std::cell::RefCell::new(Vec::new());
        retire_due_with(&fx.fleet, 1, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert!(calls.borrow().is_empty() && fx.has_location());
        // Turned on again, it is caught up.
        fx.delete_setting(None);
        retire_due_with(&fx.fleet, 2, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn a_running_node_is_skipped_until_its_runner_ends() {
        let fx = fx();
        fx.locate("sb-4");
        fx.set_lifecycle("done");
        let now = tod_store::outline::uuid_blob::now_ms();
        let lease = Uuid::new_v4();
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        tod_store::activity::ActivityRepo::new(&conn)
            .start(lease, fx.node, tod_store::activity::KIND_RUN, "t", now)
            .unwrap();
        let calls = std::cell::RefCell::new(Vec::new());
        retire_due_with(&fx.fleet, 1, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert!(calls.borrow().is_empty(), "a live runner defers the delete");
        tod_store::activity::ActivityRepo::new(&conn).end(lease).unwrap();
        retire_due_with(&fx.fleet, 2, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn a_failed_delete_is_reported_and_retried_on_a_schedule() {
        let fx = fx();
        fx.locate("sb-5");
        fx.set_lifecycle("done");
        let attempts = std::cell::Cell::new(0);
        let mut fail = |_: &str| -> Result<String> {
            attempts.set(attempts.get() + 1);
            anyhow::bail!("blaxel said no")
        };
        let r = retire_due_with(&fx.fleet, 0, &mut fail).unwrap();
        assert_eq!(r.failed, 1);
        let problem = fx.problem().expect("the failure is recorded on the node");
        assert!(problem.message.contains("blaxel said no") && problem.message.contains("tried again"), "{problem:?}");
        assert_eq!((problem.attempts, problem.max_attempts), (1, Some(BACKOFF.len() as u32 + 1)));
        assert_eq!(problem.next_retry_at, Some(BACKOFF[0].as_millis() as i64));
        assert!(!problem.gave_up);
        assert!(fx.has_location(), "the location stays for the retry");
        // Too early: not tried.
        retire_due_with(&fx.fleet, 1_000, &mut fail).unwrap();
        assert_eq!(attempts.get(), 1);
        // After each back-off: tried again, to the last attempt.
        let mut now = 0;
        for (i, wait) in BACKOFF.iter().enumerate() {
            now += wait.as_millis() as i64;
            retire_due_with(&fx.fleet, now, &mut fail).unwrap();
            assert_eq!(fx.problem().unwrap().attempts as usize, i + 2, "the count follows the attempts");
        }
        assert_eq!(attempts.get(), 1 + BACKOFF.len());
        let last = fx.problem().unwrap();
        assert!(last.gave_up && last.next_retry_at.is_none(), "{last:?}");
        assert!(last.message.contains("not tried again") && last.message.contains("Delete sandbox"), "{last:?}");
        assert_eq!(last.first_at, 0, "when it first failed is kept");
        // Then it stops.
        retire_due_with(&fx.fleet, now + 10_000_000_000, &mut fail).unwrap();
        assert_eq!(attempts.get(), 1 + BACKOFF.len());
    }

    #[test]
    fn a_later_success_clears_the_problem_and_so_does_the_sandbox_going_another_way() {
        let fx = fx();
        fx.locate("sb-6");
        fx.set_lifecycle("done");
        let mut fail = |_: &str| -> Result<String> { anyhow::bail!("blaxel said no") };
        retire_due_with(&fx.fleet, 0, &mut fail).unwrap();
        assert!(fx.problem().is_some());
        // Tried again after the first back-off: it works now.
        let calls = std::cell::RefCell::new(Vec::new());
        let at = BACKOFF[0].as_millis() as i64;
        retire_due_with(&fx.fleet, at, &mut |n| fx.fake_delete(&calls, n)).unwrap();
        assert!(fx.problem().is_none(), "a successful delete clears it");

        // Failing again, then the user deletes it from the menu (the location goes):
        // the next pass finds nothing due and clears the record.
        let fx = self::fx();
        fx.locate("sb-7");
        fx.set_lifecycle("done");
        retire_due_with(&fx.fleet, 0, &mut fail).unwrap();
        assert!(fx.problem().is_some());
        fx.fake_delete(&calls, &fx.id()).unwrap();
        retire_due_with(&fx.fleet, 1, &mut fail).unwrap();
        assert!(fx.problem().is_none(), "no longer due: nothing left to report");
    }

    /// The real `retire_due` (no fake) deletes through the very function the
    /// task panel's Delete sandbox uses, `lost::delete_node_sandbox`: with no
    /// Blaxel account in this root both fail the same way, and the retire
    /// report and problem carry the menu path's own error.
    #[test]
    fn the_real_retire_goes_through_the_menus_delete() {
        let fx = fx();
        fx.locate("sb-real");
        fx.set_lifecycle("done");
        let id = fx.id();
        let menu_error = format!("{:#}", lost::delete_node_sandbox(&fx.fleet, &id).unwrap_err());
        assert!(!menu_error.is_empty());
        let report = retire_due(&fx.fleet).unwrap();
        assert_eq!((report.deleted.len(), report.failed), (0, 1), "{report:?}");
        let problem = fx.problem().expect("recorded on the node");
        assert!(problem.message.contains(&menu_error), "{menu_error:?} in {problem:?}");
        assert!(fx.has_location(), "a failed delete forgets nothing");
    }
}
