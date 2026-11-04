//! A project's golden sandbox: the one every node of the repository forks,
//! carrying the toolchain, the checkout and tod's own files (a node's fork
//! gets tod's files again, because its proxy update redeploys it from its image)
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
    let files = install_files()?;
    let mine = my_stamp(&files)?;
    let name = match source_for(sandboxes, bx, &repo)? {
        golden::Source::Build { why } => {
            progress(&format!("{why}: making one from a fresh sandbox with the repository cloned into it"));
            return build_golden(sandboxes, bx, &repo, &files, &mine, progress);
        }
        golden::Source::Refresh { from } => from,
    };
    ensure_current_with(
        &repo,
        &name,
        &mine,
        |name| bx.get(name),
        progress,
        |name, progress| refresh_with(sandboxes, bx, &repo, name, &files, &mine, progress),
    )
}

/// Where `repo`'s golden comes from ([`golden::decide_source`]): its own
/// entry, or the account-wide one only when its checkout is `repo`'s (asked
/// of the sandbox itself, read-only), else nothing to refresh: one is built.
fn source_for(sandboxes: &Sandboxes, bx: &Blaxel, repo: &str) -> Result<golden::Source> {
    use tod_sandbox::config::GoldenSetting;
    let setting = sandboxes.account()?.golden_for(repo);
    // Resolved (a `prefix*` stands for its newest sandbox) only when there is one.
    let resolved = match setting {
        Some(_) => Some(sandboxes.resolve_golden(bx, repo)?),
        None => None,
    };
    let (own, legacy) = match setting {
        Some(GoldenSetting::Own(_)) => (resolved.as_deref(), None),
        Some(GoldenSetting::Legacy(_)) => (None, resolved.as_deref()),
        None => (None, None),
    };
    golden::decide_source(repo, own, legacy, &mut |name| checkout_origin(bx, name))
}

/// The repository (`owner/name`, lower case) the golden `name`'s checkout was
/// cloned from, read from the sandbox (a read-only `git remote get-url`); `None`
/// when it holds no checkout or one with no `origin`.
fn checkout_origin(bx: &Blaxel, name: &str) -> Result<Option<String>> {
    let info = bx.get(name)?.ok_or_else(|| anyhow!("the golden sandbox {name} does not exist"))?;
    let url = info.url.ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
    bx.wait_reachable(&url, tod_sandbox::blaxel::WAKE_CEILING).with_context(|| format!("wake {name} to read which repository it holds"))?;
    let probe = format!(
        "d={}; [ -d \"$d/.git\" ] && git -C \"$d\" remote get-url origin 2>/dev/null",
        tod_sandbox::node::WORKSPACE_PROBE
    );
    let out = golden::retry_step(
        name,
        "read its repository",
        &golden::StepRetry::default(),
        &mut std::thread::sleep,
        &mut |_| {},
        || bx.run(&url, &format!("sh -c {}", tod_sandbox::relay::shell_quote(&probe)), 60),
    )?;
    Ok(if out.exit_code == 0 { tod_store::cloud_nodes::normalize_repo(out.output().trim()) } else { None })
}

/// The decision of [`ensure_golden_current`], apart from Blaxel and the
/// files: `lookup` reads the golden, `refresh` makes the next one from it
/// (called only when the plan says to, never for a newer golden).
fn ensure_current_with(
    repo: &str,
    name: &str,
    mine: &GoldenStamp,
    lookup: impl FnOnce(&str) -> Result<Option<tod_sandbox::blaxel::SandboxInfo>>,
    progress: &mut dyn FnMut(&str),
    refresh: impl FnOnce(&str, &mut dyn FnMut(&str)) -> Result<String>,
) -> Result<String> {
    let info = lookup(name)?.ok_or_else(|| anyhow!("the golden sandbox {name} for {repo} does not exist"))?;
    let theirs = GoldenStamp::from_info(&info)?;
    match golden::decide(repo, mine, theirs.as_ref())? {
        Plan::Use => Ok(name.to_string()),
        Plan::Refresh { why } => {
            progress(&format!("the golden sandbox {name} for {repo} is being refreshed: {why}"));
            refresh(name, progress)
        }
        Plan::Refuse => unreachable!("decide turns a refusal into an error"),
    }
}

