//! Giving the orchestrator the user's GitHub tokens (Settings →
//! Orchestrator GitHub access; `tod_store::orchestrator_github`).
//!
//! [`apply`] puts every token into the orchestrator sandbox's proxy, each
//! under its own secret name (so they never touch its disk or environment),
//! and sends the orchestrator which repositories each secret reads
//! (`tod_store::github_routes`; no token in it). The proxy matches only on
//! host, so the orchestrator picks the secret per request by sending its
//! placeholder. One caveat: the proxy's secrets are replaced as a whole, so
//! the orchestrator holds the tokens of whoever applied last; an
//! orchestrator shared by several users needs them to agree.

use anyhow::{Context, Result, bail};
use std::path::Path;
use tod_sandbox::node::{GITHUB_API_HOST, KeyedSecret, keyed_secrets_spec};
use tod_store::fleet::sandbox::Sandboxes;
use tod_store::orchestrator_github::{Config, secret_name, token};

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub tokens: usize,
    pub routes: usize,
}

/// The secrets for `config`, each with its stored token, or the first that
/// has none.
pub fn secrets(root: &Path, config: &Config) -> Result<Vec<KeyedSecret>> {
    config
        .tokens
        .iter()
        .map(|entry| {
            let value = token(root, &entry.id)
                .with_context(|| format!("{:?} has no token stored: enter it again", entry.label))?;
            Ok(KeyedSecret { name: secret_name(&entry.id), value })
        })
        .collect()
}

/// Sends the configured tokens to the orchestrator's proxy and its routes to
/// the orchestrator. Blocks on the network: never call it on the UI thread.
pub fn apply(root: &Path) -> Result<Applied> {
    let config = Config::load(root)?;
    config.check()?;
    let secrets = secrets(root, &config)?;
    let sandboxes = Sandboxes::load(root).context("the Blaxel account is not set up (tod-sandbox login)")?;
    let name = sandboxes.account()?.orchestrator.clone();
    let bx = sandboxes.blaxel()?;
    let current = bx
        .get_json(&name)?
        .with_context(|| format!("there is no orchestrator sandbox {name}: run `tod-sandbox orchestrator`"))?;
    if !tod_sandbox::node::has_proxy(&current) {
        bail!(
            "the orchestrator sandbox {name} was made without a proxy, which it cannot be given once it exists. \
             Run `tod-sandbox orchestrator --move-data` to recreate it with its /data kept"
        );
    }
    bx.update_proxy(&name, &keyed_secrets_spec(GITHUB_API_HOST, &secrets), &[])
        .context("give the orchestrator's proxy the tokens")?;
    let routes = config.routes();
    let (orchestrator, user) = crate::cloud_sync::resolve(root)?;
    orchestrator
        .put_github_routes(&user, &routes)
        .context("the tokens are in the orchestrator's proxy, but it was not told which repositories they read")?;
    Ok(Applied { tokens: secrets.len(), routes: routes.routes.len() })
}
