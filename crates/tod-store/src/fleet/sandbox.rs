//! Cloud sandboxes (Blaxel) as tod uses them: the account and sandboxes in
//! `<data root>/sandboxes.toml`, signing in, making a sandbox ready, and
//! running commands in one. `tod-sandbox` (the user's tool) and the app both
//! go through here; the transport is `tod_sandbox`.
//!
//! A node's work runs in a sandbox when its Files capability names one
//! ([`crate::fleet::DevContainerSetting::sandbox`]): git runs there through
//! the relay ([`SandboxExec`]), agents run under the relay and are bridged to
//! this machine by `tod-sandbox agent`, and their `tod-cli` comes back
//! through the relay's tunnel to [`crate::fleet::cli_relay`].

use crate::credentials::{CredentialKind, CredentialStore};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tod_sandbox::blaxel::{Blaxel, NewSandbox};
use tod_sandbox::config::{self, Account, Config, Sandbox};
use tod_sandbox::provision::{self, Payload};
use tod_sandbox::relay;

pub use tod_sandbox::config::{AuthMode, DEFAULT_IMAGE};
pub use tod_sandbox::provision::{TOD_CLI_PATH, TUNNEL_PORT};

/// Installs what tod needs in any image (see `assets/sandbox/`).
pub const BOOTSTRAP: &[u8] = include_bytes!("../../../../assets/sandbox/bootstrap.sh");

/// The file that caches a `bl login` token.
const TOKEN_CACHE: &str = "sandbox-token.json";

/// The account and sandboxes of one data root.
pub struct Sandboxes {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: Config,
}

impl Sandboxes {
    pub fn load(root: &Path) -> Result<Self> {
        let config_path = root.join(config::FILE_NAME);
        let config = Config::load(&config_path)?;
        Ok(Self { root: root.to_path_buf(), config_path, config })
    }

    pub fn save(&self) -> Result<()> {
        self.config.save(&self.config_path)
    }

    pub fn account(&self) -> Result<&Account> {
        self.config
            .blaxel
            .as_ref()
            .filter(|acct| !acct.workspace.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "cloud sandboxes are not set up: set the Blaxel workspace in \
                     Settings → Cloud sandboxes, or run `tod-sandbox setup`"
                )
            })
    }

    pub fn credentials(&self) -> CredentialStore {
        CredentialStore::from_data_root(&self.root)
    }

    pub fn token(&self) -> Result<String> {
        let acct = self.account()?;
        match acct.auth {
            AuthMode::ApiKey => self.credentials().get(CredentialKind::BlaxelApiKey).ok_or_else(|| {
                anyhow!(
                    "no Blaxel API key stored: enter one in Settings → Cloud sandboxes,                      or run `tod-sandbox setup --auth api-key`"
                )
            }),
            AuthMode::Bl => bl_token(&self.root, &acct.workspace),
        }
    }

    /// Forget a cached `bl` token (after signing in again).
    pub fn forget_token(&self) {
        let _ = std::fs::remove_file(self.root.join(TOKEN_CACHE));
    }

    pub fn blaxel(&self) -> Result<Blaxel> {
        Ok(Blaxel::new(self.account()?.workspace.clone(), self.token()?))
    }

    /// The sandbox's URL, from the config or else the control plane (neither
    /// wakes it).
    pub fn url(&mut self, bx: &Blaxel, name: &str) -> Result<String> {
        if let Some(url) = self.config.sandbox(name).and_then(|s| s.url.clone()) {
            return Ok(url);
        }
        let info = bx.get(name)?.ok_or_else(|| anyhow!("no sandbox named {name}"))?;
        let url = info
            .url
            .clone()
            .ok_or_else(|| anyhow!("sandbox {name} has no URL yet ({})", info.status))?;
        let mut entry = self.config.sandbox(name).cloned().unwrap_or(Sandbox {
            name: name.to_string(),
            image: info.image.clone(),
            url: None,
            agents: false,
        });
        entry.url = Some(url.clone());
        self.config.upsert(entry);
        self.save()?;
        Ok(url)
    }

    /// Make the sandbox ready for tod (idempotent; one round trip when it
    /// is) and return its URL. `progress` hears about each slow step.
    pub fn ensure(&mut self, bx: &Blaxel, name: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
        let info = bx.get(name)?.ok_or_else(|| anyhow!("no sandbox named {name}"))?;
        let info = if info.status == "DEPLOYED" {
            info
        } else {
            bx.wait_deployed(name, Duration::from_secs(180))?
        };
        let url = info.url.clone().unwrap_or_default();
        let agents = self.config.sandbox(name).is_some_and(|s| s.agents);
        let relay_bin = relay_binary()?;
        let payload = payload(&relay_bin, agents);
        let started = Instant::now();
        let outcome = provision::ensure(bx, &url, &payload, progress)?;
        if outcome != provision::Outcome::AlreadyReady {
            progress(&format!("{name}: ready ({outcome:?}, {:.1}s)", started.elapsed().as_secs_f64()));
        }
        let mut entry = self.config.sandbox(name).cloned().unwrap_or(Sandbox {
            name: name.to_string(),
            image: info.image,
            url: None,
            agents,
        });
        if entry.url.as_deref() != Some(url.as_str()) {
            entry.url = Some(url.clone());
            self.config.upsert(entry);
            self.save()?;
        }
        Ok(url)
    }
}