/// Makes `repo`'s golden again from its current one, whatever its stamp (but
/// never over a newer one: see [`ensure_golden_current`]; use that to refresh
/// only when needed). The steps: fork it, check what a fork needs, install
/// tod's files, label it with this build's stamp, mark it `ready`, point
/// `repo`'s golden at it, and delete the old one (see below). Returns its name.
/// A failure deletes the half-made sandbox (never the old golden), leaves
/// `sandboxes.toml` pointing at the old one, and says so.
pub fn refresh_golden(sandboxes: &mut Sandboxes, bx: &Blaxel, repo: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let repo = normalized(repo)?;
    let files = install_files()?;
    let mine = my_stamp(&files)?;
    let old = match source_for(sandboxes, bx, &repo)? {
        golden::Source::Build { why } => {
            progress(&format!("{why}: making one from a fresh sandbox with the repository cloned into it"));
            return build_golden(sandboxes, bx, &repo, &files, &mine, progress);
        }
        golden::Source::Refresh { from } => from,
    };
    let info = bx.get(&old)?.ok_or_else(|| anyhow!("the golden sandbox {old} for {repo} does not exist"))?;
    if let Some(theirs) = GoldenStamp::from_info(&info)? {
        // The same refusal `ensure_golden_current` makes: never refresh back.
        golden::decide(&repo, &mine, Some(&theirs))?;
    }
    refresh_with(sandboxes, bx, &repo, &old, &files, &mine, progress)
}

