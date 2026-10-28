//! Running a node in the cloud (`tod_core::cloud_sync`), as the lifecycle
//! panel offers it: "Run in the cloud" and "Sync now" go to the background
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
    /// The node no longer runs in the cloud.
    Left(String),
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

/// Take `node_id` out of the cloud off the UI thread (`cloud_sync::lost::stop_running_in_cloud`).
pub fn stop_running<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    delete_sandbox: bool,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let _ = tx.send_blocking(match cloud_sync::lost::stop_running_in_cloud(&fleet, &node_id, delete_sandbox) {
            Ok(msg) => CloudUpdate::Left(msg),
            Err(err) => CloudUpdate::failed(describe("Stop running in the cloud failed", &err)),
        });
    });
}

/// Delete `node_id`'s cloud sandbox in Blaxel off the UI thread; the node stays a cloud node.
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

/// Run `node_id` in the cloud off the UI thread; `on_update` hears each step
/// and the result.
pub fn run_in_cloud<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let mut progress = |msg: &str| {
            cloud_sync::log_progress(&format!("running {node_id}"), msg);
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
                    "The cloud accepted the node, but its supervisor has not reported in after {} seconds.\n\
                     Sync now looks again. If it stays silent, check the log panel for the cloud's problems, \
                     or use Reinstall and restart.",
                    cloud_sync::SUPERVISOR_REPORTS_WITHIN.as_secs()
                ))
            }
            Err(err) => CloudUpdate::failed(describe("Run in the cloud failed", &err)),
        });
    });
}

/// Have `node_id`'s supervisor run its current step in a new session, off
/// the UI thread (`cloud_sync::wake_node_tracked_with`): the sandbox is not
/// touched beyond a request file and a poke, so nothing is reinstalled.
pub fn fresh_session<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let mut progress = |msg: &str| {
            cloud_sync::log_progress(&format!("new session for {node_id}"), msg);
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let _ = tx.send_blocking(CloudUpdate::Progress("Asking the supervisor for a new session…".into()));
        let result = cloud_sync::wake_node_tracked_with(&fleet, &root, &node_id, true, &mut progress);
        let _ = tx.send_blocking(match result {
            Ok((said, true)) => CloudUpdate::Synced(format!("Asked for a new session: {said}.")),
            Ok((said, false)) => CloudUpdate::failed(format!(
                "Asked for a new session ({said}), but its supervisor has not reported in after {} seconds.\n\
                 Sync now looks again. If it stays silent, check the log panel for the cloud's problems, \
                 or use Reinstall and restart.",
                cloud_sync::SUPERVISOR_REPORTS_WITHIN.as_secs()
            )),
            Err(err) => CloudUpdate::failed(describe("Run with a new session failed", &err)),
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
