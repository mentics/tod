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
            Err(err) => CloudUpdate::Failed(format!("Stop running in the cloud failed: {err:#}")),
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
            let _ = tx.send_blocking(CloudUpdate::Progress(msg.to_string()));
        };
        let result = cloud_sync::run_in_cloud(&fleet, &root, &node_id, &mut progress);
        let _ = tx.send_blocking(match result {
            Ok(node) => CloudUpdate::Accepted(node),
            Err(err) => CloudUpdate::Failed(format!("Run in the cloud failed: {err:#}")),
        });
    });
}

/// Sync with the orchestrator off the UI thread.
pub fn sync_now<T: 'static>(
    fleet: Arc<FleetStore>,
    cx: &mut Context<T>,
    on_update: impl Fn(&mut T, CloudUpdate, &mut Context<T>) + 'static,
) {
    spawn(cx, on_update, move |tx| {
        let root = fleet.paths().root().to_path_buf();
        let _ = tx.send_blocking(match cloud_sync::sync_now(&fleet, &root) {
            Ok(report) => CloudUpdate::Synced(report.summary()),
            Err(err) => CloudUpdate::Failed(format!("Sync failed: {err:#}")),
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