/// Makes the golden of a repository that has none (or whose only candidate
/// is another repository's), from the right source: a fresh sandbox from the
/// account's default image, with tod's dependencies and files installed and
/// the repository cloned into [`tod_sandbox::node::WORKSPACE_DIR`] through the
/// sandbox's proxy, which holds the user's GitHub token (so a private
/// repository clones) and initializes its submodules. Then the same labels,
/// `ready` last, and `sandboxes.toml` pointing at it. A failure deletes the
/// half-made sandbox and names why; it never falls back to another golden.
fn build_golden(
    sandboxes: &mut Sandboxes,
    bx: &Blaxel,
    repo: &str,
    files: &InstallFiles,
    mine: &GoldenStamp,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    use tod_sandbox::node;
    tod_sandbox::provision::check_relay_arch(&files.relay)?;
    // Before anything is made: a name that cannot be cloned is refused here.
    let script = golden::clone_script(repo)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let name = golden::new_name(repo, now);
    if bx.get(&name)?.is_some() {
        bail!("a sandbox named {name} already exists; try again in a moment");
    }
    let account = sandboxes.account()?.clone();
    let github = sandboxes.node_credentials(Vec::new()).github_token.filter(|t| !t.is_empty());
    if github.is_none() {
        progress("warning: no GitHub token is stored, so only a public repository can be cloned (Settings → Cloud sandboxes)");
    }
    let creds = node::NodeCredentials { github_token: github, ..Default::default() };
    let rules = node::credential_rules(&creds, account.claude_token_via);
    let proxy = if rules.is_empty() { placeholder_proxy() } else { node::proxy_spec(&rules) };
    let image = sandboxes.runtime_image("", false, progress)?;
    progress(&format!("creating {name} from {image}…"));
    let body = golden::build_body(&name, &image, &account.region, account.memory_mb, proxy);
    bx.create_from_body(&body).with_context(|| format!("create {name} from {image}"))?;
    let policy = golden::StepRetry::default();
    let said = std::cell::RefCell::new(progress);
    let say = |m: &str| (said.borrow_mut())(m);
    macro_rules! step {
        ($what:expr, $body:expr) => {
            golden::retry_step(&name, $what, &policy, &mut std::thread::sleep, &mut |m| say(m), || $body)
        };
    }
    let made = (|| -> Result<()> {
        let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
        let url = info.url.clone().ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        sandboxes.config.upsert(tod_sandbox::config::Sandbox { name: name.clone(), image: info.image, url: Some(url.clone()), agents: true });
        sandboxes.save()?;
        step!("wake it", bx.wait_reachable(&url, tod_sandbox::blaxel::WAKE_CEILING).with_context(|| format!("wake {name}")))?;
        // An image holds only what it was built with: tod's dependencies
        // first (git among them), then its own files.
        say("installing tod's dependencies…");
        sandboxes.ensure_as(bx, &name, tod_sandbox::provision::Mode::Install, &mut |m| say(m))?;
        let payload = node::NodePayload {
            relay: &files.relay,
            shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
            local_cli: files.local_cli.as_deref(),
            supervisor: files.supervisor.as_deref(),
            // Not used by `install`: the clone is made below.
            repo_url: "",
            branch: "",
            git_identity: &node::GitIdentity::fallback(),
            bundles: &files.bundles,
            supervisor_env: &[],
        };
        say("installing tod's files…");
        step!("install tod's files", node::install(bx, &url, &payload, &mut |m| say(m)))?;
        say(&format!("cloning {repo} into {}…", node::WORKSPACE_DIR));
        let cloned = bx.run(&url, &format!("sh -c {} 2>&1", tod_sandbox::relay::shell_quote(&script)), 900)?;
        if cloned.exit_code != 0 {
            bail!("cloning {repo} into {name} failed:\n{}", cloned.output().trim());
        }
        mark_ready(bx, &name, repo, mine, &policy, &say)
    })();
    if let Err(err) = made {
        // Nothing points at it yet; remove it unless it is somehow ready.
        let after = match bx.get(&name).and_then(|found| match golden::half_made_action(&name, "", found.as_ref()) {
            golden::HalfMade::Delete => bx.delete(&name).map(|()| format!("the half-made {name} was deleted")),
            golden::HalfMade::Keep(why) => Ok(format!("{name} was left as it is: {why}")),
        }) {
            Ok(text) => text,
            Err(cleanup) => format!("deleting the half-made {name} failed too ({cleanup:#}); delete it with `tod-sandbox delete {name}`"),
        };
        sandboxes.config.sandboxes.retain(|s| s.name != name);
        // Not hidden if saving the forgetting fails too: said with the rest.
        let saved = sandboxes.save().err().map(|e| format!(" (and sandboxes.toml could not be saved: {e:#})")).unwrap_or_default();
        return Err(err.context(format!("make the golden sandbox for {repo}: {name} was built but not finished; {after}{saved}. {repo} still has no golden")));
    }
    let account = sandboxes.config.blaxel.as_mut().ok_or_else(|| anyhow!("run `tod-sandbox setup` first"))?;
    account.set_golden(repo, &name);
    sandboxes.save()?;
    say(&format!("{name} is now the golden of {repo}"));
    Ok(name)
}

