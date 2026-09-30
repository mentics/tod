//! An autonomous node's own sandbox: created with proxy rules that inject the
//! user's credentials, then given the node's branch, the relay, the HTTP
//! `tod-cli` shim, and the supervisor.
//!
//! This crate is a leaf: the caller reads the user's `CredentialStore` and
//! passes the values in ([`NodeCredentials`]); they go only into the
//! sandbox's proxy rules, never into its environment or files. The one
//! exception is the Claude subscription token with
//! [`ClaudeTokenVia::Env`], which goes into the supervisor's environment
//! alone ([`supervisor_env`]).
//!
//! Design: `doc/cloud-sandboxes/autonomous-nodes.md` (Credentials).

use crate::blaxel::{Blaxel, RELAY_PORT};
pub use crate::config::{ClaudeTokenVia, SchedulerKind};
use crate::provision::{MANIFEST_PATH, RELAY_PATH, RELAY_PROCESS, TOD_CLI_PATH, TOD_DIR, node_manifest};
use crate::relay::shell_quote;
use anyhow::{Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::path::Path;

/// Where the supervisor is installed in a node's sandbox.
pub const SUPERVISOR_PATH: &str = "/opt/tod/tod-supervisor";
/// The supervisor's name in the sandbox's process API.
pub const SUPERVISOR_PROCESS: &str = "tod-supervisor";
/// Where the node's repository is checked out.
pub const WORKSPACE_DIR: &str = "/workspace/repo";
/// What `gh` sees as its token; the proxy replaces the header it sends.
pub const GH_TOKEN_PLACEHOLDER: &str = "placeholder-injected-by-proxy";
/// Set to [`GITHUB_AUTH_PROXY`] in a node's sandbox whose proxy injects the
/// user's GitHub token: tod's own GitHub client (`tod_store::github`) then
/// sends no token of its own, through the proxy, trusting its CA.
pub const GITHUB_AUTH_ENV: &str = "TOD_GITHUB_AUTH";
pub const GITHUB_AUTH_PROXY: &str = "proxy";
/// Where the real `tod-cli` is installed in a node's sandbox, beside the
/// HTTP shim at `TOD_CLI_PATH`: the shim runs the nouns that reach GitHub
/// with it (`tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT`).
pub const LOCAL_CLI_PATH: &str = "/opt/tod/tod-cli-local";
/// The supervisor keeps its copy of the user's database in
/// `<this>/<node>` (its default `--state-dir`).
pub const SUPERVISOR_STATE_ROOT: &str = "/var/lib/tod-supervisor";
/// What Claude Code sees as `CLAUDE_CODE_OAUTH_TOKEN` with
/// [`ClaudeTokenVia::Proxy`], so it believes it is signed in; the proxy
/// replaces the `Authorization` header it sends to `api.anthropic.com`.
pub const CLAUDE_TOKEN_PLACEHOLDER: &str = "sk-ant-oat01-placeholder-injected-by-proxy";
/// The variable Claude Code reads its subscription token from.
pub const CLAUDE_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// The relay takes each `TOD_SUPERVISOR_ENV_<NAME>` out of its own
/// environment and gives it to the supervisor it starts, as `<NAME>`
/// (`tod-relay`'s `SUPERVISOR_ENV_PREFIX`). Nothing else it starts sees it.
pub const RELAY_SUPERVISOR_ENV_PREFIX: &str = "TOD_SUPERVISOR_ENV_";
/// Which [`SchedulerKind`] the node's supervisor uses (`orchestrator` or `blaxel`).
pub const SCHEDULER_ENV: &str = "TOD_SCHEDULER";
/// The Blaxel workspace the node's sandbox is in.
pub const BLAXEL_WORKSPACE_ENV: &str = "TOD_BLAXEL_WORKSPACE";
/// What the supervisor sends Blaxel as its token: the proxy replaces the
/// `Authorization` header for `api.blaxel.ai` with the real one.
pub const BLAXEL_TOKEN_PLACEHOLDER: &str = "placeholder-injected-by-proxy";

/// The user's credentials for a node's proxy rules.
#[derive(Clone, Default)]
pub struct NodeCredentials {
    pub github_token: Option<String>,
    pub linear_api_key: Option<String>,
    /// The Blaxel token (for `api.blaxel.ai` and the orchestrator's host).
    pub blaxel_token: String,
    /// The user's Claude subscription token (`claude setup-token`), for the
    /// node's Claude Code. Where it goes is [`ClaudeTokenVia`].
    pub claude_oauth_token: Option<String>,
    /// Credentials the user defined in the node's Environment, with hosts.
    pub custom: Vec<CustomCredential>,
}

/// The label a node sandbox carries with [`env_fingerprint`] of the custom
/// credentials its proxy was made with, so the app can tell when the user's
/// Environment has a credential the proxy lacks (proxy rules are fixed when
/// the sandbox is created).
pub const ENV_LABEL: &str = "tod-env";

/// A short fingerprint of `custom` (names, hosts, header, template, and
/// values: a changed value changes it); `none` without any. Safe as a label
/// value, and reveals nothing of the values.
pub fn env_fingerprint(custom: &[CustomCredential]) -> String {
    use sha2::{Digest, Sha256};
    if custom.is_empty() {
        return "none".to_string();
    }
    let mut sorted: Vec<&CustomCredential> = custom.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut h = Sha256::new();
    for c in sorted {
        for part in [c.name.as_str(), &c.hosts.join(","), &c.header, &c.template, &c.secret_value] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
    }
    let digest = h.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A user-defined credential applied by the proxy on `hosts`.
#[derive(Clone)]
pub struct CustomCredential {
    /// Unique among the node's credentials; names the proxy secret.
    pub name: String,
    pub hosts: Vec<String>,
    pub header: String,
    /// The header value with `{value}` for the secret.
    pub template: String,
    /// What the proxy substitutes (already encoded for the auth style).
    pub secret_value: String,
}

impl std::fmt::Debug for NodeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeCredentials")
            .field("github_token", &self.github_token.as_ref().map(|_| "<set>"))
            .field("linear_api_key", &self.linear_api_key.as_ref().map(|_| "<set>"))
            .field("claude_oauth_token", &self.claude_oauth_token.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

/// One proxy rule: requests to `destination` get `header: value`, where
/// `value` references `secret` as `{{SECRET:<name>}}`.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyRule {
    pub destination: String,
    pub header: String,
    /// The header value template, e.g. `Bearer {{SECRET:github}}`.
    pub value: String,
    pub secret_name: String,
    pub secret_value: String,
}

impl std::fmt::Debug for ProxyRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRule")
            .field("destination", &self.destination)
            .field("header", &self.header)
            .field("value", &self.value)
            .field("secret_name", &self.secret_name)
            .finish_non_exhaustive()
    }
}