/// What a new sandbox starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewSandboxSource {
    /// A cold start from an image; empty means the account's default. An
    /// image not built for Blaxel is wrapped first (a build on Blaxel, with
    /// `bl`).
    Image(String),
    /// A copy of another sandbox's current state (Blaxel's fork; not every
    /// workspace has it).
    Fork(String),
}

impl Sandboxes {
    /// Create sandbox `name` from `source`, wait for it, and make it ready
    /// for tod. Returns its URL. Slow (seconds for a fork or a baked image,
    /// a minute or more when an image is wrapped and set up): never on the UI
    /// thread. `inherit_output` shows `bl`'s build output on this process's
    /// terminal; otherwise it is only reported on failure.
    pub fn create(
        &mut self,
        name: &str,
        source: &NewSandboxSource,
        agents: bool,
        inherit_output: bool,
        progress: &mut dyn FnMut(&str),
    ) -> Result<String> {
        validate_name(name)?;
        let acct = self.account()?.clone();
        let bx = self.blaxel()?;
        if let Some(info) = bx.get(name)?
            && info.status != "TERMINATED"
        {
            bail!("a sandbox named {name} already exists ({})", info.status);
        }
        let started = Instant::now();
        let image = match source {
            NewSandboxSource::Image(image) => {
                let image = match image.trim() {
                    "" => acct.default_image.as_str(),
                    image => image,
                };
                let runtime_image = if runs_as_is(image) {
                    image.to_string()
                } else {
                    let wrapped = format!("tod-{}", label_value(short_image(image)));
                    progress(&format!(
                        "{image} is not built for Blaxel; wrapping it as sandbox/{wrapped} \
                         (adds Blaxel's sandbox API)…"
                    ));
                    let dir = build_dir(&wrapped)?;
                    std::fs::write(dir.join("Dockerfile"), provision::wrap_dockerfile(image))?;
                    std::fs::write(
                        dir.join("blaxel.toml"),
                        provision::blaxel_toml(&wrapped, acct.memory_mb),
                    )?;
                    self.bl_push(&dir, inherit_output, progress)?;
                    format!("sandbox/{wrapped}:latest")
                };
                let owner = acct.owner.as_deref().map(label_value).unwrap_or_default();
                let mut labels = vec![("tod", "1")];
                if !owner.is_empty() {
                    labels.push(("tod-owner", owner.as_str()));
                }
                progress(&format!("creating {name} from {runtime_image}…"));
                bx.create(&NewSandbox {
                    name,
                    image: &runtime_image,
                    region: &acct.region,
                    memory_mb: acct.memory_mb,
                    labels: &labels,
                })?;
                runtime_image
            }
            NewSandboxSource::Fork(source) => {
                validate_name(source)?;
                let image = bx
                    .get(source)?
                    .ok_or_else(|| anyhow!("no sandbox named {source} to fork"))?
                    .image;
                progress(&format!("forking {source} into {name}…"));
                bx.fork(source, name)?;
                image
            }
        };
        let info = bx.wait_deployed(name, Duration::from_secs(300))?;
        progress(&format!("{name}: deployed in {:.1}s", started.elapsed().as_secs_f64()));
        self.config.upsert(Sandbox { name: name.to_string(), image, url: info.url, agents });
        self.save()?;
        forget(name);
        self.ensure(&bx, name, progress)
    }

