//! Opening the store directly from a short-lived process (`tod-cli`, the
//! runner menu), the one place that refusal is explained.
//!
//! The store has one owner at a time (`tod.lock`): the daemon, or the app. A
//! client that finds the lock held must not fail with the bare lock error, and
//! must not carry on without the store: it is a client of the holder. So a
//! refused open looks for the daemon again (it may have been busy when first
//! asked) and, if it answers, opens the store through it. When nothing
//! answers, the error says who holds the lock, why the daemon could not be
//! reached, and what to start or restart.

use anyhow::{Result, anyhow};
use std::path::Path;
use std::time::{Duration, Instant};
use tod_agentd_client::Paths;
use tod_agentd_client::client as daemon_client;
use tod_store::fleet::{FleetLaunchError, FleetStore};

/// How long a refused open keeps looking for the daemon.
const LOOK_FOR: Duration = Duration::from_secs(5);

/// Open the store at `root` for this process, or reach it through the daemon
/// that holds it. `reattach` runs the launch-time reattach (see
/// [`FleetStore::open`]); without it, [`FleetStore::open_without_reattach`].
pub fn open(root: &Path, reattach: bool) -> Result<FleetStore> {
    open_waiting(root, reattach, LOOK_FOR)
}

pub fn open_waiting(root: &Path, reattach: bool, look_for: Duration) -> Result<FleetStore> {
    let direct = if reattach { FleetStore::open(root) } else { FleetStore::open_without_reattach(root) };
    let (path, holder) = match direct {
        Ok(store) => return Ok(store),
        Err(FleetLaunchError::StorageInUse { path, holder }) => (path, holder),
        Err(other) => return Err(anyhow!("{other}")),
    };
    let paths = Paths::new(root);
    let until = Instant::now() + look_for;
    loop {
        if daemon_client::connect(&paths).is_some() {
            return tod_agentd_client::remote::open_store(root, None)
                .map_err(|err| anyhow!("open the store through tod-agentd for {}: {err:#}", root.display()));
        }
        if Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(anyhow!(
        "the store at {} is held by another process ({path}{}), and tod-agentd does not answer for this data root: {}. \
         Restart tod-agentd for this data root (quit it, then run any tod-cli command or start the app), \
         or start the app, which serves tod-cli itself.",
        root.display(),
        tod_store::fleet::lock::held_by(&holder),
        daemon_client::why_unreachable(&paths),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_with_no_answering_daemon_says_what_to_start() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _holder = FleetStore::open(&root).unwrap();
        let err = match open_waiting(&root, false, Duration::from_millis(300)) {
            Ok(_) => panic!("the lock is held"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("held by another process"), "{err}");
        assert!(err.contains("tod-agentd does not answer"), "{err}");
        assert!(err.contains("agentd.json"), "{err}");
        assert!(err.contains("Restart tod-agentd"), "{err}");
    }

    #[test]
    fn a_free_lock_opens_directly() {
        let root = tod_test_util::scratch_dir(format!("direct-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        open_waiting(&root, false, Duration::from_millis(10)).unwrap();
    }
}
