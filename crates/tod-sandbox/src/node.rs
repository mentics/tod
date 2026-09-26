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
pub use crate::config::ClaudeTokenVia;
use crate::provision::{RELAY_PATH, RELAY_PROCESS, TOD_CLI_PATH, TOD_DIR};
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
    pub header: &'static str,
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
        header: "Authorization",
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
    if let Some(token) = creds.github_token.as_deref().filter(|t| !t.is_empty()) {
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
    rules.push(rule("api.blaxel.ai", "blaxel", "Bearer ", creds.blaxel_token.clone()));
    rules.push(rule(orchestrator_host, "blaxel", "Bearer ", creds.blaxel_token.clone()));
    rules
}

fn claude_token(creds: &NodeCredentials) -> Option<&str> {
    creds.claude_oauth_token.as_deref().map(str::trim).filter(|t| !t.is_empty())
}

/// What the supervisor's environment adds to the sandbox's: the Claude
/// token with [`ClaudeTokenVia::Env`] (Claude Code, the supervisor's child,
/// inherits it); nothing with the proxy, or without a token. The relay is
/// given these as `TOD_SUPERVISOR_ENV_<NAME>` ([`relay_env`]).
pub fn supervisor_env(creds: &NodeCredentials, claude_via: ClaudeTokenVia) -> Vec<(&'static str, String)> {
    match (claude_via, claude_token(creds)) {
        (ClaudeTokenVia::Env, Some(token)) => vec![(CLAUDE_TOKEN_ENV, token.to_string())],
        _ => Vec::new(),
    }
}

/// The relay process's own environment for [`supervisor_env`]: each variable
/// under [`RELAY_SUPERVISOR_ENV_PREFIX`], which the relay passes on to the
/// supervisor alone.
pub fn relay_env(supervisor_env: &[(&str, String)]) -> Vec<(String, String)> {
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
                "headers": { r.header: r.value },
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
pub fn node_env(
    sandbox: &str,
    user: &str,
    node: &str,
    orchestrator_cli_url: &str,
    agent: Option<&str>,
    claude_placeholder: bool,
) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("GH_TOKEN", GH_TOKEN_PLACEHOLDER.to_string()),
        // Node's fetch ignores HTTP(S)_PROXY without it, and so gets nothing injected.
        ("NODE_USE_ENV_PROXY", "1".to_string()),
        ("TOD_SANDBOX", sandbox.to_string()),
        ("TOD_USER", user.to_string()),
        ("TOD_NODE", node.to_string()),
        ("TOD_ORCHESTRATOR_CLI_URL", orchestrator_cli_url.to_string()),
    ];
    if let Some(agent) = agent.filter(|a| !a.is_empty()) {
        env.push(("TOD_SUPERVISOR_AGENT", agent.to_string()));
    }
    if claude_placeholder {
        env.push((CLAUDE_TOKEN_ENV, CLAUDE_TOKEN_PLACEHOLDER.to_string()));
    }
    env
}

/// What a node's sandbox is created as.
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
}

