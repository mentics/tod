//! Running a node under its supervisor in the cloud (`tod_core::cloud_sync`),
//! as the task panel's runner menu offers it: Start, Resume, Pause, Stop now,
//! Reset environment, Delete sandbox and Sync now go to the background
//! executor, and their progress comes back as [`CloudUpdate`]s.

use gpui::Context;
use std::sync::Arc;
use tod_core::cloud_sync::{self, CloudNode};
use tod_store::fleet::FleetStore;

/// What a background cloud job reports.
#[derive(Debug, Clone)]
pub enum CloudUpdate {
    Progress(String),
    Accepted(CloudNode),
    Synced(String),
    Failed(String),
}

impl CloudUpdate {
    /// A failed job: shown in the panel and also recorded in the error log
    /// (the app's log panel reads ERROR from tracing), which the panel alone
    /// would never reach.
    pub fn failed(msg: String) -> Self {
        tracing::error!("cloud: {msg}");
        CloudUpdate::Failed(msg)
    }
}

/// A failure as the panel shows it: the title on the first line, then each
/// cause of the error on a line of its own.
fn describe(title: &str, err: &anyhow::Error) -> String {
    let mut out = title.to_string();
    for cause in err.chain() {
        out.push('\n');
        out.push_str(&cause.to_string());
    }
    out
}

/// Delete `node_id`'s sandbox in Blaxel off the UI thread, keeping its
/// uncommitted work first; the node's record of it is forgotten.
pub fn delete_sandbox<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let _ = tx.send_blocking(CloudUpdate::Progress("Deleting the sandbox…".into()));
        let _ = tx.send_blocking(match cloud_sync::lost::delete_node_sandbox(&fleet, &node_id) {
            Ok(msg) => CloudUpdate::Synced(msg),
            Err(err) => CloudUpdate::failed(describe("Delete sandbox failed", &err)),
        });
    });
}

/// [`delete_sandbox`] without the checkpoint first
/// (`cloud_sync::lost::delete_node_sandbox_anyway`): uncommitted work in the
/// sandbox is lost, so the caller offers it only beside the ordinary delete.
pub fn delete_sandbox_anyway<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let _ = tx.send_blocking(CloudUpdate::Progress("Deleting the sandbox without a checkpoint…".into()));
        let _ = tx.send_blocking(match cloud_sync::lost::delete_node_sandbox_anyway(&fleet, &node_id) {
            Ok(msg) => CloudUpdate::Synced(msg),
            Err(err) => CloudUpdate::failed(describe("Delete sandbox failed", &err)),
        });
    });
}

/// "Stop and delete": the node's runner is stopped first, then its sandbox
/// is deleted (`cloud_sync::lost::stop_and_delete_node_sandbox`), with the
/// checkpoint unless `checkpoint` is false. A run hosted in this app's own
/// process is stopped by the caller beforehand.
pub fn stop_and_delete_sandbox<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    checkpoint: bool,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let _ = tx.send_blocking(CloudUpdate::Progress("Stopping, then deleting the sandbox…".into()));
        let mut progress = |msg: &str| {
            cloud_sync::log_progress(&format!("stop and delete {node_id}"), msg);
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let result = cloud_sync::lost::stop_and_delete_node_sandbox(&fleet, &node_id, checkpoint, &mut progress);
        let _ = tx.send_blocking(match result {
            Ok(msg) => CloudUpdate::Synced(msg),
            Err(err) => CloudUpdate::failed(describe("Stop and delete failed", &err)),
        });
    });
}

/// Start `node_id` under its supervisor off the UI thread, making its sandbox
/// when it has none; `on_update` hears each step and the result.
pub fn run_in_cloud<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let mut progress = |msg: &str| {
            cloud_sync::log_progress(&format!("starting {node_id}"), msg);
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let result = cloud_sync::start_in_cloud(&fleet, &root, &node_id, &mut progress);
        let _ = tx.send_blocking(match result {
            Ok(started) if started.heard => CloudUpdate::Accepted(started.node),
            Ok(started) => {
                // Accepted, but nothing has come back: said, not left to look
                // like it is working.
                let _ = tx.send_blocking(CloudUpdate::Accepted(started.node));
                CloudUpdate::failed(format!(
                    "The node's sandbox accepted it, but its supervisor has not reported in after {} seconds.\n\
                     Sync now looks again. If it stays silent, check the log panel for the cloud's problems, \
                     or use Reset environment.",
                    cloud_sync::SUPERVISOR_REPORTS_WITHIN.as_secs()
                ))
            }
            Err(err) => CloudUpdate::failed(describe("Start failed", &err)),
        });
    });
}

/// What the user asked of a supervisor, in words, for the line: what was
/// asked for, and the menu entry's name for a failure's title.
fn request_words(kind: &str) -> (&'static str, &'static str) {
    match kind {
        cloud_sync::REQUEST_FRESH_SESSION => ("a new session", "Run with a new session"),
        cloud_sync::REQUEST_FRESH_BUDGET => ("a fresh budget", "Resume with a fresh budget"),
        cloud_sync::REQUEST_KEEP_GOING => ("it to go on", "Resume"),
        _ => ("this", "The request"),
    }
}