fn rule(destination: &str, secret: &str, value_prefix: &str, secret_value: String) -> ProxyRule {
    ProxyRule {
        destination: destination.to_string(),
        header: "Authorization".to_string(),
        value: format!("{value_prefix}{{{{SECRET:{secret}}}}}"),
        secret_name: secret.to_string(),
        secret_value,
    }
}

/// The rules a node's sandbox is created with: GitHub's API (Bearer) and git
/// (Basic `x-access-token:<token>`), Linear (the bare key), Anthropic's API
/// (Bearer Claude subscription token, with [`ClaudeTokenVia::Proxy`]),
/// `api.blaxel.ai` and the orchestrator's host (Bearer Blaxel token).
/// Destinations are exact hosts, never wildcards: a destination that echoes
/// headers would hand the secret back. A missing GitHub, Linear, or Claude
/// credential just leaves its rule out.
pub fn proxy_rules(creds: &NodeCredentials, orchestrator_host: &str, claude_via: ClaudeTokenVia) -> Vec<ProxyRule> {
    let mut rules = Vec::new();
    if let Some(token) = github_token(creds) {
        rules.push(rule("api.github.com", "github", "Bearer ", token.to_string()));
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        rules.push(rule("github.com", "github_basic", "Basic ", basic));
    }
    if let Some(key) = creds.linear_api_key.as_deref().filter(|k| !k.is_empty()) {
        rules.push(rule("api.linear.app", "linear", "", key.to_string()));
    }
    if claude_via == ClaudeTokenVia::Proxy
        && let Some(token) = claude_token(creds)
    {
        rules.push(rule("api.anthropic.com", "claude", "Bearer ", token.to_string()));
    }
    rules.extend(custom_proxy_rules(&creds.custom));
    rules.push(rule("api.blaxel.ai", "blaxel", "Bearer ", creds.blaxel_token.clone()));
    rules.push(rule(orchestrator_host, "blaxel", "Bearer ", creds.blaxel_token.clone()));
    rules
}

/// The rules for the Environment's credentials (`NodeCredentials::custom`) alone (the Environment's
/// secrets with hosts), for a sandbox whose proxy holds nothing else.
pub fn custom_proxy_rules(custom: &[CustomCredential]) -> Vec<ProxyRule> {
    let mut rules = Vec::new();
    for c in custom {
        let secret: String = c
            .name
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect();
        let secret = format!("env_{secret}");
        for host in &c.hosts {
            rules.push(ProxyRule {
                destination: host.clone(),
                header: c.header.clone(),
                value: c.template.replace("{value}", &format!("{{{{SECRET:{secret}}}}}")),
                secret_name: secret.clone(),
                secret_value: c.secret_value.clone(),
            });
        }
    }
    rules
}

fn claude_token(creds: &NodeCredentials) -> Option<&str> {
    creds.claude_oauth_token.as_deref().map(str::trim).filter(|t| !t.is_empty())
}

/// What the supervisor's environment adds to the sandbox's: the Claude
/// token with [`ClaudeTokenVia::Env`] (Claude Code, the supervisor's child,
/// inherits it); nothing with the proxy, or without a token. The relay is
/// given these as `TOD_SUPERVISOR_ENV_<NAME>` ([`relay_env`]).
pub fn supervisor_env(creds: &NodeCredentials, claude_via: ClaudeTokenVia) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let (ClaudeTokenVia::Env, Some(token)) = (claude_via, claude_token(creds)) {
        out.push((CLAUDE_TOKEN_ENV.to_string(), token.to_string()));
    }
    out
}

/// The relay process's own environment for [`supervisor_env`]: each variable
/// under [`RELAY_SUPERVISOR_ENV_PREFIX`], which the relay passes on to the
/// supervisor alone.
pub fn relay_env(supervisor_env: &[(String, String)]) -> Vec<(String, String)> {
    supervisor_env
        .iter()
        .map(|(name, value)| (format!("{RELAY_SUPERVISOR_ENV_PREFIX}{name}"), value.clone()))
        .collect()
}

/// `spec.network.proxy` for [`proxy_rules`].
///
/// TODO(W6): Blaxel's proxy routing is in public preview and its request
/// shape is not documented in this repository; this is our best reading of
/// the design doc (routing rules per destination adding headers, secrets
/// given with the rule and referenced as `{{SECRET:name}}`, omitted from the
/// stored spec). Verify against a live workspace and adjust only here.
pub fn proxy_spec(rules: &[ProxyRule]) -> Value {
    let routing: Vec<Value> = rules
        .iter()
        .map(|r| {
            json!({
                "destinations": [r.destination],
                "headers": { r.header.clone(): r.value },
                "secrets": { r.secret_name.clone(): r.secret_value },
            })
        })
        .collect();
    json!({ "enabled": true, "routing": routing })
}

