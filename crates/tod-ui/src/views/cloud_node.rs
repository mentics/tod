//! Running a node under its supervisor in the cloud (`tod_core::cloud_sync`),
//! as the task panel's runner menu offers it: Start, Resume, Pause, Stop now,
//! Reset environment, Delete sandbox and Sync now go to the background
//! executor, and their progress comes back as [`CloudUpdate`]s.
//!
//! The jobs themselves are `tod_core::cloud_jobs`, which `tod-cli runner`
//! runs too; this only moves them off the UI thread and turns their results
//! into updates.

use gpui::Context;
use std::sync::Arc;
use tod_core::cloud_jobs::{self, JobResult};
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

    fn of(result: JobResult) -> Self {
        match result {
            Ok(msg) => CloudUpdate::Synced(msg),
            Err(msg) => CloudUpdate::failed(msg),
        }
    }
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
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::delete_sandbox(&fleet, &node_id)));
    });
}

/// Adopt the sandbox an older build started `node_id` in
/// (`cloud_sync::adopt::adopt_node_sandbox`) off the UI thread: it is checked
/// to be the node's and to answer, then recorded as its Files location.
pub fn adopt_sandbox<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let mut progress = |msg: &str| {
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::adopt_sandbox(&fleet, &node_id, &mut progress)));
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
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::delete_sandbox_anyway(&fleet, &node_id)));
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
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::stop_and_delete_sandbox(
            &fleet, &node_id, checkpoint, &mut progress,
        )));
    });
}

/// Submit work using the same sandbox and supervisor rules as the CLI.
pub fn submit_work<T: 'static>(
    fleet: Arc<FleetStore>,
    node_id: String,
    kind: Option<&'static str>,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let mut progress = |msg: &str| {
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let started = cloud_jobs::submit_work_tracked(&fleet, &root, &node_id, kind, &mut progress);
        if let Some(node) = started.accepted {
            let _ = tx.send_blocking(CloudUpdate::Accepted(node));
            if let Err(message) = started.result {
                let _ = tx.send_blocking(CloudUpdate::failed(message));
            }
        } else {
            let _ = tx.send_blocking(CloudUpdate::of(started.result));
        }
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
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::stop_request(&fleet, &root, &node_id, kind, action)));
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
        let _ = tx.send_blocking(CloudUpdate::Progress("Setting the environment up again…".into()));
        let mut progress = |msg: &str| {
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::reset_environment(&fleet, node, &mut progress)));
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
        let _ = tx.send_blocking(CloudUpdate::of(cloud_jobs::sync_now(&fleet, &root, node)));
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
