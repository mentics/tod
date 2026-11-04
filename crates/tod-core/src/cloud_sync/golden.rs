//! A project's golden sandbox: the one every node of the repository forks,
//! carrying tod's own files so a fork installs none
//! (`doc/runner-menu/design.md`, "The golden sandbox").
//!
//! [`ensure_golden_current`] is the entry a creator calls before any fork;
//! [`refresh_golden`] makes the next golden (`tod-sandbox golden refresh`).
//! The ordering rules are `tod_sandbox::golden` (pure); this module holds the
//! files and the network, and so lives here, above `tod-store`.

use super::refresh::{InstallFiles, install_files};
use anyhow::{Context, Result, anyhow, bail};
use std::time::Duration;
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::golden::{self, GoldenStamp, Plan};
use tod_store::fleet::sandbox::Sandboxes;

/// This build's stamp for `files`: the install, the sync contract, and the
/// commit time. A build with no commit time (made outside a git checkout)
/// cannot say which of two goldens is newer, so it is an error here, not 0.
pub fn my_stamp(files: &InstallFiles) -> Result<GoldenStamp> {
    let contract = super::my_contract();
    let built: u64 = crate::GIT_COMMIT_TIME.parse().map_err(|_| {
        anyhow!("this tod was built without a commit time (not from a git checkout), so it cannot tell a newer golden sandbox from an older one")
    })?;
    Ok(GoldenStamp { install: files.install().stamp, schema: contract.schema, wire: contract.wire, built })
}

/// The repository as owner/name, lower case, from a slug or a URL.
fn normalized(repo: &str) -> Result<String> {
    tod_store::cloud_nodes::normalize_repo(repo).ok_or_else(|| anyhow!("cannot read an owner and repository name from {repo:?}"))
}

/// The name of `repo`'s golden sandbox, current for this build: the same
/// stamp is used as it is; an older one (by schema, wire, then commit time),
/// or one of the same order but another stamp, is refreshed forward (the
/// user hears which, through `progress`) and the new one's name returned; a
/// **newer** one is refused with an error naming the repository and both
/// stamps (never refreshed back: that would break the newer tod's sandboxes).
/// Slow when it refreshes, and it changes `sandboxes.toml`: never on the UI
/// thread.
pub fn ensure_golden_current(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = normalized(repo)?;
    let name = sandboxes.resolve_golden(bx, &repo)?;
    let info = bx.get(&name)?.ok_or_else(|| anyhow!("the golden sandbox {name} for {repo} does not exist"))?;
    let theirs = GoldenStamp::from_info(&info)?;
    let files = install_files()?;
    let mine = my_stamp(&files)?;
    match golden::decide(&repo, &mine, theirs.as_ref())? {
        Plan::Use => Ok(name),
        Plan::Refresh { why } => {
            progress(&format!("the golden sandbox {name} for {repo} is being refreshed: {why}"));
            refresh_with(sandboxes, bx, &repo, &name, &files, &mine, progress)
        }
        Plan::Refuse => unreachable!("decide turns a refusal into an error"),
    }
}

/// Makes `repo`'s golden again from its current one, whatever its stamp (but
/// never over a newer one: see [`ensure_golden_current`]; use that to refresh
/// only when needed). The steps: fork it, check what a fork needs, install
/// tod's files, label it with this build's stamp, mark it `ready`, point
/// `repo`'s golden at it, and delete the old one (see below). Returns its name.
/// A failure leaves the half-made sandbox in place (it is not `ready`, and
/// nothing points at it) and names it.
pub fn refresh_golden(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = normalized(repo)?;
    let old = sandboxes.resolve_golden(bx, &repo)?;
    let info = bx.get(&old)?.ok_or_else(|| anyhow!("the golden sandbox {old} for {repo} does not exist"))?;
    let files = install_files()?;
    let mine = my_stamp(&files)?;
    if let Some(theirs) = GoldenStamp::from_info(&info)? {
        // The same refusal `ensure_golden_current` makes: never refresh back.
        golden::decide(&repo, &mine, Some(&theirs))?;
    }
    refresh_with(sandboxes, bx, &repo, &old, &files, &mine, progress)
}