/// The environment of every process in a node's sandbox. No secrets.
/// `agent` (`claude` or `mock`) becomes the supervisor's `TOD_SUPERVISOR_AGENT`;
/// `None` leaves it to the supervisor's default, Claude. `claude_placeholder`
/// (the proxy holds a Claude token) sets `CLAUDE_CODE_OAUTH_TOKEN` to
/// [`CLAUDE_TOKEN_PLACEHOLDER`], so Claude Code starts signed in.
/// `github_via_proxy` (the proxy holds a GitHub token) sets
/// [`GITHUB_AUTH_ENV`], so tod's own GitHub client relies on the proxy.
/// `TOD_SCHEDULER` ([`SchedulerKind`]) and `TOD_BLAXEL_WORKSPACE` (for
/// the Blaxel scheduler's calls, whose token the proxy adds) say how the
/// supervisor schedules its wakes (`tod_core::scheduler::from_env`).
pub fn node_env(spec: &NodeSandboxSpec, claude_placeholder: bool, github_via_proxy: bool) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("GH_TOKEN", GH_TOKEN_PLACEHOLDER.to_string()),
        // Node's fetch ignores HTTP(S)_PROXY without it, and so gets nothing injected.
        ("NODE_USE_ENV_PROXY", "1".to_string()),
        ("TOD_SANDBOX", spec.name.to_string()),
        ("TOD_USER", spec.user.to_string()),
        ("TOD_NODE", spec.node.to_string()),
        ("TOD_ORCHESTRATOR_CLI_URL", spec.orchestrator_cli_url.to_string()),
        (SCHEDULER_ENV, spec.scheduler.as_str().to_string()),
        (BLAXEL_WORKSPACE_ENV, spec.workspace.to_string()),
    ];
    if let Some(agent) = spec.agent.filter(|a| !a.is_empty()) {
        env.push(("TOD_SUPERVISOR_AGENT", agent.to_string()));
    }
    if claude_placeholder {
        env.push((CLAUDE_TOKEN_ENV, CLAUDE_TOKEN_PLACEHOLDER.to_string()));
    }
    if github_via_proxy {
        env.push((GITHUB_AUTH_ENV, GITHUB_AUTH_PROXY.to_string()));
    }
    env
}

fn github_token(creds: &NodeCredentials) -> Option<&str> {
    creds.github_token.as_deref().filter(|t| !t.is_empty())
}

/// What a node's sandbox is created as. With an empty `node`, a base
/// that nodes are forked from ([`ensure_base`]).
#[derive(Clone, Copy)]
pub struct NodeSandboxSpec<'a> {
    pub name: &'a str,
    pub image: &'a str,
    pub region: &'a str,
    pub memory_mb: u32,
    pub user: &'a str,
    pub node: &'a str,
    /// The orchestrator's host name, e.g. `<name>-<ws>.bl.run`.
    pub orchestrator_host: &'a str,
    /// The orchestrator's full `/cli` URL.
    pub orchestrator_cli_url: &'a str,
    /// The supervisor's agent (`claude`, `mock`); `None` for its default.
    pub agent: Option<&'a str>,
    /// Where the Claude subscription token goes (`sandboxes.toml`'s
    /// `claude_token_via`).
    pub claude_via: ClaudeTokenVia,
    /// Who wakes the node when it waits (`sandboxes.toml`'s `scheduler`).
    pub scheduler: SchedulerKind,
    /// The Blaxel workspace, for the supervisor's own Blaxel calls.
    pub workspace: &'a str,
}

/// The variables [`node_env`] gives a node for `spec` and `creds`.
pub fn env_for(spec: &NodeSandboxSpec, creds: &NodeCredentials) -> Vec<(&'static str, String)> {
    let claude_placeholder = spec.claude_via == ClaudeTokenVia::Proxy && claude_token(creds).is_some();
    let github_via_proxy = github_token(creds).is_some();
    node_env(spec, claude_placeholder, github_via_proxy)
}

/// The `POST /sandboxes` body for a node's sandbox.
pub fn create_body(spec: &NodeSandboxSpec, creds: &NodeCredentials) -> Value {
    let envs: Vec<Value> = env_for(spec, creds)
        .into_iter()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    json!({
        "metadata": {
            "name": spec.name,
            "labels": {
                "tod-kind": if spec.node.is_empty() { "node-base" } else { "node" },
                "tod-user": spec.user,
                "tod-node": spec.node,
                ENV_LABEL: env_fingerprint(&creds.custom),
            },
        },
        "spec": {
            "region": spec.region,
            "runtime": {
                "image": spec.image,
                "memory": spec.memory_mb,
                "ports": [{ "name": "tod-relay", "target": RELAY_PORT, "protocol": "HTTP" }],
                "envs": envs,
            },
            "network": { "proxy": proxy_spec(&proxy_rules(creds, spec.orchestrator_host, spec.claude_via)) },
        },
    })
}

/// Creates a node's sandbox. The proxy cannot be added later, so this is the
/// only way a node's sandbox is made. Wait with [`Blaxel::wait_deployed`].
pub fn create(bx: &Blaxel, spec: &NodeSandboxSpec, creds: &NodeCredentials) -> Result<()> {
    bx.create_from_body(&create_body(spec, creds))
}