    /// Build `dir` (a Dockerfile and blaxel.toml) into the workspace's
    /// registry. The build runs on Blaxel; no local Docker is needed.
    pub fn bl_push(
        &self,
        dir: &Path,
        inherit_output: bool,
        progress: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let acct = self.account()?;
        let mut cmd = Command::new("bl");
        // `bl` does not find blaxel.toml through `-d` with a Windows path; run it there.
        cmd.args(["-w", &acct.workspace, "push", "--skip-version-warning"]).current_dir(dir);
        if acct.auth == AuthMode::ApiKey {
            cmd.env("BL_API_KEY", self.token()?).env("BL_WORKSPACE", &acct.workspace);
        }
        let missing = || {
            anyhow!(
                "building an image needs the Blaxel CLI (`bl`); \
                 see https://docs.blaxel.ai/cli-reference/introduction"
            )
        };
        let started = Instant::now();
        let (status, detail) = if inherit_output {
            (cmd.status().map_err(|_| missing())?, String::new())
        } else {
            no_window(&mut cmd);
            let out = cmd.output().map_err(|_| missing())?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines[lines.len().saturating_sub(15)..].join("\n");
            (out.status, tail)
        };
        if !status.success() {
            bail!(
                "`bl push` failed ({status}); the build context is in {}\n{detail}",
                dir.display()
            );
        }
        progress(&format!("built in {}s", started.elapsed().as_secs()));
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }
}

