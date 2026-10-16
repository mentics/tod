//! Making a sandbox ready for tod: its packages, the relay, and the relay running.
//!
//! What a ready sandbox has is summed up by a manifest (hashes of the bootstrap
//! script and relay binary, and whether agents were asked for) written to
//! `/opt/tod/manifest`. Checking it is one command through the relay when the
//! relay is already up, so connecting to a ready sandbox costs one round trip.
//! A baked image carries the same manifest, so it is never reprovisioned.
//!
//! A node sandbox (`crate::node`) is provisioned by the app that runs the
//! node, not here, and its `tod-cli` is a different script. It writes a
//! [`node_manifest`], which [`ensure`] takes as ready: reinstalling would
//! kill its relay, and with it the node's running supervisor, and replace
//! its `tod-cli`, just because someone opened a shell or ran a command there.

use crate::blaxel::Blaxel;
use crate::relay;
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub const TOD_DIR: &str = "/opt/tod";
pub const MANIFEST_PATH: &str = "/opt/tod/manifest";
pub const RELAY_PATH: &str = "/opt/tod/tod-relay";
/// `tod-cli` for processes in the sandbox: a script that sends each command
/// through the relay's tunnel to the tod app, which runs it.
pub const TOD_CLI_PATH: &str = "/opt/tod/bin/tod-cli";
/// Where the relay's tunnel listens in the sandbox (see `tod-relay`).
pub const TUNNEL_PORT: u16 = 2223;
/// The relay's name in the sandbox's process API.
pub const RELAY_PROCESS: &str = "tod-relay";

/// How the sandbox's process API starts the relay. The API runs a command
/// through `sh -c` and reaps only its own children; `exec` makes the relay
/// that child, so killing it to restart it leaves no `<defunct>` relay
/// behind (without it `sh` is killed and reaped, and the relay is orphaned
/// to PID 1, which never reaps it).
pub fn relay_command() -> String {
    format!("exec {RELAY_PATH} --port {}", crate::blaxel::RELAY_PORT)
}

/// What gets installed into a sandbox.
pub struct Payload<'a> {
    pub bootstrap: &'a [u8],
    pub relay: &'a [u8],
    /// The `tod-cli` script ([`TOD_CLI_PATH`]).
    pub tod_cli: &'a [u8],
    /// Install Node.js and the agent adapter too.
    pub agents: bool,
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl Payload<'_> {
    pub fn manifest(&self) -> String {
        format!(
            "bootstrap={} relay={} tod-cli={} agents={}",
            &hex(self.bootstrap)[..16],
            &hex(self.relay)[..16],
            &hex(self.tod_cli)[..16],
            self.agents
        )
    }

    pub fn bootstrap_args(&self) -> &'static str {
        if self.agents { "--agents" } else { "" }
    }
}

/// What a node sandbox's [`MANIFEST_PATH`] says: its relay, installed by
/// `crate::node::install`. [`ensure`] never reprovisions a sandbox that has one.
pub fn node_manifest(relay: &[u8]) -> String {
    format!("{NODE_MANIFEST_PREFIX}relay={}", &hex(relay)[..16])
}

const NODE_MANIFEST_PREFIX: &str = "node ";

/// Whether `manifest` is a node sandbox's ([`node_manifest`]).
pub fn is_node_manifest(manifest: &str) -> bool {
    manifest.trim().starts_with(NODE_MANIFEST_PREFIX)
}

/// Whether a sandbox whose relay reports `reported` needs nothing from
/// [`ensure`]: it is the manifest expected, or the sandbox is a node's.
fn ready_as_is(reported: Option<&str>, expected: &str) -> bool {
    reported.is_some_and(|m| m == expected || is_node_manifest(m))
}

/// What a fork's [`MANIFEST_PATH`] says once [`ensure`] has adopted it
/// ([`Mode::Fork`]): the relay and `tod-cli` put there. Unlike
/// [`Payload::manifest`] it does not cover the bootstrap script: a fork's
/// packages are whatever its source had, and are never installed here.
pub fn fork_manifest(payload: &Payload) -> String {
    format!("{FORK_MANIFEST_PREFIX}relay={} tod-cli={}", &hex(payload.relay)[..16], &hex(payload.tod_cli)[..16])
}