/// What gets installed into a node's sandbox.
pub struct NodePayload<'a> {
    pub relay: &'a [u8],
    /// `tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT`.
    pub shim: &'a [u8],
    /// The real `tod-cli` (Linux), installed at [`LOCAL_CLI_PATH`] for the
    /// nouns the shim runs here. `None` when not built yet: installing goes
    /// on without it, with a warning.
    pub local_cli: Option<&'a [u8]>,
    /// `None` when not built yet: installing goes on without it, with a warning.
    pub supervisor: Option<&'a [u8]>,
    /// The repository's HTTPS URL (git goes through the proxy).
    pub repo_url: &'a str,
    pub branch: &'a str,
    /// Who the node's commits are by: the user's own git identity, set in
    /// the checkout (a fresh sandbox has none, and `git commit` refuses to
    /// run without one).
    pub git_identity: &'a GitIdentity,
    /// The supervisor's bundles, as `(path relative to /opt/tod, contents)`:
    /// `process/...` and `media/...`, found beside its executable (it builds
    /// the agent's context from them). See [`bundle_files`].
    pub bundles: &'a [(String, Vec<u8>)],
    /// [`supervisor_env`]: handed to the relay at each start, which gives it
    /// to the supervisor alone. Kept off every command line.
    pub supervisor_env: &'a [(String, String)],
}