/// A fresh directory to build an image in.
pub fn build_dir(name: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("tod-sandbox-build-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A label value Blaxel accepts: lowercase alphanumerics and dashes.
pub fn label_value(s: &str) -> String {
    let v: String = s
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    v.trim_matches('-').chars().take(63).collect()
}

/// `ubuntu:24.04` for `docker.io/library/ubuntu:24.04`, for naming images after it.
pub fn short_image(image: &str) -> &str {
    let image = image.strip_prefix("docker.io/").unwrap_or(image);
    image.strip_prefix("library/").unwrap_or(image)
}

/// Images Blaxel can run as they are: its own and ones built in the workspace.
pub fn runs_as_is(image: &str) -> bool {
    image.starts_with("blaxel/") || image.starts_with("sandbox/")
}

/// A sandbox in the workspace, for choosing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSandbox {
    pub name: String,
    /// Whether it is running or in standby (`RUNNING`, `STANDBY`, …) when
    /// Blaxel says, else its deployment status (`DEPLOYED`, …).
    pub status: String,
    pub image: String,
    /// Who created it (the `tod-owner` label).
    pub owner: Option<String>,
    /// In this data root's `sandboxes.toml`.
    pub known: bool,
}

/// Every sandbox in the workspace, by name. Asks Blaxel (which does not wake
/// any): never on the UI thread.
pub fn list(root: &Path) -> Result<Vec<ListedSandbox>> {
    let sandboxes = Sandboxes::load(root)?;
    let mut all: Vec<ListedSandbox> = sandboxes
        .blaxel()?
        .list()?
        .into_iter()
        .filter(|info| !matches!(info.status.as_str(), "TERMINATED" | "DELETING"))
        .map(|info| ListedSandbox {
            status: info.state_or_status().to_string(),
            owner: info.label("tod-owner").map(str::to_string),
            known: sandboxes.config.sandbox(&info.name).is_some(),
            name: info.name,
            image: info.image,
        })
        .collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

/// The Blaxel workspace and the image new sandboxes start from, as set up
/// in this data root. Reads the file only.
pub fn account_settings(root: &Path) -> (String, String) {
    let acct = Sandboxes::load(root).ok().and_then(|s| s.config.blaxel);
    match acct {
        Some(acct) => (acct.workspace, acct.default_image),
        None => (String::new(), config::DEFAULT_IMAGE.to_string()),
    }
}

/// Record the workspace and the default image. How to sign in carries over
/// (an API key when nothing was set up). An empty image means Blaxel's base
/// image.
pub fn set_account_settings(root: &Path, workspace: &str, default_image: &str) -> Result<()> {
    let mut sandboxes = Sandboxes::load(root)?;
    let workspace = workspace.trim();
    let default_image = match default_image.trim() {
        "" => config::DEFAULT_IMAGE,
        image => image,
    };
    let previous = sandboxes.config.blaxel.take();
    let auth = previous.as_ref().map_or(AuthMode::ApiKey, |acct| acct.auth);
    let mut acct = match previous {
        // Clearing the workspace, or setting it again, keeps the rest.
        Some(acct) if workspace.is_empty() || acct.workspace.is_empty() || acct.workspace == workspace => acct,
        _ => Account { auth, ..Account::new(workspace) },
    };
    if acct.workspace != workspace {
        sandboxes.forget_token();
    }
    acct.workspace = workspace.to_string();
    acct.default_image = default_image.to_string();
    sandboxes.config.blaxel = Some(acct);
    sandboxes.save()
}

/// How this data root signs in to Blaxel (an API key when nothing is set up
/// yet). Reads the file only.
pub fn sign_in_mode(root: &Path) -> AuthMode {
    Sandboxes::load(root).ok().and_then(|s| s.config.blaxel).map_or(AuthMode::ApiKey, |acct| acct.auth)
}

/// Whether a Blaxel API key is stored. Reads the OS keyring, which can
/// prompt: never on the UI thread.
pub fn has_api_key(root: &Path) -> bool {
    CredentialStore::from_data_root(root).get(CredentialKind::BlaxelApiKey).is_some()
}

/// Sign in with `auth` from now on, storing `api_key` when one is given (in
/// the credential store, as a kind agents cannot read). Touches the keyring:
/// never on the UI thread.
pub fn set_sign_in(root: &Path, auth: AuthMode, api_key: Option<&str>) -> Result<()> {
    let mut sandboxes = Sandboxes::load(root)?;
    if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
        sandboxes
            .credentials()
            .set(CredentialKind::BlaxelApiKey, key)
            .map_err(|e| anyhow!("could not store the API key: {e}"))?;
    }
    sandboxes.config.blaxel.get_or_insert_with(|| Account::new("")).auth = auth;
    sandboxes.forget_token();
    sandboxes.save()
}

/// Sign in and count the workspace's sandboxes, to show that the sign-in
/// works. Network, and maybe `bl`: never on the UI thread.
pub fn check_sign_in(root: &Path) -> Result<usize> {
    Ok(Sandboxes::load(root)?.blaxel()?.list()?.len())
}

/// A name for a new sandbox for the node `node_slug`.
pub fn suggested_name(node_slug: &str) -> String {
    let name: String = label_value(node_slug).chars().take(48).collect();
    let name = name.trim_matches('-');
    if name.is_empty() { "sandbox".into() } else { name.to_string() }
}

/// What gets installed into a sandbox.
pub fn payload(relay_bin: &[u8], agents: bool) -> Payload<'_> {
    Payload {
        bootstrap: BOOTSTRAP,
        relay: relay_bin,
        tod_cli: crate::fleet::cli_relay::SHIM_SCRIPT.as_bytes(),
        agents,
    }
}

