//! Deploying the workspace's orchestrator sandbox from the app
//! (`doc/cloud-sandboxes/orchestrator.md`): at launch, when the Blaxel
//! account is configured and no orchestrator sandbox exists, the app makes one
//! in the background, as `tod-sandbox orchestrator` does.
//!
//! An orchestrator that exists is never touched: replacing or updating one
//! (new binaries, a volume) stays an explicit `tod-sandbox orchestrator`, since
//! other users of the workspace share it.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tod_sandbox::orchestrator as orch;
use tod_store::fleet::sandbox::Sandboxes;

/// What [`ensure_orchestrator`] found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No Blaxel account is configured, or the URL is given explicitly.
    NotApplicable,
    /// A live orchestrator sandbox is already there; it was left alone.
    Present,
    /// A new orchestrator sandbox was deployed, at this URL.
    Deployed(String),
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// [`ensure_orchestrator`] on a thread of its own, once at a time. Errors are
/// logged (the daemon log), never shown as a failure of the app.
pub fn spawn_ensure(root: PathBuf) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("orchestrator-ensure".into()).spawn(move || {
        match ensure_orchestrator(&root, &mut |m| tracing::info!("orchestrator: {m}")) {
            Ok(Outcome::Deployed(url)) => tracing::info!("orchestrator deployed at {url}"),
            Ok(_) => {}
            Err(err) => tracing::warn!(
                "orchestrator is not deployed and the app could not deploy it: {err:#}. \
                 Run `tod-sandbox orchestrator` after `scripts/build-sandbox-binaries.sh`"
            ),
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
    if let Err(err) = spawned {
        tracing::warn!("orchestrator check did not start: {err}");
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Deploys the orchestrator sandbox when the account is configured and none is
/// there. Blocks on the network: never call it on the UI thread.
pub fn ensure_orchestrator(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<Outcome> {
    if std::env::var_os(super::ORCHESTRATOR_URL_ENV).is_some()
        || super::CloudSyncState::load(root).is_ok_and(|s| s.orchestrator_url.is_some())
    {
        return Ok(Outcome::NotApplicable);
    }
    // Not configured for Blaxel (or no token yet): nothing to do, quietly.
    let Ok(sandboxes) = Sandboxes::load(root) else { return Ok(Outcome::NotApplicable) };
    let Ok(acct) = sandboxes.account().cloned() else { return Ok(Outcome::NotApplicable) };
    let Ok(bx) = sandboxes.blaxel() else { return Ok(Outcome::NotApplicable) };
    if let Some(info) = bx.get(&acct.orchestrator).context("look for the orchestrator sandbox")?
        && !orch::is_dead(&info.status)
    {
        return Ok(Outcome::Present);
    }

    let dir = tod_store::fleet::sandbox::relay_path()
        .context("find the Linux sandbox binaries (scripts/build-sandbox-binaries.sh)")?
        .parent()
        .map(Path::to_path_buf)
        .context("the sandbox binaries' directory")?;
    let read = |name: &str| -> Result<Vec<u8>> {
        let path = dir.join(name);
        if !path.is_file() {
            bail!("{} not found; build it with scripts/build-sandbox-binaries.sh", path.display());
        }
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    };
    let orchestrator = read("tod-orchestrator")?;
    let tod_cli = read("tod-cli")?;
    let relay = read("tod-relay")?;
    let spec = orch::Spec {
        name: &acct.orchestrator,
        volume: acct.orchestrator_volume.as_deref().filter(|v| !v.is_empty()),
        move_data: false,
        backup: None,
        image: &acct.default_image,
        region: &acct.region,
        memory_mb: acct.memory_mb,
        orchestrator: &orchestrator,
        tod_cli: &tod_cli,
        relay: &relay,
        blaxel_workspace: bx.workspace(),
        blaxel_token: bx.token(),
    };
    progress(&format!("no orchestrator sandbox {} in the workspace; deploying it", acct.orchestrator));
    let url = orch::provision(&bx, &spec, progress)?;
    Ok(Outcome::Deployed(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-deploy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_data_root_without_a_blaxel_account_deploys_nothing() {
        let dir = scratch();
        let outcome = ensure_orchestrator(&dir, &mut |_| {}).unwrap();
        assert_eq!(outcome, Outcome::NotApplicable);
    }

    #[test]
    fn an_explicit_orchestrator_url_deploys_nothing() {
        let dir = scratch();
        let state = super::super::CloudSyncState {
            orchestrator_url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        };
        state.save(&dir).unwrap();
        let outcome = ensure_orchestrator(&dir, &mut |_| {}).unwrap();
        assert_eq!(outcome, Outcome::NotApplicable);
    }
}