/// Every file under `dir`, as `(prefix/relative/path, contents)`, for
/// [`NodePayload::bundles`].
pub fn bundle_files(dir: &Path, prefix: &str) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = format!("{rel}/{name}");
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), &rel, out)?;
            } else {
                out.push((rel, std::fs::read(entry.path())?));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, prefix.trim_end_matches('/'), &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Reads the supervisor from `target/sandbox/` (built by
/// `scripts/build-sandbox-binaries.sh`), if present.
pub fn supervisor_from(sandbox_target_dir: &Path) -> Option<Vec<u8>> {
    std::fs::read(sandbox_target_dir.join("tod-supervisor")).ok()
}

/// Reads the Linux `tod-cli` from `target/sandbox/`, if present.
pub fn local_cli_from(sandbox_target_dir: &Path) -> Option<Vec<u8>> {
    std::fs::read(sandbox_target_dir.join("tod-cli")).ok()
}

/// A git author: `user.name` and `user.email`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

impl GitIdentity {
    /// Used when the user has no git identity of their own configured.
    pub fn fallback() -> Self {
        Self {
            name: "tod".to_string(),
            email: "tod@localhost".to_string(),
        }
    }
}

/// Sets git's CA to the proxy's system-wide, then clones the repository (or
/// fetches), sets the checkout's author to `identity`, and checks out
/// `branch`: from `origin/<branch>` when it exists, else the local branch
/// when there is one, else a new branch from the latest of origin's default
/// branch (`origin/HEAD`).
pub fn checkout_script(repo_url: &str, branch: &str, identity: &GitIdentity) -> String {
    let (repo, br, dir) = (shell_quote(repo_url), shell_quote(branch), shell_quote(WORKSPACE_DIR));
    let (name, email) = (shell_quote(&identity.name), shell_quote(&identity.email));
    format!(
        "set -e\n\
         if [ -n \"$SSL_CERT_FILE\" ]; then git config --system http.sslCAInfo \"$SSL_CERT_FILE\"; fi\n\
         if [ -d {dir}/.git ]; then git -C {dir} fetch origin; \
         else mkdir -p \"$(dirname {dir})\" && git clone {repo} {dir}; fi\n\
         cd {dir}\n\
         git config user.name {name}\n\
         git config user.email {email}\n\
         if git show-ref --verify --quiet refs/remotes/origin/{br}; then \
         git checkout -B {br} origin/{br} && git branch --set-upstream-to=origin/{br}; \
         elif git show-ref --verify --quiet refs/heads/{br}; then git checkout {br}; \
         else git checkout --no-track -b {br} \"$(git symbolic-ref -q --short refs/remotes/origin/HEAD || echo origin/main)\"; fi\n",
    )
}

/// Installs the relay, the shim, and the supervisor into the node's sandbox
/// at `url`, checks out the branch, and starts the relay and supervisor.
/// `progress` hears each slow step and any warning.
pub fn provision(bx: &Blaxel, url: &str, payload: &NodePayload, progress: &mut dyn FnMut(&str)) -> Result<()> {
    install(bx, url, payload, progress)?;
    start(bx, url, payload, progress)
}

/// The part of [`provision`] that is the same for every node: the relay,
/// the shim, the supervisor, the Linux `tod-cli`, and the bundles. A base
/// gets only this.
pub fn install(bx: &Blaxel, url: &str, payload: &NodePayload, progress: &mut dyn FnMut(&str)) -> Result<()> {
    bx.upload_large(url, RELAY_PATH, payload.relay, "0755")?;
    bx.upload(url, TOD_CLI_PATH, payload.shim, "0755")?;
    match payload.supervisor {
        Some(bytes) => bx.upload_large(url, SUPERVISOR_PATH, bytes, "0755")?,
        None => progress("warning: tod-supervisor is not built (target/sandbox/); the node will not run on its own"),
    }
    match payload.local_cli {
        Some(bytes) => bx.upload_large(url, LOCAL_CLI_PATH, bytes, "0755")?,
        None => progress("warning: tod-cli is not built for Linux (target/sandbox/); `tod-cli pr` will not work in the node"),
    }
    // The node manifest keeps `tod-sandbox`'s own provisioning (a shell,
    // `exec`, Zed) from reinstalling over this relay and `tod-cli`.
    let res = bx.run(
        url,
        &format!(
            "mkdir -p {TOD_DIR}/bin && ln -sf {TOD_CLI_PATH} /usr/local/bin/tod-cli && {LOOPBACK_HOSTS} && printf '%s\\n' {} > {MANIFEST_PATH}",
            shell_quote(&node_manifest(payload.relay))
        ),
        30,
    )?;
    if res.exit_code != 0 {
        bail!("installing tod-cli failed: {}", res.output());
    }
    ensure_claude_adapter(bx, url, progress)?;
    if !payload.bundles.is_empty() {
        progress(&format!("installing {} bundle files…", payload.bundles.len()));
        let res = bx.run(url, &format!("rm -rf {TOD_DIR}/process {TOD_DIR}/media"), 30)?;
        if res.exit_code != 0 {
            bail!("clearing the old bundles failed: {}", res.output());
        }
        for (rel, bytes) in payload.bundles {
            bx.upload(url, &format!("{TOD_DIR}/{rel}"), bytes, "0644")?;
        }
    }
    Ok(())
}

/// The part of [`provision`] that is the node's own: checks out its branch,
/// and starts the relay and the supervisor. All a node forked from a base
/// needs.
pub fn start(bx: &Blaxel, url: &str, payload: &NodePayload, progress: &mut dyn FnMut(&str)) -> Result<()> {
    progress(&format!("checking out {}…", payload.branch));
    let res = bx.run(url, &format!("sh -c {} 2>&1", shell_quote(&checkout_script(payload.repo_url, payload.branch, payload.git_identity))), 600)?;
    if res.exit_code != 0 {
        bail!("checking out {} failed (exit {}): {}", payload.branch, res.exit_code, res.output());
    }

    // Restarted, so it runs the binary just installed.
    bx.kill(url, RELAY_PROCESS)?;
    let relay_env = relay_env(payload.supervisor_env);
    let relay_env: Vec<(&str, &str)> = relay_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    bx.start_with_env(url, RELAY_PROCESS, &crate::provision::relay_command(), true, &relay_env)?;
    if payload.supervisor.is_some() {
        // Through the relay's poke, like every later wake: the relay starts
        // `tod-supervisor wake` (its default `--supervisor-cmd`, the same
        // [`supervisor_command`]) and tracks it, so a later poke signals it.
        progress("starting the supervisor…");
        let res = bx.run(url, &poke_script(), 60)?;
        if res.exit_code != 0 {
            bail!("poking the relay to start the supervisor failed: {}", res.output());
        }
    }
    Ok(())
}

/// What a base made from `base_body` (its [`create_body`]) with `payload`
/// installed is: a base whose fingerprint differs (new binaries, bundles,
/// credentials, image, orchestrator) is made again. Hex SHA-256; it
/// covers the proxy's secrets, so it is kept only on this machine.
pub fn base_fingerprint(base_body: &Value, payload: &NodePayload) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut part = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part(base_body.to_string().as_bytes());
    part(payload.relay);
    part(payload.shim);
    part(payload.local_cli.unwrap_or_default());
    part(payload.supervisor.unwrap_or_default());
    for (rel, bytes) in payload.bundles {
        part(rel.as_bytes());
        part(bytes);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Makes sure the base `base` (a [`NodeSandboxSpec`] with no node) is there,
/// deployed, and has `payload` installed ([`install`]), making it again when
/// it is missing, can no longer run, or its fingerprint ([`base_fingerprint`])
/// is not `recorded`. Returns the fingerprint to record once it is ready.
pub fn ensure_base(
    bx: &Blaxel,
    base: &NodeSandboxSpec,
    creds: &NodeCredentials,
    payload: &NodePayload,
    recorded: Option<&str>,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let body = create_body(base, creds);
    let fingerprint = base_fingerprint(&body, payload);
    let existing = bx.get(base.name)?;
    let dead = |status: &str| matches!(status.to_ascii_uppercase().as_str(), "FAILED" | "TERMINATED" | "DELETING" | "DELETED");
    if let Some(info) = &existing
        && !dead(&info.status)
        && recorded == Some(fingerprint.as_str())
    {
        return Ok(fingerprint);
    }
    if existing.is_some() {
        progress(&format!("replacing the base {}…", base.name));
        bx.delete(base.name)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while bx.get(base.name)?.is_some() {
            if std::time::Instant::now() >= deadline {
                bail!("the old base {} is still there", base.name);
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    progress(&format!("creating the base {}…", base.name));
    bx.create_from_body(&body)?;
    let info = bx.wait_deployed(base.name, std::time::Duration::from_secs(300))?;
    let url = info.url.ok_or_else(|| anyhow::anyhow!("sandbox {} has no URL", base.name))?;
    install(bx, &url, payload, progress)?;
    Ok(fingerprint)
}

/// Forks `spec`'s sandbox from the base `base`, with `spec`'s environment
/// ([`env_for`]) over the base's. Its proxy rules and labels are the base's
/// (the same user's credentials); wait with [`Blaxel::wait_deployed`], then
/// [`start`] it.
pub fn fork(bx: &Blaxel, base: &str, spec: &NodeSandboxSpec, creds: &NodeCredentials) -> Result<()> {
    bx.fork_with_envs(base, spec.name, &env_for(spec, creds))
}

/// Names `localhost` in `/etc/hosts` when the image left it out (Blaxel's
/// images ship it empty). curl resolves `localhost` by itself; most programs
/// do not, and the sandbox's proxy is `http://localhost:49152`.
const LOOPBACK_HOSTS: &str = "{ grep -qw localhost /etc/hosts 2>/dev/null || \
     printf '127.0.0.1 localhost\\n::1 localhost ip6-localhost ip6-loopback\\n' >> /etc/hosts; }";

/// The Claude ACP adapter's npm package: the only adapter tod runs Claude on
/// (`tod_agent::claude_adapter`).
const CLAUDE_ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";

/// Installs the Claude adapter globally when the image lacks it, as an image
/// baked before it replaced `claude-code-acp` does; otherwise a Claude node
/// fails every step with "the Claude ACP adapter is not installed". Exits 0
/// at once when it is there, 3 when there is no npm to install it with.
fn claude_adapter_script() -> String {
    format!(
        "command -v claude-agent-acp >/dev/null 2>&1 && exit 0; \
         command -v npm >/dev/null 2>&1 || {{ echo 'no npm in the image'; exit 3; }}; \
         echo installing; npm install -g --silent --prefix /usr/local {CLAUDE_ADAPTER_PACKAGE} 2>&1"
    )
}

/// See [`claude_adapter_script`]. A sandbox that cannot have it still runs
/// a node on the mock agent, so failing is only a warning.
fn ensure_claude_adapter(bx: &Blaxel, url: &str, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let check = bx.run(url, "command -v claude-agent-acp >/dev/null 2>&1", 30)?;
    if check.exit_code == 0 {
        return Ok(());
    }
    progress("installing the Claude agent adapter (the image lacks it)…");
    let res = bx.run(url, &format!("sh -c {}", shell_quote(&claude_adapter_script())), 600)?;
    if res.exit_code != 0 {
        progress(&format!(
            "warning: could not install {CLAUDE_ADAPTER_PACKAGE} (exit {}), so the node cannot run Claude: {}",
            res.exit_code,
            res.output().trim()
        ));
    }
    Ok(())
}

/// The command the relay runs to start the supervisor (its default
/// `--supervisor-cmd`).
pub fn supervisor_command() -> String {
    format!("{SUPERVISOR_PATH} wake --workspace {WORKSPACE_DIR}")
}

/// What a wake's Blaxel schedule runs in the node's sandbox: a poke of the
/// relay, which starts the supervisor (with the environment only it is
/// given) or signals the one running, exactly as a poke from outside does.
/// Should the relay not be running (the sandbox was restarted, not just
/// woken), the supervisor is started directly. The process API runs it
/// with a shell.
pub fn wake_command() -> String {
    format!("curl -fsS -m 20 -X POST http://127.0.0.1:{RELAY_PORT}/poke || exec {}", supervisor_command())
}

/// Pokes the relay on loopback, retrying while it starts listening.
pub fn poke_script() -> String {
    format!(
        "for i in 1 2 3 4 5 6 7 8 9 10; do \
         curl -fsS -X POST http://127.0.0.1:{RELAY_PORT}/poke && exit 0; sleep 1; done; exit 1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_adapter_script_names_the_adapter_package() {
        let s = claude_adapter_script();
        assert!(s.starts_with("command -v claude-agent-acp"), "{s}");
        assert!(s.contains("npm install -g --silent --prefix /usr/local @agentclientprotocol/claude-agent-acp"));
        assert!(!s.contains("claude-code-acp"));
    }

    /// Runs the script with only `bin` on `PATH`; `npm` there records that
    /// it ran.
    #[cfg(unix)]
    fn run_adapter_script(adapter: bool, npm: bool) -> (i32, bool) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tod-adapter-{}-{adapter}-{npm}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = |name: &str, body: &str| {
            let p = bin.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        if adapter {
            script("claude-agent-acp", "exit 0");
        }
        if npm {
            script("npm", &format!(": > {}", dir.join("npm-ran").display()));
        }
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(claude_adapter_script())
            .env("PATH", &bin)
            .status()
            .unwrap();
        let ran = dir.join("npm-ran").exists();
        let _ = std::fs::remove_dir_all(&dir);
        (status.code().unwrap_or(-1), ran)
    }

    #[cfg(unix)]
    #[test]
    fn claude_adapter_script_installs_only_when_missing() {
        assert_eq!(run_adapter_script(true, true), (0, false));
        assert_eq!(run_adapter_script(false, true), (0, true));
        assert_eq!(run_adapter_script(false, false), (3, false));
    }

    fn creds() -> NodeCredentials {
        NodeCredentials {
            github_token: Some("ghp_x".into()),
            linear_api_key: Some("lin_y".into()),
            blaxel_token: "bl_z".into(),
            claude_oauth_token: Some("sk-ant-oat01-real".into()),
            custom: Vec::new(),
        }
    }

    fn spec(claude_via: ClaudeTokenVia) -> NodeSandboxSpec<'static> {
        NodeSandboxSpec {
            name: "node-1",
            image: "img",
            region: "us-was-1",
            memory_mb: 4096,
            user: "u1",
            node: "n1",
            orchestrator_host: "orch.bl.run",
            orchestrator_cli_url: "https://orch.bl.run/port/8090/cli",
            agent: Some("mock"),
            claude_via,
            scheduler: SchedulerKind::Blaxel,
            workspace: "ws-1",
        }
    }

    #[test]
    fn the_node_is_told_its_scheduler_and_workspace() {
        let body = create_body(&spec(ClaudeTokenVia::Proxy), &creds());
        assert_eq!(env_value(&body, SCHEDULER_ENV).as_deref(), Some("blaxel"));
        assert_eq!(env_value(&body, BLAXEL_WORKSPACE_ENV).as_deref(), Some("ws-1"));
        let orch = NodeSandboxSpec { scheduler: SchedulerKind::Orchestrator, ..spec(ClaudeTokenVia::Proxy) };
        assert_eq!(env_value(&create_body(&orch, &creds()), SCHEDULER_ENV).as_deref(), Some("orchestrator"));
    }

    #[test]
    fn a_base_is_labelled_apart_from_nodes() {
        let node = spec(ClaudeTokenVia::Proxy);
        assert_eq!(create_body(&node, &creds())["metadata"]["labels"]["tod-kind"], "node");
        let base = NodeSandboxSpec { name: "tod-node-base", node: "", ..node };
        assert_eq!(create_body(&base, &creds())["metadata"]["labels"]["tod-kind"], "node-base");
    }

    #[test]
    fn a_base_fingerprint_changes_with_what_it_is_made_from() {
        fn payload<'a>(relay: &'a [u8], bundles: &'a [(String, Vec<u8>)], identity: &'a GitIdentity) -> NodePayload<'a> {
            NodePayload {
                relay,
                shim: b"shim",
                local_cli: None,
                supervisor: Some(b"sup"),
                repo_url: "https://example/r.git",
                branch: "b",
                git_identity: identity,
                bundles,
                supervisor_env: &[],
            }
        }
        let identity = GitIdentity::fallback();
        let bundles = vec![("process/a.md".to_string(), b"one".to_vec())];
        let bundles = bundles.as_slice();
        let body = create_body(&NodeSandboxSpec { node: "", ..spec(ClaudeTokenVia::Proxy) }, &creds());
        let a = base_fingerprint(&body, &payload(b"relay-1", bundles, &identity));
        assert_eq!(a, base_fingerprint(&body, &payload(b"relay-1", bundles, &identity)));
        assert_ne!(a, base_fingerprint(&body, &payload(b"relay-2", bundles, &identity)));
        assert_ne!(a, base_fingerprint(&body, &payload(b"relay-1", &[], &identity)));
        let mut other = body.clone();
        other["spec"]["runtime"]["image"] = json!("other:latest");
        assert_ne!(a, base_fingerprint(&other, &payload(b"relay-1", bundles, &identity)));
        // The branch and repository are the node's, not the base's.
        let mut p = payload(b"relay-1", bundles, &identity);
        p.branch = "other";
        assert_eq!(a, base_fingerprint(&body, &p));
    }

    #[test]
    fn a_wake_pokes_the_relay_and_falls_back_to_the_supervisor() {
        let cmd = wake_command();
        assert!(cmd.starts_with("curl -fsS -m 20 -X POST http://127.0.0.1:2222/poke || exec "), "{cmd}");
        assert!(cmd.ends_with(&supervisor_command()), "{cmd}");
    }

    #[test]
    fn rules_name_exact_hosts_with_the_right_headers() {
        let rules = proxy_rules(&creds(), "orch-ws.bl.run", ClaudeTokenVia::Proxy);
        let hosts: Vec<&str> = rules.iter().map(|r| r.destination.as_str()).collect();
        assert_eq!(
            hosts,
            ["api.github.com", "github.com", "api.linear.app", "api.anthropic.com", "api.blaxel.ai", "orch-ws.bl.run"]
        );
        assert_eq!(rules[0].value, "Bearer {{SECRET:github}}");
        assert_eq!(rules[1].value, "Basic {{SECRET:github_basic}}");
        assert_eq!(rules[1].secret_value, "eC1hY2Nlc3MtdG9rZW46Z2hwX3g="); // x-access-token:ghp_x
        assert_eq!(rules[2].value, "{{SECRET:linear}}");
        assert_eq!(rules[3].header, "Authorization");
        assert_eq!(rules[3].value, "Bearer {{SECRET:claude}}");
        assert_eq!(rules[3].secret_value, "sk-ant-oat01-real");
        assert!(rules.iter().all(|r| !r.destination.contains('*')));
    }

    #[test]
    fn missing_credentials_leave_their_rules_out() {
        let creds = NodeCredentials { blaxel_token: "b".into(), ..Default::default() };
        let rules = proxy_rules(&creds, "o.bl.run", ClaudeTokenVia::Proxy);
        assert_eq!(rules.len(), 2);
        // A blank Claude token is no token.
        let creds = NodeCredentials { claude_oauth_token: Some("  ".into()), ..creds };
        assert_eq!(proxy_rules(&creds, "o.bl.run", ClaudeTokenVia::Proxy).len(), 2);
        assert!(supervisor_env(&creds, ClaudeTokenVia::Env).is_empty());
    }

    #[test]
    fn secrets_are_only_in_the_proxy_spec() {
        let body = create_body(&spec(ClaudeTokenVia::Proxy), &creds());
        let runtime = body["spec"]["runtime"].to_string();
        assert!(!runtime.contains("ghp_x") && !runtime.contains("lin_y") && !runtime.contains("bl_z"));
        assert!(!runtime.contains("sk-ant-oat01-real"));
        assert!(runtime.contains("NODE_USE_ENV_PROXY"));
        assert!(runtime.contains(GH_TOKEN_PLACEHOLDER));
        assert!(runtime.contains("TOD_SUPERVISOR_AGENT"));
        let proxy = body["spec"]["network"]["proxy"].to_string();
        assert!(proxy.contains("ghp_x") && proxy.contains("{{SECRET:github}}"));
        assert!(!format!("{:?}", creds()).contains("ghp_x"));
    }

    #[test]
    fn debug_never_shows_a_credential() {
        let shown = format!("{:?}", creds());
        for secret in ["ghp_x", "lin_y", "bl_z", "sk-ant-oat01-real"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(shown.contains("claude_oauth_token: Some(\"<set>\")"), "{shown}");
        let rules = proxy_rules(&creds(), "o.bl.run", ClaudeTokenVia::Proxy);
        assert!(!format!("{rules:?}").contains("sk-ant-oat01-real"));
    }

    fn env_value(body: &Value, name: &str) -> Option<String> {
        body["spec"]["runtime"]["envs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .map(|e| e["value"].as_str().unwrap().to_string())
    }

    #[test]
    fn github_through_the_proxy_is_flagged_only_with_a_token() {
        let body = create_body(&spec(ClaudeTokenVia::Proxy), &creds());
        assert_eq!(env_value(&body, GITHUB_AUTH_ENV).as_deref(), Some(GITHUB_AUTH_PROXY));
        let none = NodeCredentials { github_token: None, ..creds() };
        let body = create_body(&spec(ClaudeTokenVia::Proxy), &none);
        assert_eq!(env_value(&body, GITHUB_AUTH_ENV), None);
        // `gh` gets its placeholder either way.
        assert_eq!(env_value(&body, "GH_TOKEN").as_deref(), Some(GH_TOKEN_PLACEHOLDER));
    }

    #[test]
    fn proxy_mode_gives_the_sandbox_only_a_placeholder() {
        let body = create_body(&spec(ClaudeTokenVia::Proxy), &creds());
        assert_eq!(env_value(&body, CLAUDE_TOKEN_ENV).as_deref(), Some(CLAUDE_TOKEN_PLACEHOLDER));
        assert!(!body["spec"]["runtime"].to_string().contains("sk-ant-oat01-real"));
        let routing = body["spec"]["network"]["proxy"]["routing"].as_array().unwrap();
        let claude = routing.iter().find(|r| r["destinations"][0] == "api.anthropic.com").unwrap();
        assert_eq!(claude["headers"]["Authorization"], "Bearer {{SECRET:claude}}");
        assert_eq!(claude["secrets"]["claude"], "sk-ant-oat01-real");
        // Nothing goes to the supervisor beyond the sandbox's environment.
        assert!(supervisor_env(&creds(), ClaudeTokenVia::Proxy).is_empty());
    }

    #[test]
    fn env_mode_gives_the_token_to_the_supervisor_alone() {
        let body = create_body(&spec(ClaudeTokenVia::Env), &creds());
        // Neither the token nor a placeholder in the sandbox-wide
        // environment, and no proxy rule for it.
        assert_eq!(env_value(&body, CLAUDE_TOKEN_ENV), None);
        assert!(!body.to_string().contains("sk-ant-oat01-real"));
        assert!(!body.to_string().contains("api.anthropic.com"));
        let sup = supervisor_env(&creds(), ClaudeTokenVia::Env);
        assert_eq!(sup, [(CLAUDE_TOKEN_ENV.to_string(), "sk-ant-oat01-real".to_string())]);
        assert_eq!(
            relay_env(&sup),
            [("TOD_SUPERVISOR_ENV_CLAUDE_CODE_OAUTH_TOKEN".to_string(), "sk-ant-oat01-real".to_string())]
        );
    }

    #[test]
    fn the_env_fingerprint_follows_credentials_and_hides_values() {
        let c = |value: &str| CustomCredential {
            name: "a".into(),
            hosts: vec!["h.io".into()],
            header: "Authorization".into(),
            template: "Bearer {value}".into(),
            secret_value: value.into(),
        };
        assert_eq!(env_fingerprint(&[]), "none");
        assert_eq!(env_fingerprint(&[c("x")]), env_fingerprint(&[c("x")]));
        assert_ne!(env_fingerprint(&[c("x")]), env_fingerprint(&[c("y")]));
        assert!(!env_fingerprint(&[c("x")]).contains('x') || env_fingerprint(&[c("x")]).len() == 16);
    }

    #[test]
    fn custom_credentials_become_proxy_rules_and_nothing_reaches_the_supervisor() {
        let creds = NodeCredentials {
            custom: vec![CustomCredential {
                name: "grow-book".into(),
                hosts: vec!["api.growthbook.io".into()],
                header: "Authorization".into(),
                template: "Bearer {value}".into(),
                secret_value: "gb_secret".into(),
            }],
            ..creds()
        };
        let rules = proxy_rules(&creds, "orch.example", ClaudeTokenVia::Proxy);
        let r = rules.iter().find(|r| r.destination == "api.growthbook.io").unwrap();
        assert_eq!(r.header, "Authorization");
        assert_eq!(r.value, "Bearer {{SECRET:env_grow_book}}");
        assert_eq!(r.secret_value, "gb_secret");
        let sup = supervisor_env(&creds, ClaudeTokenVia::Proxy);
        assert!(sup.is_empty());
    }

    #[test]
    fn no_claude_token_means_no_placeholder_and_no_rule() {
        let creds = NodeCredentials { claude_oauth_token: None, ..creds() };
        for via in [ClaudeTokenVia::Proxy, ClaudeTokenVia::Env] {
            let body = create_body(&spec(via), &creds);
            assert_eq!(env_value(&body, CLAUDE_TOKEN_ENV), None);
            assert!(!body.to_string().contains("api.anthropic.com"));
            assert!(supervisor_env(&creds, via).is_empty());
        }
    }

    #[test]
    fn the_supervisor_command_is_the_relays_default() {
        // tod-relay's `--supervisor-cmd` default (crates/tod-relay/src/server.rs).
        assert_eq!(supervisor_command(), "/opt/tod/tod-supervisor wake --workspace /workspace/repo");
        assert!(poke_script().contains("http://127.0.0.1:2222/poke"));
    }

    #[test]
    fn checkout_sets_git_ca_and_tracks_origin() {
        let me = GitIdentity {
            name: "Ada O'Neil".into(),
            email: "ada@example.com".into(),
        };
        let s = checkout_script("https://github.com/o/r.git", "feat/x", &me);
        assert!(s.contains(r"git config user.name 'Ada O'\''Neil'"), "{s}");
        assert!(s.contains("git config user.email ada@example.com"), "{s}");
        assert!(s.contains("git config --system http.sslCAInfo \"$SSL_CERT_FILE\""));
        assert!(s.contains("git clone https://github.com/o/r.git /workspace/repo"));
        assert!(s.contains("refs/remotes/origin/feat/x"));
        assert!(s.contains("git checkout -B feat/x origin/feat/x"));
        // A new branch starts from origin's default branch, never a stale HEAD.
        assert!(s.contains("git checkout --no-track -b feat/x \"$(git symbolic-ref -q --short refs/remotes/origin/HEAD"));
        assert!(!s.contains("else git checkout -B feat/x;"));
        // A branch needing quotes stays one word after `origin/`.
        assert!(checkout_script("u", "a b", &me).contains("origin/'a b'"));
    }
}