/// A `bl login` token, cached until five minutes before it expires (asking
/// `bl` costs a quarter second, and Zed and git connect often).
fn bl_token(root: &Path, workspace: &str) -> Result<String> {
    let cache = root.join(TOKEN_CACHE);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    if let Ok(text) = std::fs::read_to_string(&cache) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if v["workspace"] == workspace {
                if let Some(token) = v["token"].as_str() {
                    if tod_sandbox::blaxel::jwt_expiry(token).is_some_and(|exp| exp > now + 300) {
                        return Ok(token.to_string());
                    }
                }
            }
        }
    }
    let mut command = Command::new("bl");
    command.args(["-w", workspace, "token", "--skip-version-warning"]);
    no_window(&mut command);
    let out = command.output().map_err(|_| {
        anyhow!(
            "signing in with `bl login` needs the Blaxel CLI (`bl`), which is not installed;              install it (https://docs.blaxel.ai/cli-reference/introduction), or sign in with              an API key in Settings → Cloud sandboxes"
        )
    })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let token = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default()
        .to_string();
    if !out.status.success() || token.is_empty() || token.contains(' ') {
        bail!(
            "could not get a Blaxel token; run `bl login {workspace}`, or sign in with an API              key in Settings → Cloud sandboxes ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let _ = std::fs::write(&cache, serde_json::json!({ "workspace": workspace, "token": token }).to_string());
    Ok(token)
}

#[cfg(windows)]
pub(crate) fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn no_window(_command: &mut Command) {}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

/// A tod executable installed next to the running one (`tod-sandbox`,
/// `tod-zed-shim`).
pub fn sibling_exe(name: &str) -> PathBuf {
    exe_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// The Linux relay binary: `TOD_RELAY_BIN`, the install's `sandbox/tod-relay`,
/// or, in a dev build, the cross-compiled one in `target/`.
pub fn relay_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("TOD_RELAY_BIN") {
        return Ok(PathBuf::from(p));
    }
    let dir = exe_dir();
    let installed = dir.join("sandbox").join("tod-relay");
    if installed.is_file() {
        return Ok(installed);
    }
    for ancestor in dir.ancestors() {
        let dev = ancestor.join("x86_64-unknown-linux-musl").join("release").join("tod-relay");
        if dev.is_file() {
            return Ok(dev);
        }
    }
    bail!(
        "tod-relay not found ({}); build it with \
         `cargo build --release -p tod-relay --target x86_64-unknown-linux-musl`",
        installed.display()
    )
}

pub fn relay_binary() -> Result<Vec<u8>> {
    let path = relay_path()?;
    std::fs::read(&path).with_context(|| format!("read {}", path.display()))
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// Where a sandbox is reached, once this process has made sure it is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub url: String,
    pub token: String,
}

/// The data root the app's sandboxes are read from. Set once at startup by
/// the app (and in tests); `TOD_DATA_ROOT` otherwise.
static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn set_data_root(root: &Path) {
    let _ = DATA_ROOT.set(root.to_path_buf());
}

fn data_root() -> Result<PathBuf> {
    if let Some(root) = DATA_ROOT.get() {
        return Ok(root.clone());
    }
    std::env::var_os("TOD_DATA_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("no data root for cloud sandboxes"))
}

/// Sandboxes this process has made ready, with when.
type Ready = HashMap<String, (Connection, Instant)>;

fn ready() -> &'static Mutex<Ready> {
    static READY: OnceLock<Mutex<Ready>> = OnceLock::new();
    READY.get_or_init(Default::default)
}

/// How long a check that a sandbox is ready stands (its relay could restart
/// after that, and a token expires).
const READY_TTL: Duration = Duration::from_secs(600);

/// The sandbox `name`, made ready for tod. Cached for a while, so a burst of
/// git commands costs one check. Talks to Blaxel: never on the UI thread.
pub fn connect(name: &str) -> Result<Connection> {
    if let Some((conn, at)) = ready().lock().expect("sandbox cache").get(name) {
        if at.elapsed() < READY_TTL {
            return Ok(conn.clone());
        }
    }
    let mut sandboxes = Sandboxes::load(&data_root()?)?;
    let bx = sandboxes.blaxel()?;
    let url = sandboxes.ensure(&bx, name, &mut |m| tracing::info!(sandbox = name, "{m}"))?;
    let conn = Connection { url, token: bx.token().to_string() };
    ready()
        .lock()
        .expect("sandbox cache")
        .insert(name.to_string(), (conn.clone(), Instant::now()));
    Ok(conn)
}

fn forget(name: &str) {
    ready().lock().expect("sandbox cache").remove(name);
}

/// Runs commands in a sandbox through its relay, the way a process tod
/// started there would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxExec {
    pub name: String,
}