/// Labels the sandbox with this build's stamp, `tod-kind=golden` and the
/// repository it holds (`tod-repo`), marks it
/// `ready` last, and checks Blaxel says so.
fn mark_ready(
    bx: &Blaxel,
    name: &str,
    repo: &str,
    mine: &GoldenStamp,
    policy: &golden::StepRetry,
    say: &dyn Fn(&str),
) -> Result<()> {
    let run = |what: &str, f: &mut dyn FnMut() -> Result<Option<tod_sandbox::blaxel::SandboxInfo>>| {
        golden::retry_step(name, what, policy, &mut std::thread::sleep, &mut |m| say(m), f)
    };
    let stamp_labels = mine.labels();
    let mut labels: Vec<(&str, &str)> = stamp_labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
    labels.push(("tod-kind", golden::KIND_GOLDEN));
    let repo_label = golden::repo_label(repo)?;
    labels.push((golden::LABEL_REPO, repo_label.as_str()));
    run("label it", &mut || bx.set_labels(name, &labels).map(|()| None))?;
    run("mark it ready", &mut || bx.set_labels(name, &[(golden::LABEL_READY, "true")]).map(|()| None))?;
    match run("verify it", &mut || bx.get(name))? {
        Some(info) if info.label(golden::LABEL_READY) == Some("true") => Ok(()),
        Some(_) => bail!("{name} does not carry the ready label after it was set"),
        None => bail!("{name} is gone"),
    }
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
    let policy = golden::StepRetry::default();
    let said = std::cell::RefCell::new(progress);
    let say = |m: &str| (said.borrow_mut())(m);
    // Right after a deploy or a proxy update Blaxel answers "not ready" or
    // "currently not available" for a while: each step that can hit that is
    // run again (they are all safe to repeat), and failing for good names it.
    macro_rules! step {
        ($what:expr, $body:expr) => {
            golden::retry_step(&name, $what, &policy, &mut std::thread::sleep, &mut |m| say(m), || $body)
        };
    }
    let made = (|| -> Result<()> {
        let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
        let url = info.url.ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        step!("check what a fork needs", tod_sandbox::provision::require_fork_prerequisites(bx, &url, true, &mut |m| say(m)))?;
        // The proxy goes in BEFORE tod's files: updating a sandbox's network
        // configuration redeploys it from its image, and what was installed
        // into it until then is gone (seen live: a freshly refreshed golden had
        // no node manifest, relay or bundles, so every fork reinstalled them).
        //
        // A fork does not keep its source's proxy routing (checked live: the
        // fork's `network` comes back empty), and `fork_golden` refuses a
        // golden with none, since a fork of it cannot take the node's rules.
        // Give it a placeholder rule, as a hand-made golden has.
        step!("give it a proxy", bx.update_proxy(&name, &placeholder_proxy(), &[]))
            .with_context(|| format!("give {name} a proxy, so nodes can be forked from it"))?;
        let info = step!("wait until deployed", bx.wait_deployed(&name, Duration::from_secs(300)))?;
        let url = info.url.ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
        step!("wake it", bx.wait_reachable(&url, tod_sandbox::blaxel::WAKE_CEILING).with_context(|| format!("wake {name}")))?;
        let routed = step!("read its proxy", bx.get_json(&name))?.is_some_and(|j| has_proxy_routes(&j));
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
        say("installing tod's files…");
        step!("install tod's files", tod_sandbox::node::install(bx, &url, &payload, &mut |m| say(m)))?;
        // Stamp first and `ready` last: a half-made golden is never ready.
        mark_ready(bx, &name, repo, mine, &policy, &say)
    })();
    if let Err(err) = made {
        // `sandboxes.toml` still points at `old`, which this never touches.
        // A half-made golden must not stay for a later step to mistake for
        // one: remove it (`half_made_action` says whether it may be), and say
        // what happened to it.
        let after = match bx.get(&name).and_then(|found| match golden::half_made_action(&name, old, found.as_ref()) {
            golden::HalfMade::Delete => bx.delete(&name).map(|()| format!("the half-made {name} was deleted")),
            golden::HalfMade::Keep(why) => Ok(format!("{name} was left as it is: {why}")),
        }) {
            Ok(text) => text,
            Err(cleanup) => format!("deleting the half-made {name} failed too ({cleanup:#}); delete it with `tod-sandbox delete {name}`"),
        };
        return Err(err.context(format!(
            "refresh the golden sandbox for {repo}: {name} was made from {old} but not finished; {after}. {repo}'s golden is still {old}"
        )));
    }

    let account = sandboxes
        .config
        .blaxel
        .as_mut()
        .ok_or_else(|| anyhow!("run `tod-sandbox setup` first"))?;
    account.set_golden(repo, &name);
    // Whether anything else still points at the old one, as it stood.
    let wanted_elsewhere = account.golden_used_elsewhere(old, repo);
    sandboxes.save()?;
    say(&format!("{name} is now the golden of {repo}"));

    // Forks are independent of the sandbox they were made from, so nothing
    // needs the old one once nothing points at it. One tod did not make is
    // the user's to delete.
    let made_by_tod = bx.get(old)?.is_some_and(|i| i.label("tod-kind") == Some(golden::KIND_GOLDEN));
    if wanted_elsewhere {
        say(&format!("left {old} in place: another repository's golden setting still points at it"));
    } else if !made_by_tod {
        say(&format!("left {old} in place: tod did not make it (delete it yourself when you no longer need it)"));
    } else {
        say(&format!("deleting the old golden {old}…"));
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

    fn info_with(stamp: Option<&GoldenStamp>) -> tod_sandbox::blaxel::SandboxInfo {
        tod_sandbox::blaxel::SandboxInfo {
            name: "tod-golden-acme-widget-1".into(),
            status: "DEPLOYED".into(),
            state: None,
            url: None,
            image: String::new(),
            labels: stamp.map(|s| s.labels().into_iter().map(|(k, v)| (k.to_string(), v)).collect()).unwrap_or_default(),
            volumes: Vec::new(),
            node_env: Vec::new(),
        }
    }

    /// An older tod meeting a golden a newer one made: the error names the
    /// repository and both stamps, and nothing is made (the refresh is never
    /// called, so no sandbox is made and `sandboxes.toml` is not changed).
    #[test]
    fn an_older_tod_is_refused_a_newer_golden_naming_both_stamps_and_makes_nothing() {
        let mine = GoldenStamp { install: "mine-files".into(), schema: 93, wire: 1, built: 100 };
        let newer = GoldenStamp { install: "newer-files".into(), schema: 94, wire: 1, built: 200 };
        let info = info_with(Some(&newer));
        let mut said = Vec::new();
        let mut refreshed = false;
        let err = ensure_current_with(
            "acme/widget",
            "tod-golden-acme-widget-1",
            &mine,
            |_| Ok(Some(info)),
            &mut |m| said.push(m.to_string()),
            |_, _| {
                refreshed = true;
                Ok("never".into())
            },
        )
        .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("acme/widget") && text.contains("newer tod"), "{text}");
        assert!(text.contains("install newer-files, schema 94") && text.contains("install mine-files, schema 93"), "{text}");
        assert!(!refreshed, "a newer golden is never refreshed back");
        assert!(said.is_empty(), "nothing was said as if it were being made: {said:?}");
    }

    /// The other plans through the same seam: the same stamp is used as it is
    /// (no refresh); an older one, or one with no stamp, is refreshed, and the
    /// new golden's name comes back.
    #[test]
    fn the_same_golden_is_used_and_an_older_or_unstamped_one_is_refreshed() {
        let mine = GoldenStamp { install: "mine-files".into(), schema: 93, wire: 1, built: 100 };
        let run = |stamp: Option<GoldenStamp>| {
            let mut refreshed = Vec::new();
            let mut said = Vec::new();
            let out = ensure_current_with(
                "acme/widget",
                "old-name",
                &mine,
                |_| Ok(Some(info_with(stamp.as_ref()))),
                &mut |m| said.push(m.to_string()),
                |from, _| {
                    refreshed.push(from.to_string());
                    Ok("new-name".into())
                },
            );
            (out.unwrap(), refreshed, said)
        };
        assert_eq!(run(Some(mine.clone())), ("old-name".to_string(), vec![], vec![]));
        let older = GoldenStamp { install: "old-files".into(), schema: 92, wire: 1, built: 50 };
        let (name, refreshed, said) = run(Some(older));
        assert_eq!((name.as_str(), refreshed), ("new-name", vec!["old-name".to_string()]));
        assert!(said[0].contains("being refreshed") && said[0].contains("older tod"), "{said:?}");
        let (name, _, said) = run(None);
        assert_eq!(name, "new-name");
        assert!(said[0].contains("no tod-install label"), "{said:?}");
        // A golden that is gone is an error saying so, with nothing made.
        let err = ensure_current_with("acme/widget", "gone", &mine, |_| Ok(None), &mut |_| {}, |_, _| Ok("x".into())).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn a_repository_is_normalized_from_a_slug_or_a_url() {
        assert_eq!(normalized("Acme/Widget").unwrap(), "acme/widget");
        assert_eq!(normalized("https://github.com/acme/widget.git").unwrap(), "acme/widget");
        assert!(normalized("widget").is_err());
    }
}