const FORK_MANIFEST_PREFIX: &str = "fork ";

/// Whether `manifest` is a fork's ([`fork_manifest`]).
pub fn is_fork_manifest(manifest: &str) -> bool {
    manifest.trim().starts_with(FORK_MANIFEST_PREFIX)
}

/// What [`ensure`] had to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyReady,
    RelayStarted,
    Provisioned,
    /// A fork got the relay and `tod-cli` and nothing else.
    Adopted,
}

/// How [`ensure`] treats a sandbox that is not ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Install what is missing ([`Payload::bootstrap`]). For a sandbox from a
    /// plain image, which has nothing of tod's.
    Install,
    /// [`Mode::Install`] even for a fork that [`ensure`] adopted: the
    /// explicit repair when a fork lacks something (`tod-sandbox ensure --install`).
    ForceInstall,
    /// The sandbox is a copy of one that was prepared ahead (a golden). Never
    /// install a package: put the relay and `tod-cli` in (a few megabytes, and
    /// they have to match this app) and check the rest is there, naming what
    /// is not and how to fix the source instead. Forking is for speed, and an
    /// install here would cost every fork what the golden was built to save.
    Fork,
}

/// What tod needs in a sandbox that [`Mode::Fork`] only checks for. The
/// first line it prints is `missing:` and the commands it did not find.
fn requirements_script(agents: bool) -> String {
    let agent_checks = if agents {
        r#"
node_ok() {
    command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1 || return 1
    node -e 'const [a, b] = process.versions.node.split(".").map(Number);
             process.exit(a > 20 || (a === 20 && b >= 10) ? 0 : 1)' 2>/dev/null
}
node_ok || missing="$missing node>=20.10"
command -v claude-agent-acp >/dev/null 2>&1 || missing="$missing claude-agent-acp"
# A non-login shell may not have where `claude` was installed on its PATH.
PATH="$PATH:/usr/local/bin:$HOME/.local/bin:/root/.local/bin:$HOME/.claude/local"
command -v claude >/dev/null 2>&1 || optional="$optional claude"
"#
    } else {
        ""
    };
    format!(
        r#"mkdir -p {TOD_DIR}/bin {TOD_DIR}/logs
missing=""
optional=""
for c in sh git curl tar gzip bash ps scp; do
    command -v "$c" >/dev/null 2>&1 || missing="$missing $c"
done
found=""
for p in /usr/lib/ssh/sftp-server /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server /usr/libexec/sftp-server; do
    if [ -x "$p" ]; then ln -sf "$p" {TOD_DIR}/bin/sftp-server; found=1; break; fi
done
[ -n "$found" ] || missing="$missing sftp-server"
{agent_checks}
echo "missing:$missing"
echo "optional:$optional"
case "${{HTTPS_PROXY:-}}" in *"{{{{"*) echo "proxy:unrendered" ;; esac
"#
    )
}

/// What [`requirements_script`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Requirements {
    pub missing: Vec<String>,
    pub optional: Vec<String>,
    /// The sandbox's `HTTPS_PROXY` is a `{{file(...)}}` template the platform
    /// never filled in ([`LOCAL_PROXY_URL`] is what it should say).
    pub proxy_unrendered: bool,
}

/// The proxy every sandbox made with a proxy runs on loopback, which adds the
/// credentials its rules hold. A sandbox forked and then given new proxy
/// rules gets the rules but can keep an `HTTPS_PROXY` that points at an
/// unrendered template, which git and every other client then refuse
/// (`Unsupported proxy syntax`).
pub const LOCAL_PROXY_URL: &str = "http://localhost:49152";