impl SandboxExec {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Run `program args…` in `dir` and collect its output. A relay that
    /// cannot be reached is made ready again, once.
    pub fn output(&self, dir: &str, program: &str, args: &[&str]) -> Result<Output> {
        let mut cmd = format!("cd {} && exec {}", relay::shell_quote(dir), relay::shell_quote(program));
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&relay::shell_quote(arg));
        }
        let run = |conn: &Connection| {
            block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(600),
                    relay::run_capture(&relay::ws_url(&conn.url, "/exec"), &conn.token, &cmd),
                )
                .await
                .map_err(|_| anyhow!("timed out in sandbox {}: {program}", self.name))?
            })
        };
        let conn = connect(&self.name)?;
        let (code, stdout, stderr) = match run(&conn) {
            Ok(result) => result,
            Err(_) => {
                forget(&self.name);
                run(&connect(&self.name)?)?
            }
        };
        Ok(Output { status: exit_status(code), stdout, stderr: stderr.into_bytes() })
    }

    /// Whether `path` is a directory in the sandbox.
    pub fn is_dir(&self, path: &str) -> Result<bool> {
        Ok(self.output("/", "test", &["-d", path])?.status.success())
    }

    /// Where `program` is in the sandbox: on the relay's `PATH`, else on the
    /// one a login shell sets up.
    pub fn find_program(&self, program: &str) -> Result<Option<String>> {
        const FIND: &str = r#"command -v "$1""#;
        for (shell, flag) in [("sh", "-c"), ("sh", "-lc")] {
            let Ok(out) = self.output("/", shell, &[flag, FIND, shell, program]) else {
                continue;
            };
            if !out.status.success() {
                continue;
            }
            let found = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .rfind(|line| line.starts_with('/'))
                .map(str::to_string);
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw((code & 0xff) << 8)
}

#[cfg(windows)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code as u32)
}

