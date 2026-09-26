//! An autonomous node's own sandbox: created with proxy rules that inject the
//! user's credentials, then given the node's branch, the relay, the HTTP
//! `tod-cli` shim, and the supervisor.
//!
//! This crate is a leaf: the caller reads the user's `CredentialStore` and
//! passes the values in ([`NodeCredentials`]); they go only into the
//! sandbox's proxy rules, never into its environment or files.
//!
//! Design: `doc/cloud-sandboxes/autonomous-nodes.md` (Credentials).

use crate::blaxel::{Blaxel, RELAY_PORT};
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

/// The user's credentials for a node's proxy rules.
#[derive(Clone, Default)]
pub struct NodeCredentials {
    pub github_token: Option<String>,
    pub linear_api_key: Option<String>,
    /// The Blaxel token (for `api.blaxel.ai` and the orchestrator's host).
    pub blaxel_token: String,
}

impl std::fmt::Debug for NodeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeCredentials")
            .field("github_token", &self.github_token.as_ref().map(|_| "<set>"))
            .field("linear_api_key", &self.linear_api_key.as_ref().map(|_| "<set>"))
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
/// (Basic `x-access-token:<token>`), Linear (the bare key), `api.blaxel.ai`
/// and the orchestrator's host (Bearer Blaxel token). Destinations are exact
/// hosts, never wildcards: a destination that echoes headers would hand the
/// secret back. A missing GitHub or Linear credential just leaves its rule out.
pub fn proxy_rules(creds: &NodeCredentials, orchestrator_host: &str) -> Vec<ProxyRule> {
    let mut rules = Vec::new();
    if let Some(token) = creds.github_token.as_deref().filter(|t| !t.is_empty()) {
        rules.push(rule("api.github.com", "github", "Bearer ", token.to_string()));
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        rules.push(rule("github.com", "github_basic", "Basic ", basic));
    }
    if let Some(key) = creds.linear_api_key.as_deref().filter(|k| !k.is_empty()) {
        rules.push(rule("api.linear.app", "linear", "", key.to_string()));
    }
    rules.push(rule("api.blaxel.ai", "blaxel", "Bearer ", creds.blaxel_token.clone()));
    rules.push(rule(orchestrator_host, "blaxel", "Bearer ", creds.blaxel_token.clone()));
    rules
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
pub fn node_env(user: &str, node: &str, orchestrator_cli_url: &str) -> Vec<(&'static str, String)> {
    vec![
        ("GH_TOKEN", GH_TOKEN_PLACEHOLDER.to_string()),
        // Node's fetch ignores HTTP(S)_PROXY without it, and so gets nothing injected.
        ("NODE_USE_ENV_PROXY", "1".to_string()),
        ("TOD_USER", user.to_string()),
        ("TOD_NODE", node.to_string()),
        ("TOD_ORCHESTRATOR_CLI_URL", orchestrator_cli_url.to_string()),
    ]
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
}

/// The `POST /sandboxes` body for a node's sandbox.
pub fn create_body(spec: &NodeSandboxSpec, creds: &NodeCredentials) -> Value {
    let envs: Vec<Value> = node_env(spec.user, spec.node, spec.orchestrator_cli_url)
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
            "network": { "proxy": proxy_spec(&proxy_rules(creds, spec.orchestrator_host)) },
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
    bx.upload(url, RELAY_PATH, payload.relay, "0755")?;
    bx.upload(url, TOD_CLI_PATH, payload.shim, "0755")?;
    match payload.supervisor {
        Some(bytes) => bx.upload(url, SUPERVISOR_PATH, bytes, "0755")?,
        None => progress("warning: tod-supervisor is not built (target/sandbox/); the node will not run on its own"),
    }
    let res = bx.run(url, &format!("mkdir -p {TOD_DIR}/bin && ln -sf {TOD_CLI_PATH} /usr/local/bin/tod-cli"), 30)?;
    if res.exit_code != 0 {
        bail!("installing tod-cli failed: {}", res.output());
    }

    progress(&format!("checking out {}…", payload.branch));
    let res = bx.run(url, &format!("sh -c {} 2>&1", shell_quote(&checkout_script(payload.repo_url, payload.branch))), 600)?;
    if res.exit_code != 0 {
        bail!("checking out {} failed (exit {}): {}", payload.branch, res.exit_code, res.output());
    }

    if bx.process_status(url, RELAY_PROCESS)?.as_deref() != Some("running") {
        bx.start(url, RELAY_PROCESS, &format!("{RELAY_PATH} --port {RELAY_PORT}"), true)?;
    }
    if payload.supervisor.is_some() && bx.process_status(url, SUPERVISOR_PROCESS)?.as_deref() != Some("running") {
        progress("starting the supervisor…");
        bx.start(url, SUPERVISOR_PROCESS, &format!("{SUPERVISOR_PATH} --workspace {WORKSPACE_DIR}"), true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> NodeCredentials {
        NodeCredentials {
            github_token: Some("ghp_x".into()),
            linear_api_key: Some("lin_y".into()),
            blaxel_token: "bl_z".into(),
        }
    }

    #[test]
    fn rules_name_exact_hosts_with_the_right_headers() {
        let rules = proxy_rules(&creds(), "orch-ws.bl.run");
        let hosts: Vec<&str> = rules.iter().map(|r| r.destination.as_str()).collect();
        assert_eq!(hosts, ["api.github.com", "github.com", "api.linear.app", "api.blaxel.ai", "orch-ws.bl.run"]);
        assert_eq!(rules[0].value, "Bearer {{SECRET:github}}");
        assert_eq!(rules[1].value, "Basic {{SECRET:github_basic}}");
        assert_eq!(rules[1].secret_value, "eC1hY2Nlc3MtdG9rZW46Z2hwX3g="); // x-access-token:ghp_x
        assert_eq!(rules[2].value, "{{SECRET:linear}}");
        assert!(rules.iter().all(|r| !r.destination.contains('*')));
    }

    #[test]
    fn missing_credentials_leave_their_rules_out() {
        let creds = NodeCredentials { blaxel_token: "b".into(), ..Default::default() };
        let rules = proxy_rules(&creds, "o.bl.run");
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn secrets_are_only_in_the_proxy_spec() {
        let spec = NodeSandboxSpec {
            name: "node-1",
            image: "img",
            region: "us-was-1",
            memory_mb: 4096,
            user: "u1",
            node: "n1",
            orchestrator_host: "orch.bl.run",
            orchestrator_cli_url: "https://orch.bl.run/port/8080/cli",
        };
        let body = create_body(&spec, &creds());
        let runtime = body["spec"]["runtime"].to_string();
        assert!(!runtime.contains("ghp_x") && !runtime.contains("lin_y") && !runtime.contains("bl_z"));
        assert!(runtime.contains("NODE_USE_ENV_PROXY"));
        assert!(runtime.contains(GH_TOKEN_PLACEHOLDER));
        let proxy = body["spec"]["network"]["proxy"].to_string();
        assert!(proxy.contains("ghp_x") && proxy.contains("{{SECRET:github}}"));
        assert!(!format!("{:?}", creds()).contains("ghp_x"));
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
