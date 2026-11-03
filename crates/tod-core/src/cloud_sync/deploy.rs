//! Deploying the workspace's orchestrator sandbox from the app
//! (`doc/cloud-sandboxes/orchestrator.md`): at launch, when the Blaxel
//! account is configured and no orchestrator sandbox exists, the app makes one
//! in the background, as `tod-sandbox orchestrator` does.
//!
//! An orchestrator that exists is judged by its version contract
//! (`tod_store::sync::contract`, `GET /health`): compatible, it is left alone;
//! older than this app (a lower wire or schema, or none reported), it is
//! updated in place (the same install `tod-sandbox orchestrator` does, with
//! its `/data` kept); newer than this app reads, it is refused with both
//! versions named. Nothing here looks at source hashes.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tod_sandbox::orchestrator as orch;
use tod_store::fleet::sandbox::Sandboxes;
use tod_store::sync::contract::{Contract, Verdict, judge, newer_message};

/// What [`ensure_orchestrator`] found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No Blaxel account is configured, or the URL is given explicitly.
    NotApplicable,
    /// A live orchestrator sandbox is already there and compatible; it was
    /// left alone.
    Present,
    /// The orchestrator was older than this app; it was updated in place, at
    /// this URL.
    Updated(String),
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
            Ok(Outcome::Updated(url)) => tracing::info!("orchestrator updated at {url}"),
            Ok(_) => {}
            Err(err) => tracing::error!(
                "the orchestrator could not be checked or deployed: {err:#}. To deploy it by hand \
                 run `tod-sandbox orchestrator` after `scripts/build-sandbox-binaries.sh`"
            ),
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
    if let Err(err) = spawned {
        tracing::warn!("orchestrator check did not start: {err}");
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// What to do with an orchestrator that reported `theirs`, from this app's
/// `mine`: `Ok(false)` leave it, `Ok(true)` update it, `Err` refuse (a newer
/// schema or wire this app cannot read).
pub fn assess(mine: &Contract, theirs: &Contract) -> Result<bool> {
    match judge(mine, theirs) {
        Verdict::Compatible => Ok(false),
        Verdict::Older => Ok(true),
        Verdict::Newer => bail!("{}", newer_message(mine, theirs)),
    }
}

/// Deploys the orchestrator sandbox when the account is configured and none is
/// there, and updates one that is older than this app. Blocks on the network:
/// never call it on the UI thread.
pub fn ensure_orchestrator(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<Outcome> {
    if std::env::var_os(super::ORCHESTRATOR_URL_ENV).is_some()
        || super::CloudSyncState::load(root).is_ok_and(|s| s.orchestrator_url.is_some())
    {
        return Ok(Outcome::NotApplicable);
    }
    // Not configured for Blaxel (or no token yet): nothing to do, quietly.
    let Ok(mut sandboxes) = Sandboxes::load(root) else { return Ok(Outcome::NotApplicable) };
    let Ok(acct) = sandboxes.account().cloned() else { return Ok(Outcome::NotApplicable) };
    let Ok(bx) = sandboxes.blaxel() else { return Ok(Outcome::NotApplicable) };
    if let Some(info) = bx.get(&acct.orchestrator).context("look for the orchestrator sandbox")?
        && !orch::is_dead(&info.status)
    {
        let url = sandboxes.url(&bx, &acct.orchestrator).context("find the orchestrator's URL")?;
        let base = format!("{}/port/{}", url.trim_end_matches('/'), orch::PORT);
        let theirs = super::HttpOrchestrator::new(base, Some(bx.token().to_string())).health()?;
        let mine = super::my_contract();
        if !assess(&mine, &theirs)? {
            return Ok(Outcome::Present);
        }
        progress(&format!(
            "the orchestrator ({}) is older than this app ({}); updating it",
            theirs.describe(),
            mine.describe()
        ));
        return install(&bx, &acct, progress).map(Outcome::Updated);
    }
    progress(&format!("no orchestrator sandbox {} in the workspace; deploying it", acct.orchestrator));
    install(&bx, &acct, progress).map(Outcome::Deployed)
}

/// Installs the Linux binaries (`scripts/build-sandbox-binaries.sh`) into the
/// orchestrator's sandbox, creating it when there is none, and restarts the
/// server; an existing sandbox keeps its `/data`.
fn install(
    bx: &tod_sandbox::blaxel::Blaxel,
    acct: &tod_sandbox::config::Account,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
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
    orch::provision(bx, &spec, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-deploy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn contract(wire: u32, schema: u32) -> Contract {
        Contract { wire, schema, build: "b".into() }
    }

    /// Serves `body` to every request on a loopback port, once per connection.
    fn serve(body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn a_compatible_orchestrator_is_left_alone() {
        let mine = crate::cloud_sync::my_contract();
        let mut theirs = mine.clone();
        theirs.build = "another build".into();
        assert!(!assess(&mine, &theirs).unwrap());
    }

    #[test]
    fn an_older_orchestrator_is_updated() {
        let mine = contract(2, 87);
        assert!(assess(&mine, &contract(1, 87)).unwrap());
        assert!(assess(&mine, &contract(2, 80)).unwrap());
    }

    #[test]
    fn a_newer_schema_is_refused_naming_both_versions() {
        let err = assess(&contract(1, 87), &contract(1, 99)).unwrap_err().to_string();
        assert!(err.contains("schema 99") && err.contains("schema 87"), "{err}");
    }

    #[test]
    fn an_orchestrator_without_a_version_in_health_is_wire_zero_and_updated() {
        let base = serve("ok\n");
        let theirs = super::super::HttpOrchestrator::new(base, None).health().unwrap();
        assert_eq!((theirs.wire, theirs.schema), (0, 0));
        assert!(assess(&crate::cloud_sync::my_contract(), &theirs).unwrap());
    }

    #[test]
    fn health_reads_the_contract_and_sync_refuses_a_newer_one() {
        use crate::cloud_sync::Orchestrator;
        let mine = crate::cloud_sync::my_contract();
        let compatible = super::super::HttpOrchestrator::new(serve(Box::leak(serde_json::to_string(&mine).unwrap().into_boxed_str())), None);
        assert_eq!(compatible.health().unwrap(), mine);
        compatible.check_contract().unwrap();

        let newer = Contract { schema: mine.schema + 1, ..mine.clone() };
        let refused = super::super::HttpOrchestrator::new(serve(Box::leak(serde_json::to_string(&newer).unwrap().into_boxed_str())), None);
        let err = format!("{:#}", refused.check_contract().unwrap_err());
        assert!(err.contains(&format!("schema {}", newer.schema)) && err.contains(&format!("schema {}", mine.schema)), "{err}");
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