/// The `POST /sandboxes` body for a node's sandbox.
pub fn create_body(spec: &NodeSandboxSpec, creds: &NodeCredentials) -> Value {
    let claude_placeholder = spec.claude_via == ClaudeTokenVia::Proxy && claude_token(creds).is_some();
    let envs: Vec<Value> =
        node_env(spec.name, spec.user, spec.node, spec.orchestrator_cli_url, spec.agent, claude_placeholder)
            .into_iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
    json!({
        "metadata": {
            "name": spec.name,
            "labels": { "tod-kind": "node", "tod-user": spec.user, "tod-node": spec.node },
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
    /// `None` when not built yet: installing goes on without it, with a warning.
    pub supervisor: Option<&'a [u8]>,
    /// The repository's HTTPS URL (git goes through the proxy).
    pub repo_url: &'a str,
    pub branch: &'a str,
    /// The supervisor's bundles, as `(path relative to /opt/tod, contents)`:
    /// `process/...` and `media/...`, found beside its executable (it builds
    /// the agent's context from them). See [`bundle_files`].
    pub bundles: &'a [(String, Vec<u8>)],
    /// [`supervisor_env`]: handed to the relay at each start, which gives it
    /// to the supervisor alone. Kept off every command line.
    pub supervisor_env: &'a [(&'static str, String)],
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

/// Sets git's CA to the proxy's system-wide, then clones the repository (or
/// fetches) and checks out `branch`, from `origin/<branch>` when it exists.
pub fn checkout_script(repo_url: &str, branch: &str) -> String {
    let (repo, br, dir) = (shell_quote(repo_url), shell_quote(branch), shell_quote(WORKSPACE_DIR));
    format!(
        "set -e\n\
         if [ -n \"$SSL_CERT_FILE\" ]; then git config --system http.sslCAInfo \"$SSL_CERT_FILE\"; fi\n\
         if [ -d {dir}/.git ]; then git -C {dir} fetch origin; \
         else mkdir -p \"$(dirname {dir})\" && git clone {repo} {dir}; fi\n\
         cd {dir}\n\
         if git show-ref --verify --quiet refs/remotes/origin/{br}; then \
         git checkout -B {br} origin/{br} && git branch --set-upstream-to=origin/{br}; \
         else git checkout -B {br}; fi\n",
    )
}

/// Installs the relay, the shim, and the supervisor into the node's sandbox
/// at `url`, checks out the branch, and starts the relay and supervisor.
/// `progress` hears each slow step and any warning.
pub fn provision(bx: &Blaxel, url: &str, payload: &NodePayload, progress: &mut dyn FnMut(&str)) -> Result<()> {
    bx.upload_large(url, RELAY_PATH, payload.relay, "0755")?;
    bx.upload(url, TOD_CLI_PATH, payload.shim, "0755")?;
    match payload.supervisor {
        Some(bytes) => bx.upload_large(url, SUPERVISOR_PATH, bytes, "0755")?,
        None => progress("warning: tod-supervisor is not built (target/sandbox/); the node will not run on its own"),
    }
    let res = bx.run(
        url,
        &format!("mkdir -p {TOD_DIR}/bin && ln -sf {TOD_CLI_PATH} /usr/local/bin/tod-cli && {LOOPBACK_HOSTS}"),
        30,
    )?;
    if res.exit_code != 0 {
        bail!("installing tod-cli failed: {}", res.output());
    }
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

    progress(&format!("checking out {}…", payload.branch));
    let res = bx.run(url, &format!("sh -c {} 2>&1", shell_quote(&checkout_script(payload.repo_url, payload.branch))), 600)?;
    if res.exit_code != 0 {
        bail!("checking out {} failed (exit {}): {}", payload.branch, res.exit_code, res.output());
    }

    // Restarted, so it runs the binary just installed.
    bx.kill(url, RELAY_PROCESS)?;
    let relay_env = relay_env(payload.supervisor_env);
    let relay_env: Vec<(&str, &str)> = relay_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    bx.start_with_env(url, RELAY_PROCESS, &format!("{RELAY_PATH} --port {RELAY_PORT}"), true, &relay_env)?;
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

/// Names `localhost` in `/etc/hosts` when the image left it out (Blaxel's
/// images ship it empty). curl resolves `localhost` by itself; most programs
/// do not, and the sandbox's proxy is `http://localhost:49152`.
const LOOPBACK_HOSTS: &str = "{ grep -qw localhost /etc/hosts 2>/dev/null || \
     printf '127.0.0.1 localhost\\n::1 localhost ip6-localhost ip6-loopback\\n' >> /etc/hosts; }";

/// The command the relay runs to start the supervisor (its default
/// `--supervisor-cmd`).
pub fn supervisor_command() -> String {
    format!("{SUPERVISOR_PATH} wake --workspace {WORKSPACE_DIR}")
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

    fn creds() -> NodeCredentials {
        NodeCredentials {
            github_token: Some("ghp_x".into()),
            linear_api_key: Some("lin_y".into()),
            blaxel_token: "bl_z".into(),
            claude_oauth_token: Some("sk-ant-oat01-real".into()),
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
        }
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
        assert_eq!(sup, [(CLAUDE_TOKEN_ENV, "sk-ant-oat01-real".to_string())]);
        assert_eq!(
            relay_env(&sup),
            [("TOD_SUPERVISOR_ENV_CLAUDE_CODE_OAUTH_TOKEN".to_string(), "sk-ant-oat01-real".to_string())]
        );
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
        let s = checkout_script("https://github.com/o/r.git", "feat/x");
        assert!(s.contains("git config --system http.sslCAInfo \"$SSL_CERT_FILE\""));
        assert!(s.contains("git clone https://github.com/o/r.git /workspace/repo"));
        assert!(s.contains("refs/remotes/origin/feat/x"));
        assert!(s.contains("git checkout -B feat/x origin/feat/x"));
        // A branch needing quotes stays one word after `origin/`.
        assert!(checkout_script("u", "a b").contains("origin/'a b'"));
    }
}