/// The sandboxes this data root knows, for choosing one. Reads the file only.
pub fn known(root: &Path) -> Vec<String> {
    Sandboxes::load(root)
        .map(|s| s.config.sandboxes.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default()
}

/// A sandbox name is written into URLs and shell commands, so only Blaxel's
/// own name characters are accepted.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if !ok {
        bail!("sandbox names are lowercase letters, digits, and dashes (at most 48): {name:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_names_are_valid() {
        for slug in ["fix-login", "Big Task: v2!", "", "---", &"x".repeat(80)] {
            let name = suggested_name(slug);
            assert!(validate_name(&name).is_ok(), "{slug:?} -> {name:?}");
        }
        assert_eq!(suggested_name("fix-login"), "fix-login");
    }

    #[test]
    fn account_settings_round_trip() {
        let root = std::env::temp_dir().join(format!("tod-sbx-acct-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(account_settings(&root), (String::new(), config::DEFAULT_IMAGE.to_string()));
        assert!(Sandboxes::load(&root).unwrap().account().is_err());
        set_account_settings(&root, " team ", "sandbox/baked:latest").unwrap();
        // Nothing set up before: an API key.
        assert_eq!(sign_in_mode(&root), AuthMode::ApiKey);
        assert_eq!(account_settings(&root), ("team".into(), "sandbox/baked:latest".into()));
        // An empty image goes back to the default; the account stays.
        set_account_settings(&root, "team", "").unwrap();
        assert_eq!(account_settings(&root).1, config::DEFAULT_IMAGE);
        assert_eq!(Sandboxes::load(&root).unwrap().account().unwrap().workspace, "team");
        // No workspace is not set up, whatever else is there.
        set_account_settings(&root, "", "x").unwrap();
        assert!(Sandboxes::load(&root).unwrap().account().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exit_codes_round_trip() {
        assert_eq!(exit_status(0).code(), Some(0));
        assert_eq!(exit_status(3).code(), Some(3));
        assert!(!exit_status(128).success());
    }

    #[test]
    fn names_are_checked() {
        assert!(validate_name("tod-dev-1").is_ok());
        for bad in ["", "-x", "Dev", "a b", "a;b", "a/b"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    /// Against a real sandbox: `TOD_TEST_SANDBOX` (its name) and
    /// `TOD_TEST_SANDBOX_ROOT` (a data root set up with `tod-sandbox setup`),
    /// with `tod-sandbox` and `tod-cli` built. Git runs there through the
    /// relay, and an agent started the way the app starts one reaches this
    /// machine's `tod-cli` through the tunnel.
    #[test]
    fn a_sandbox_runs_git_and_bridges_an_agent_with_tod_cli() {
        use crate::fleet::{Workdir, cli_relay};
        use std::io::{BufRead, BufReader, Write};

        let (Ok(name), Ok(root)) = (
            std::env::var("TOD_TEST_SANDBOX"),
            std::env::var("TOD_TEST_SANDBOX_ROOT"),
        ) else {
            eprintln!("skipped: set TOD_TEST_SANDBOX and TOD_TEST_SANDBOX_ROOT");
            return;
        };
        let root = std::fs::canonicalize(root).unwrap();
        set_data_root(&root);

        let repo = "/root/tod-e2e-repo";
        let exec = SandboxExec::new(&name);
        let setup = format!(
            "rm -rf {repo} && mkdir -p {repo} && cd {repo} && git init -q -b main \
             && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init"
        );
        let out = exec.output("/", "sh", &["-c", &setup]).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let dir = Workdir::sandbox(&name, repo);
        let top = dir.output("git", &["rev-parse", "--show-toplevel"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&top.stdout).trim(), repo);

        // The built binaries: in a directory above this test's.
        let exe_name = |n: &str| format!("{n}{}", std::env::consts::EXE_SUFFIX);
        let test_exe = std::env::current_exe().unwrap();
        let bin_dir = test_exe
            .ancestors()
            .find(|dir| dir.join(exe_name("tod-sandbox")).is_file())
            .expect("build tod-sandbox and tod-cli first")
            .to_path_buf();
        let exe = |n: &str| bin_dir.join(exe_name(n));
        let relay = cli_relay::start(root.clone(), exe("tod-cli")).unwrap();
        let launch = tod_agent::sandbox::SandboxLaunch {
            launcher: exe("tod-sandbox"),
            data_root: root.clone(),
            sandbox: name.clone(),
            directory: repo.into(),
            env: vec![("TOD_E2E".into(), "yes".into())],
            cli_relay: relay.env(),
        };
        // How the provider finds the agent to start there.
        assert!(launch.find_program(&["sh"]).unwrap().is_some_and(|p| p.starts_with('/')));
        assert_eq!(launch.find_program(&["no-such-program-tod"]).unwrap(), None);
        let script = r#"while read -r line; do printf 'cli:%s\n' "$(tod-cli help 2>&1 | head -1)"; echo "got $line $TOD_E2E $(pwd)"; done"#;
        let mut child = launch
            .agent_command("sh", &["-c".into(), script.into()], &[])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "hello").unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let cli = lines.next().unwrap().unwrap();
        let got = lines.next().unwrap().unwrap();
        assert_eq!(got, format!("got hello yes {repo}"));
        assert!(cli.starts_with("cli:") && cli.contains("tod-cli"), "{cli}");
        drop(stdin);
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn the_payload_carries_the_tod_cli_script() {
        let p = payload(b"relay", true);
        assert!(std::str::from_utf8(p.tod_cli).unwrap().contains("tod-cli-relay 1"));
        assert_ne!(p.manifest(), payload(b"relay", false).manifest());
    }
}