/// The environment that overrides an unrendered proxy, for the relay (and so
/// everything it runs).
pub fn local_proxy_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("HTTPS_PROXY", LOCAL_PROXY_URL),
        ("HTTP_PROXY", LOCAL_PROXY_URL),
        ("https_proxy", LOCAL_PROXY_URL),
        ("http_proxy", LOCAL_PROXY_URL),
    ]
}

fn parse_requirements(output: &str) -> Requirements {
    let list = |tag: &str| -> Vec<String> {
        output
            .lines()
            .find_map(|l| l.trim().strip_prefix(tag))
            .map(|rest| rest.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    };
    Requirements {
        missing: list("missing:"),
        optional: list("optional:"),
        proxy_unrendered: output.lines().any(|l| l.trim() == "proxy:unrendered"),
    }
}

/// Why a fork cannot be used, and what to do about it.
pub fn missing_in_fork_message(missing: &[String]) -> String {
    format!(
        "this sandbox is a copy of one that lacks what tod needs: {}. tod does not install anything into a \
         fork, so every fork stays fast. Fix the sandbox it was forked from: add these to the image the golden \
         is built from and make the golden again (for the workspace, .devcontainer/blaxel/Dockerfile and \
         `blaxel-sbx golden refresh`). To install them into just this sandbox instead, run \
         `tod-sandbox ensure <name> --install`",
        missing.join(", ")
    )
}

/// Makes the sandbox at `url` ready. `progress` hears about each slow step.
pub fn ensure(
    bx: &Blaxel,
    url: &str,
    payload: &Payload,
    mode: Mode,
    progress: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    let manifest = payload.manifest();
    let fork_manifest = fork_manifest(payload);
    let reported = relay_manifest(bx, url);
    if ready_as_is(reported.as_deref(), &manifest) {
        return Ok(Outcome::AlreadyReady);
    }
    if reported.as_deref() == Some(fork_manifest.as_str()) {
        // The relay's own environment can still carry the unrendered proxy
        // (a relay started before this was handled, or restarted by the
        // platform): fix that, which costs one command when it is fine.
        if relay_output(bx, url, "printf '%s' \"${HTTPS_PROXY:-}\"").is_some_and(|v| v.contains("{{")) {
            progress(&format!("restarting the relay with {LOCAL_PROXY_URL} as its proxy (its HTTPS_PROXY was never rendered)…"));
            bx.kill(url, RELAY_PROCESS)?;
            bx.start_with_env(url, RELAY_PROCESS, &relay_command(), true, &local_proxy_env())?;
            wait_for_relay(bx, url, &fork_manifest)?;
            return Ok(Outcome::RelayStarted);
        }
        return Ok(Outcome::AlreadyReady);
    }

    let current = bx.run(url, &format!("cat {MANIFEST_PATH} 2>/dev/null"), 30)?;
    if is_node_manifest(current.output()) {
        // Its relay is started by the node's provisioning, with the
        // supervisor's environment; starting one here would lack it.
        bail!(
            "this is a node's sandbox and its relay is not answering; run the node in the cloud again to \
            reprovision it"
        );
    }
    if mode == Mode::Fork && !is_node_manifest(current.output()) {
        return adopt_fork(bx, url, payload, &fork_manifest, current.output().trim(), progress);
    }
    let mut outcome = Outcome::RelayStarted;
    if mode != Mode::ForceInstall && is_fork_manifest(current.output()) {
        // Adopted before, by another build of tod: its relay and `tod-cli`
        // are replaced, and nothing is installed.
        return adopt_fork(bx, url, payload, &fork_manifest, current.output().trim(), progress);
    }
    if current.output().trim() != manifest {
        check_relay_arch(payload.relay)?;
        progress("installing tod's dependencies in the sandbox (first connect only)…");
        bx.upload(url, &format!("{TOD_DIR}/bootstrap.sh"), payload.bootstrap, "0755")?;
        bx.upload(url, &format!("{RELAY_PATH}.new"), payload.relay, "0755")?;
        bx.upload(url, TOD_CLI_PATH, payload.tod_cli, "0755")?;
        let started = Instant::now();
        let res = bx.run(url, &format!("sh {TOD_DIR}/bootstrap.sh {} 2>&1", payload.bootstrap_args()), 1200)?;
        if res.exit_code != 0 {
            let tail: Vec<&str> = res.output().lines().rev().take(20).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            bail!("bootstrap failed (exit {}):\n{}", res.exit_code, tail.join("\n"));
        }
        progress(&format!("installed in {}s", started.elapsed().as_secs()));
        // A relay from an older install would keep running the old binary.
        bx.kill(url, RELAY_PROCESS)?;
        let res = bx.run(
            url,
            &format!(
                "mv {RELAY_PATH}.new {RELAY_PATH} && printf '%s\\n' {} > {MANIFEST_PATH}",
                relay::shell_quote(&manifest)
            ),
            30,
        )?;
        if res.exit_code != 0 {
            bail!("installing the relay failed: {}", res.output());
        }
        outcome = Outcome::Provisioned;
    }

    if bx.process_status(url, RELAY_PROCESS)?.as_deref() != Some("running") {
        progress("starting the relay…");
        bx.start(url, RELAY_PROCESS, &relay_command(), true)?;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match relay_manifest(bx, url) {
            Some(m) if ready_as_is(Some(&m), &manifest) => return Ok(outcome),
            Some(m) => bail!("the sandbox reports manifest {m:?}, expected {manifest:?}"),
            None if Instant::now() > deadline => bail!("the relay did not come up within 20s"),
            None => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// Checks a fork has what tod needs, installing nothing: fails naming what
/// is missing ([`missing_in_fork_message`]). Warns about what is only optional.
pub fn require_fork_prerequisites(
    bx: &Blaxel,
    url: &str,
    agents: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Requirements> {
    let found = parse_requirements(bx.run(url, &requirements_script(agents), 60)?.output());
    if !found.optional.is_empty() {
        progress(&format!(
            "warning: the sandbox has no {} on its PATH, nor in /usr/local/bin or ~/.local/bin (a terminal agent cannot start there; the ACP agent is unaffected). If it is installed elsewhere, put it on the PATH in the image the sandbox is made from",
            found.optional.join(", ")
        ));
    }
    if !found.missing.is_empty() {
        bail!("{}", missing_in_fork_message(&found.missing));
    }
    Ok(found)
}

/// [`Mode::Fork`]: checks the sandbox has what tod needs, puts the relay and
/// `tod-cli` in, and starts the relay. Installs no package.
fn adopt_fork(
    bx: &Blaxel,
    url: &str,
    payload: &Payload,
    fork_manifest: &str,
    current: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    check_relay_arch(payload.relay)?;
    let found = require_fork_prerequisites(bx, url, payload.agents, progress)?;
    let started = Instant::now();
    if current.is_empty() {
        progress("putting tod's relay and tod-cli in the sandbox…");
    } else {
        progress("updating tod's relay and tod-cli in the sandbox…");
    }
    bx.upload_large(url, RELAY_PATH, payload.relay, "0755")?;
    bx.upload(url, TOD_CLI_PATH, payload.tod_cli, "0755")?;
    // A relay from an older install would keep running the old binary.
    bx.kill(url, RELAY_PROCESS)?;
    let res = bx.run(url, &format!("printf '%s\\n' {} > {MANIFEST_PATH}", relay::shell_quote(fork_manifest)), 30)?;
    if res.exit_code != 0 {
        bail!("writing {MANIFEST_PATH} failed: {}", res.output());
    }
    progress(&format!("relay installed in {:.1}s; starting it…", started.elapsed().as_secs_f64()));
    if found.proxy_unrendered {
        progress(&format!(
            "the platform left this sandbox's HTTPS_PROXY unrendered; the relay and what it runs use {LOCAL_PROXY_URL} instead"
        ));
        bx.start_with_env(url, RELAY_PROCESS, &relay_command(), true, &local_proxy_env())?;
    } else {
        bx.start(url, RELAY_PROCESS, &relay_command(), true)?;
    }
    wait_for_relay(bx, url, fork_manifest)?;
    Ok(Outcome::Adopted)
}

fn wait_for_relay(bx: &Blaxel, url: &str, manifest: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match relay_manifest(bx, url) {
            Some(m) if ready_as_is(Some(&m), manifest) || m == manifest => return Ok(()),
            Some(m) => bail!("the sandbox reports manifest {m:?}, expected {manifest:?}"),
            None if Instant::now() > deadline => bail!(
                "the relay did not come up within 20s (see {TOD_DIR}/logs in the sandbox, \
                 and `tod-sandbox exec <name> -- ps`)"
            ),
            None => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// Sandboxes are x86-64 Linux. A relay built for anything else (an arm64
/// Docker build on Apple Silicon, say) uploads fine but cannot run, and shows
/// up only as the relay never answering.
pub fn check_relay_arch(relay: &[u8]) -> Result<()> {
    const EM_X86_64: u16 = 0x3e;
    if relay.len() < 20 || &relay[..4] != b"\x7fELF" {
        bail!("the relay binary is not a Linux (ELF) executable; rebuild it with scripts/build-sandbox-binaries.sh");
    }
    let machine = u16::from_le_bytes([relay[18], relay[19]]);
    if machine != EM_X86_64 {
        bail!(
            "the relay binary is built for the wrong architecture (ELF machine {machine:#x}, sandboxes need x86-64); \
            rebuild it with scripts/build-sandbox-binaries.sh"
        );
    }
    Ok(())
}

/// The manifest as read through the relay, or `None` if the relay is not reachable.
fn relay_manifest(bx: &Blaxel, url: &str) -> Option<String> {
    relay_output(bx, url, &format!("cat {MANIFEST_PATH} 2>/dev/null"))
}

/// What `command` prints, run through the relay (so in its environment), or
/// `None` if the relay is not reachable.
fn relay_output(bx: &Blaxel, url: &str, command: &str) -> Option<String> {
    let ws = relay::ws_url(url, "/exec");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let res = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(20),
            relay::run_capture(&ws, bx.token(), command),
        )
        .await
    });
    match res {
        Ok(Ok((_, out, _))) => Some(String::from_utf8_lossy(&out).trim().to_string()),
        _ => None,
    }
}

/// A repository cloned into a baked image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakeRepo {
    /// An `https://` URL with no credentials in it: the image keeps it as
    /// the clone's `origin`, and the build's layers keep the command.
    pub url: String,
    /// Where it is cloned: an absolute path in the image.
    pub dir: String,
}

/// Reads one repository per line: `<url> [<directory>]`. The directory
/// defaults to `/root/<repository name>`. Blank lines and lines starting with
/// `#` are skipped. A URL that carries credentials is refused, because
/// whatever is in the Dockerfile stays in the image's layers.
pub fn parse_bake_repos(text: &str) -> Result<Vec<BakeRepo>> {
    let mut repos: Vec<BakeRepo> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let mut words = line.split_whitespace();
        let url = words.next().unwrap_or_default();
        let dir = words.next();
        if words.next().is_some() {
            bail!("expected `<url> [<directory>]`, got: {line}");
        }
        let Some(rest) = url.strip_prefix("https://") else {
            bail!("only https:// repositories can be built in, got: {url}");
        };
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty() || authority.contains('@') {
            bail!("{url}: give the URL without credentials (they would be kept in the image)");
        }
        let safe = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:~+%".contains(c));
        if !safe(url) {
            bail!("{url}: not a plain repository URL");
        }
        let name = url.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
        let name = name.strip_suffix(".git").unwrap_or(name);
        let dir = match dir {
            Some(dir) => dir.trim_end_matches('/').to_string(),
            None if !name.is_empty() => format!("/root/{name}"),
            None => bail!("{url}: no repository name to make a directory from; give one"),
        };
        if !dir.starts_with('/') || dir.len() < 2 || dir.split('/').any(|part| part == "..") || !safe(&dir) {
            bail!("{dir}: the directory must be an absolute path (letters, digits, - _ . /)");
        }
        if repos.iter().any(|r| r.dir == dir) {
            bail!("two repositories would go to {dir}");
        }
        repos.push(BakeRepo { url: url.to_string(), dir });
    }
    Ok(repos)
}

/// The build context for an image with everything installed ahead of time.
pub fn bake_dockerfile(base_image: &str, payload: &Payload) -> String {
    bake_dockerfile_with_repos(base_image, payload, &[])
}

/// As [`bake_dockerfile`], with `repos` cloned into the image after tod's
/// dependencies are installed (which include git).
pub fn bake_dockerfile_with_repos(base_image: &str, payload: &Payload, repos: &[BakeRepo]) -> String {
    let clones: String = repos
        .iter()
        .map(|r| {
            format!(
                "RUN git clone --quiet {} {}\n",
                relay::shell_quote(&r.url),
                relay::shell_quote(&r.dir)
            )
        })
        .collect();
    format!(
        r#"# Generated by `tod-sandbox bake`: {base_image} with tod's dependencies installed.
FROM {base_image}
COPY --from=ghcr.io/blaxel-ai/sandbox:latest /sandbox-api /usr/local/bin/sandbox-api
COPY bootstrap.sh {TOD_DIR}/bootstrap.sh
COPY tod-relay {RELAY_PATH}
COPY tod-cli {TOD_CLI_PATH}
RUN chmod 0755 {TOD_DIR}/bootstrap.sh {RELAY_PATH} {TOD_CLI_PATH} && sh {TOD_DIR}/bootstrap.sh {args} \
 && printf '%s\n' '{manifest}' > {MANIFEST_PATH}
{clones}ENTRYPOINT ["/usr/local/bin/sandbox-api"]
"#,
        args = payload.bootstrap_args(),
        manifest = payload.manifest(),
    )
}

/// The build context for any image as-is, made runnable as a sandbox: only
/// Blaxel's sandbox API is added; tod's dependencies install on first connect.
pub fn wrap_dockerfile(base_image: &str) -> String {
    format!(
        r#"# Generated by `tod-sandbox`: {base_image}, runnable as a Blaxel sandbox.
FROM {base_image}
COPY --from=ghcr.io/blaxel-ai/sandbox:latest /sandbox-api /usr/local/bin/sandbox-api
ENTRYPOINT ["/usr/local/bin/sandbox-api"]
"#
    )
}

pub fn blaxel_toml(image_name: &str, memory_mb: u32) -> String {
    format!(
        r#"name = "{image_name}"
type = "sandbox"

[runtime]
memory = {memory_mb}

[[runtime.ports]]
name = "tod-relay"
target = {port}
protocol = "HTTP"
"#,
        port = crate::blaxel::RELAY_PORT
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn fork_manifest_tracks_relay_and_cli_not_the_bootstrap() {
        let a = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        let b = Payload { bootstrap: b"changed", relay: b"r", tod_cli: b"t", agents: false };
        let c = Payload { bootstrap: b"a", relay: b"new", tod_cli: b"t", agents: true };
        assert_eq!(fork_manifest(&a), fork_manifest(&b));
        assert_ne!(fork_manifest(&a), fork_manifest(&c));
        assert!(is_fork_manifest(&fork_manifest(&a)));
        assert!(!is_fork_manifest(&a.manifest()));
        assert!(!ready_as_is(Some(&fork_manifest(&a)), &a.manifest()));
    }

    #[test]
    fn requirements_output_is_parsed() {
        let found = parse_requirements("missing: claude-agent-acp node>=20.10\noptional: claude\n");
        assert_eq!(found.missing, ["claude-agent-acp", "node>=20.10"]);
        assert_eq!(found.optional, ["claude"]);
        assert_eq!(parse_requirements("missing:\noptional:\n"), Requirements::default());
        assert!(parse_requirements("missing:\noptional:\nproxy:unrendered\n").proxy_unrendered);
    }

    #[test]
    fn a_missing_requirement_says_how_to_fix_the_source() {
        let msg = missing_in_fork_message(&["claude-agent-acp".to_string()]);
        assert!(msg.contains("claude-agent-acp") && msg.contains("golden refresh") && msg.contains("--install"));
    }

    #[test]
    fn the_requirements_script_checks_agents_only_when_asked() {
        assert!(requirements_script(true).contains("claude-agent-acp"));
        assert!(!requirements_script(false).contains("claude-agent-acp"));
    }

    use super::*;

    #[test]
    fn manifest_changes_with_any_input() {
        let a = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: false };
        let b = Payload { bootstrap: b"b", relay: b"r", tod_cli: b"t", agents: false };
        let c = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        assert_ne!(a.manifest(), b.manifest());
        assert_ne!(a.manifest(), c.manifest());
        assert_eq!(a.manifest(), Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: false }.manifest());
    }

    #[test]
    fn the_relay_replaces_the_shell_that_starts_it() {
        assert_eq!(relay_command(), format!("exec {RELAY_PATH} --port {}", crate::blaxel::RELAY_PORT));
    }

    #[test]
    fn a_node_sandbox_is_ready_as_is() {
        let p = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        let node = node_manifest(b"another relay");
        assert!(is_node_manifest(&node));
        assert!(is_node_manifest(&format!("{node}
")));
        assert!(!is_node_manifest(&p.manifest()));
        assert!(ready_as_is(Some(&node), &p.manifest()));
        assert!(ready_as_is(Some(&p.manifest()), &p.manifest()));
        assert!(!ready_as_is(Some("bootstrap=0 relay=0 tod-cli=0 agents=true"), &p.manifest()));
        assert!(!ready_as_is(None, &p.manifest()));
        assert_ne!(node_manifest(b"r1"), node_manifest(b"r2"));
    }

    #[test]
    fn baked_image_carries_the_manifest() {
        let p = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        let d = bake_dockerfile("ubuntu:24.04", &p);
        assert!(d.contains(&p.manifest()));
        assert!(d.contains("bootstrap.sh --agents"));
    }

    #[test]
    fn repositories_are_read_one_per_line() {
        let repos = parse_bake_repos(
            "# the app and what it builds on\n\nhttps://github.com/acme/app.git\nhttps://github.com/acme/lib /src/lib/\n",
        )
        .unwrap();
        assert_eq!(
            repos,
            vec![
                BakeRepo { url: "https://github.com/acme/app.git".into(), dir: "/root/app".into() },
                BakeRepo { url: "https://github.com/acme/lib".into(), dir: "/src/lib".into() },
            ]
        );
        assert!(parse_bake_repos("").unwrap().is_empty());
    }

    #[test]
    fn repositories_that_would_leak_or_break_the_build_are_refused() {
        for bad in [
            "git@github.com:acme/app.git",
            "http://github.com/acme/app",
            "https://user:token@github.com/acme/app",
            "https://github.com/acme/app; rm -rf /",
            "https://github.com/acme/app relative/dir",
            "https://github.com/acme/app /root/../etc",
            "https://github.com/acme/app /a b",
            "https://github.com/acme/app\nhttps://github.com/other/app",
        ] {
            assert!(parse_bake_repos(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_baked_image_clones_its_repositories_after_installing() {
        let p = Payload { bootstrap: b"a", relay: b"r", tod_cli: b"t", agents: true };
        let repos = parse_bake_repos("https://github.com/acme/app\nhttps://github.com/acme/lib /src/lib").unwrap();
        let d = bake_dockerfile_with_repos("ubuntu:24.04", &p, &repos);
        let install = d.find("bootstrap.sh --agents").unwrap();
        let app = d.find("RUN git clone --quiet https://github.com/acme/app /root/app\n").unwrap();
        let lib = d.find("RUN git clone --quiet https://github.com/acme/lib /src/lib\n").unwrap();
        assert!(install < app && app < lib && lib < d.find("ENTRYPOINT").unwrap());
        assert_eq!(bake_dockerfile("ubuntu:24.04", &p), bake_dockerfile_with_repos("ubuntu:24.04", &p, &[]));
    }
}
