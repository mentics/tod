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
    Failed(String),
}

/// The line shown instead of a cloud node's lifecycle buttons: where it
/// runs and what the supervisor last did, as far as the synced data says.
pub fn status_line(cloud: &CloudNode, lifecycle: &str) -> String {
    format!(
        "Runs in the cloud (sandbox {}, as {}); its supervisor moves it along. \
         Last synced state: {}.",
        cloud.sandbox,
        cloud.user,
        if lifecycle.is_empty() { "unknown" } else { lifecycle }
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_names_the_sandbox_and_state() {
        let cloud = CloudNode { sandbox: "node-x".into(), user: "joel".into(), accepted_at_ms: 0 };
        let line = status_line(&cloud, "active");
        assert!(line.contains("node-x") && line.contains("joel") && line.contains("active"), "{line}");
        assert!(status_line(&cloud, "").contains("unknown"));
    }
}