/// Record the user's request of `kind` for `node_id`'s supervisor (a new
/// session, a fresh budget, going on) and wake it, off the UI thread
/// (`cloud_sync::wake_node_tracked_requesting`): the sandbox is not touched
/// beyond a request file and a poke.
pub fn request<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    kind: &'static str,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    let (what, action) = request_words(kind);
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let mut progress = |msg: &str| {
            cloud_sync::log_progress(&format!("{kind} for {node_id}"), msg);
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let _ = tx.send_blocking(CloudUpdate::Progress(format!("Asking the supervisor for {what}…")));
        let result = cloud_sync::wake_node_tracked_requesting(&fleet, &root, &node_id, kind, &mut progress);
        let _ = tx.send_blocking(match result {
            Ok((said, true)) => CloudUpdate::Synced(format!("Asked for {what}: {said}.")),
            Ok((said, false)) => CloudUpdate::failed(format!(
                "Asked for {what} ({said}), but its supervisor has not reported in after {} seconds.\n\
                 Sync now looks again. If it stays silent, check the log panel for the cloud's problems, \
                 or use Reset environment.",
                cloud_sync::SUPERVISOR_REPORTS_WITHIN.as_secs()
            )),
            Err(err) => CloudUpdate::failed(describe(&format!("{action} failed"), &err)),
        });
    });
}

/// Ask the supervisor to stop once the current turn ends.
pub fn pause<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    stop_request(fleet, node_id, cloud_sync::REQUEST_PAUSE, "Pause", cx, on_update)
}

/// Ask the supervisor to cancel the turn in flight and stop.
pub fn stop_now<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    stop_request(fleet, node_id, cloud_sync::REQUEST_STOP_NOW, "Stop now", cx, on_update)
}

/// A `pause` or `stop_now`: recorded, synced and poked, and the click is
/// answered at once (`cloud_sync::wake_node_requesting`), not when the
/// supervisor reports back; the standing shows it stop.
fn stop_request<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    kind: &'static str,
    action: &'static str,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let _ = tx.send_blocking(CloudUpdate::Progress(format!("Asking the supervisor: {}…", action.to_lowercase())));
        let _ = tx.send_blocking(match cloud_sync::wake_node_requesting(&fleet, &root, &node_id, kind) {
            Ok(said) => CloudUpdate::Synced(format!("{action} asked of the supervisor: {said}.")),
            Err(err) => CloudUpdate::failed(describe(&format!("{action} failed"), &err)),
        });
    });
}

/// Set the node's environment up again, deleting nothing
/// (`tod_core::environment_reset::reset`), off the UI thread.
pub fn reset_environment<T: 'static>(
    fleet: Arc<FleetStore>,
    node: uuid::Uuid,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let mut progress = |msg: &str| {
            cloud_sync::log_progress(&format!("reset environment of {node}"), msg);
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let _ = tx.send_blocking(CloudUpdate::Progress("Setting the environment up again…".into()));
        let result = tod_core::environment_reset::reset(&fleet, node, &mut progress);
        let _ = tx.send_blocking(match result {
            Ok(()) => CloudUpdate::Synced("The environment was set up again.".into()),
            Err(err) => CloudUpdate::failed(describe("Reset environment failed", &err)),
        });
    });
}

/// Sync with the orchestrator off the UI thread.
///
/// With `node`, the result also says what the sync now shows of that node's
/// supervisor, so the click answers "is it working?", not only "did it sync?".
pub fn sync_now<T: 'static>(
    fleet: Arc<FleetStore>,
    node: Option<uuid::Uuid>,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let _ = tx.send_blocking(match cloud_sync::sync_now(&fleet, &root) {
            Ok(report) => {
                let mut msg = report.summary();
                if let Some(node) = node
                    && let Some(status) =
                        cloud_sync::cloud_status(&fleet, node, tod_store::outline::uuid_blob::now_ms())
                {
                    msg.push_str(". ");
                    msg.push_str(&status.describe());
                }
                CloudUpdate::Synced(msg)
            }
            Err(err) => CloudUpdate::failed(describe("Sync failed", &err)),
        });
    });
}

fn spawn<T: 'static>(
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
    job: impl FnOnce(async_channel::Sender<CloudUpdate>) + Send + 'static,
) {
    let (tx, rx) = async_channel::unbounded();
    cx.background_executor().spawn(async move { job(tx) }).detach();
    cx.spawn(async move |this, cx| {
        while let Ok(update) = rx.recv().await {
            if this.update(cx, |this, cx| on_update(this, update, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Asked for {what}" and "Asking the supervisor for {what}" must read as
    /// sentences for every request.
    #[test]
    fn request_words_read_in_the_asked_for_sentence() {
        for kind in [cloud_sync::REQUEST_FRESH_SESSION, cloud_sync::REQUEST_FRESH_BUDGET, cloud_sync::REQUEST_KEEP_GOING] {
            let (what, _) = request_words(kind);
            assert!(!what.starts_with("to "), "{kind}: Asked for {what}");
        }
    }
}