fn refresh_with(
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    repo: &str,
    old: &str,
    files: &InstallFiles,
    mine: &GoldenStamp,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    tod_sandbox::provision::check_relay_arch(&files.relay)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let name = golden::new_name(repo, now);
    if bx.get(&name)?.is_some() {
        bail!("a sandbox named {name} already exists; try again in a moment");
    }
    progress(&format!("forking {old} to {name}…"));
    // A fork's proxy variables come back as an unrendered template; the
    // sandbox's local proxy is what works (as `node::fork_golden` does).
    let envs: Vec<(&str, String)> =
        tod_sandbox::provision::local_proxy_env().into_iter().map(|(k, v)| (k, v.to_string())).collect();
    bx.fork_with_envs(old, &name, &envs).with_context(|| format!("fork {old}"))?;
    let made = (|| -> Result<()> {
        let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
        let url = info.url.ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        tod_sandbox::provision::require_fork_prerequisites(bx, &url, true, progress)?;
        // The proxy goes in BEFORE tod's files: updating a sandbox's network
        // configuration redeploys it from its image, and what was installed
        // into it until then is gone (seen live: a freshly refreshed golden had
        // no node manifest, relay or bundles, so every fork reinstalled them).
        //
        // A fork does not keep its source's proxy routing (checked live: the
        // fork's `network` comes back empty), and `fork_golden` refuses a
        // golden with none, since a fork of it cannot take the node's rules.
        // Give it a placeholder rule, as a hand-made golden has.
        bx.update_proxy(&name, &placeholder_proxy(), &[])
            .with_context(|| format!("give {name} a proxy, so nodes can be forked from it"))?;
        let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
        let url = info.url.ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        bx.wait_reachable(&url, Duration::from_secs(180)).with_context(|| format!("wake {name}"))?;
        let routed = bx.get_json(&name)?.is_some_and(|j| has_proxy_routes(&j));
        if !routed {
            bail!("{name} did not keep the proxy it was given, so nodes cannot be forked from it");
        }
        let payload = tod_sandbox::node::NodePayload {
            relay: &files.relay,
            shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
            local_cli: files.local_cli.as_deref(),
            supervisor: files.supervisor.as_deref(),
            // Not used by `install`: a golden holds no node's checkout.
            repo_url: "",
            branch: "",
            git_identity: &tod_sandbox::node::GitIdentity::fallback(),
            bundles: &files.bundles,
            supervisor_env: &[],
        };
        progress("installing tod's files…");
        tod_sandbox::node::install(bx, &url, &payload, progress)?;
        // Stamp first and `ready` last: a half-made golden is never ready.
        let stamp_labels = mine.labels();
        let mut labels: Vec<(&str, &str)> = stamp_labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
        labels.push(("tod-kind", golden::KIND_GOLDEN));
        bx.set_labels(&name, &labels)?;
        bx.set_labels(&name, &[(golden::LABEL_READY, "true")])
    })();
    made.with_context(|| {
        format!("refresh the golden sandbox for {repo}: {name} was made from {old} but not finished (it is not ready, and nothing points at it; delete it with `tod-sandbox delete {name}`)")
    })?;

    let account = sandboxes
        .config
        .blaxel
        .as_mut()
        .ok_or_else(|| anyhow!("run `tod-sandbox setup` first"))?;
    account.set_golden(repo, &name);
    // Whether anything else still points at the old one, as it stood.
    let wanted_elsewhere = account.golden_used_elsewhere(old, repo);
    sandboxes.save()?;
    progress(&format!("{name} is now the golden of {repo}"));

    // Forks are independent of the sandbox they were made from, so nothing
    // needs the old one once nothing points at it. One tod did not make is
    // the user's to delete.
    let made_by_tod = bx.get(old)?.is_some_and(|i| i.label("tod-kind") == Some(golden::KIND_GOLDEN));
    if wanted_elsewhere {
        progress(&format!("left {old} in place: another repository's golden setting still points at it"));
    } else if !made_by_tod {
        progress(&format!("left {old} in place: tod did not make it (delete it yourself when you no longer need it)"));
    } else {
        progress(&format!("deleting the old golden {old}…"));
        bx.delete(old).with_context(|| format!("{name} is the golden now, but deleting the old golden {old} failed"))?;
    }
    Ok(name)
}

/// The proxy a golden carries so nodes can be forked from it: one rule that
/// matches nothing real. A fork replaces it with the node's own rules.
fn placeholder_proxy() -> serde_json::Value {
    tod_sandbox::node::proxy_spec(&[tod_sandbox::node::ProxyRule {
        destination: "tod-proxy-placeholder.invalid".into(),
        header: "X-Tod-Placeholder".into(),
        value: "{{SECRET:placeholder}}".into(),
        secret_name: "placeholder".into(),
        secret_value: "unused".into(),
    }])
}

/// Whether a sandbox's record has proxy routing, which `fork_golden` needs.
fn has_proxy_routes(record: &serde_json::Value) -> bool {
    record["spec"]["network"]["proxy"]["routing"].as_array().is_some_and(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live: a golden made by `golden refresh` (a fork) came out with no proxy
    /// routing, and every node fork from it was refused. What it is given
    /// must count as routing to `fork_golden`'s own check.
    #[test]
    fn the_placeholder_proxy_is_what_fork_golden_accepts() {
        let record = serde_json::json!({ "spec": { "network": { "proxy": placeholder_proxy() } } });
        assert!(has_proxy_routes(&record));
        assert!(!has_proxy_routes(&serde_json::json!({ "spec": { "network": {} } })));
    }

    #[test]
    fn a_repository_is_normalized_from_a_slug_or_a_url() {
        assert_eq!(normalized("Acme/Widget").unwrap(), "acme/widget");
        assert_eq!(normalized("https://github.com/acme/widget.git").unwrap(), "acme/widget");
        assert!(normalized("widget").is_err());
    }
}
