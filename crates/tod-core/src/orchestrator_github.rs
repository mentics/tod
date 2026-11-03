//! Giving the orchestrator the user's GitHub tokens (Settings →
//! Orchestrator GitHub access; `tod_store::orchestrator_github`).
//!
//! [`apply`] puts every token into the orchestrator sandbox's proxy, each
//! under its own secret name (so they never touch its disk or environment),
//! and sends the orchestrator which repositories each secret reads
//! (`tod_store::github_routes`; no token in it). The proxy matches only on
//! host, so the orchestrator picks the secret per request by sending its
//! placeholder. The proxy's secrets are replaced as a whole and their values
//! are never readable, so a root cannot merge: it can only refuse to drop a
//! secret it holds no token for. Before replacing, [`apply`] reads which
//! secret names the orchestrator holds (what the proxy's `GET` shows of its
//! rules, plus the names the last apply recorded in the orchestrator,
//! `GET /github-secrets`), and refuses with [`Refused`] when the replacement
//! would remove one, unless told to (`remove_others`).

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
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

/// Why [`apply`] did nothing. Downcast the `anyhow::Error` to this.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The orchestrator holds these secrets, this root has no token for them.
    #[error(
        "the orchestrator also holds tokens {} that this root does not have; applying would remove them",
        .0.join(", ")
    )]
    WouldRemove(Vec<String>),
    /// Nothing says which secrets the orchestrator holds (an orchestrator
    /// older than `/github-secrets`, or a reset disk), so a replacement might
    /// drop another root's.
    #[error(
        "the orchestrator does not say which tokens it holds (redeploy it so it can); \
         applying would replace whatever it holds"
    )]
    HoldingsUnknown,
}

/// The names in `held` that `new` does not have, sorted. Pure.
pub fn removed_names(held: &BTreeSet<String>, new: &[String]) -> Vec<String> {
    held.iter().filter(|n| !new.contains(n)).cloned().collect()
}

/// Whether replacing `held` (None: unknown) with `new` may go ahead.
pub fn check_removal(held: Option<&BTreeSet<String>>, new: &[String], remove_others: bool) -> Result<(), Refused> {
    if remove_others {
        return Ok(());
    }
    let Some(held) = held else { return Err(Refused::HoldingsUnknown) };
    let removed = removed_names(held, new);
    if removed.is_empty() { Ok(()) } else { Err(Refused::WouldRemove(removed)) }
}

/// The secret names a sandbox's proxy rules show (`GET` shows the keys, never
/// the values), without the placeholder kept so the proxy exists.
pub fn proxy_secret_names(sandbox: &serde_json::Value) -> BTreeSet<String> {
    sandbox["spec"]["network"]["proxy"]["routing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rule| rule["secrets"].as_object())
        .flat_map(|m| m.keys().cloned())
        .filter(|n| n != tod_sandbox::node::UNUSED_SECRET)
        .collect()
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
/// the orchestrator. Refuses ([`Refused`]) rather than drop a secret this root
/// has no token for, unless `remove_others`. Blocks on the network: never call
/// it on the UI thread.
pub fn apply(root: &Path, remove_others: bool) -> Result<Applied> {
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
    let (orchestrator, user) = crate::cloud_sync::resolve(root)?;
    let mut held = proxy_secret_names(&current);
    let recorded = orchestrator.github_secrets().context("ask the orchestrator which tokens it holds")?;
    let known = recorded.is_some() || !held.is_empty();
    held.extend(recorded.into_iter().flatten());
    let names: Vec<String> = secrets.iter().map(|s| s.name.clone()).collect();
    check_removal(known.then_some(&held), &names, remove_others)?;
    bx.update_proxy(&name, &keyed_secrets_spec(GITHUB_API_HOST, &secrets), &[])
        .context("give the orchestrator's proxy the tokens")?;
    orchestrator
        .put_github_secrets(&names)
        .context("the tokens are in the orchestrator's proxy, but it could not record their names")?;
    let routes = config.routes();
    orchestrator
        .put_github_routes(&user, &routes)
        .context("the tokens are in the orchestrator's proxy, but it was not told which repositories they read")?;
    Ok(Applied { tokens: secrets.len(), routes: routes.routes.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }
    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn removed_names_are_the_held_ones_the_new_list_lacks() {
        assert_eq!(removed_names(&set(&["gh_a", "gh_b", "gh_c"]), &v(&["gh_b", "gh_d"])), v(&["gh_a", "gh_c"]));
        assert!(removed_names(&set(&["gh_a"]), &v(&["gh_a", "gh_b"])).is_empty());
    }

    #[test]
    fn a_replacement_that_drops_others_is_refused_unless_told() {
        let held = set(&["gh_a", "gh_b"]);
        let err = check_removal(Some(&held), &v(&["gh_a"]), false).unwrap_err();
        assert_eq!(err, Refused::WouldRemove(v(&["gh_b"])));
        assert!(err.to_string().contains("also holds tokens gh_b"));
        assert!(check_removal(Some(&held), &v(&["gh_a"]), true).is_ok());
        // A name this root has a token for is simply updated.
        assert!(check_removal(Some(&held), &v(&["gh_a", "gh_b"]), false).is_ok());
        assert!(check_removal(Some(&set(&[])), &v(&["gh_a"]), false).is_ok());
    }

    #[test]
    fn unknown_holdings_are_refused_unless_told() {
        assert_eq!(check_removal(None, &v(&["gh_a"]), false), Err(Refused::HoldingsUnknown));
        assert!(check_removal(None, &v(&["gh_a"]), true).is_ok());
    }

    #[test]
    fn the_proxys_secret_names_skip_the_placeholder() {
        let sandbox = serde_json::json!({ "spec": { "network": { "proxy": { "routing": [
            { "secrets": { "gh_a": "****", "tod_unused": "x" } }, { "destinations": ["h"] }
        ] } } } });
        assert_eq!(proxy_secret_names(&sandbox), set(&["gh_a"]));
        assert!(proxy_secret_names(&serde_json::json!({})).is_empty());
    }
}
