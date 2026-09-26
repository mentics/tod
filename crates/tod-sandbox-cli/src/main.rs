//! `tod-sandbox`: cloud sandboxes for tod, from the command line.
//!
//! Everything it knows lives in the data root: `sandboxes.toml` (the account
//! and the sandboxes), the credential store (a Blaxel API key, if one is used),
//! and `sandbox-token.json` (a cached `bl login` token). The data root is only
//! ever read from `--data-root`, `TOD_DATA_ROOT`, or install.toml; this tool
//! never writes install.toml.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::config::{self, Account, AuthMode};
use tod_sandbox::provision;
use tod_sandbox::relay::{self, ExecRequest};
use tod_sandbox::terminal::{self, TerminalOptions};
use tod_store::credentials::CredentialKind;
use tod_store::fleet::cli_relay;
use tod_store::fleet::sandbox::{self as sandboxes, BOOTSTRAP, NewSandboxSource, Sandboxes};

const USAGE: &str = "\
tod-sandbox: cloud sandboxes for tod (Blaxel)

usage: tod-sandbox [--data-root DIR] <command> [args]

setup [--workspace W] [--auth bl|api-key] [--api-key-stdin] [--region R]
      [--image IMAGE] [--memory MB] [--owner NAME]
                         Choose the Blaxel workspace and how to sign in to it:
                         an API key (the default; prompted for, or on stdin) or
                         your own `bl login`.
create <name> [--image IMAGE] [--agents]
                         Create a sandbox and install what tod needs in it. Any
                         image works: one not built for Blaxel is wrapped first.
fork <source> <name> [--agents]
                         Create a sandbox as a copy of another one's current
                         state (it may be in standby). Needs a Blaxel workspace
                         with forking.
ensure <name>            Install or update tod's pieces in a sandbox (idempotent).
bake <base-image> [--name IMAGE-NAME] [--agents]
                         Build an image with everything preinstalled.
list                     Sandboxes in the workspace.
status <name>            Whether a sandbox is running or in standby, and its
                         deployment status (does not wake it).
exec <name> [--keep-awake] [--cwd DIR] -- <command...>
                         Run a command in the sandbox.
shell <name> [--cwd DIR] [--park SECS] [--run CMD] [--cli-relay]
      [--cli-relay-file FILE] [--env NAME]...
                         Interactive shell; parks when idle so the sandbox sleeps.
                         --run runs CMD first; the shell stays after it.
agent <name> --name AGENT [--cwd DIR] [--idle SECS] [--cli-relay] [--env NAME]...
      -- <command...>    A line-oriented (ACP) agent in the sandbox over this
                         process's stdio; detaches when idle so the sandbox
                         sleeps. What tod runs its agents in a sandbox with.
                         --cli-relay: `tod-cli` there reaches the tod that set
                         TOD_CLI_RELAY_PORT and TOD_CLI_RELAY_TOKEN here.
                         --env NAME passes this process's NAME (or NAME=VALUE).
zed <name> [PATH]        Open PATH (default /root) in Zed on the sandbox.
delete <name>            Delete a sandbox.
doctor                   Check the setup.
connect-info <name>      (internal) The relay URL and token, as JSON.
";

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("tod-sandbox: {err:#}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Arguments

struct Args(Vec<String>);

impl Args {
    fn opt(&mut self, name: &str) -> Option<String> {
        let prefix = format!("{name}=");
        let i = self.0.iter().position(|a| a == name || a.starts_with(&prefix))?;
        let a = self.0.remove(i);
        if let Some(v) = a.strip_prefix(&prefix) {
            return Some(v.to_string());
        }
        (i < self.0.len()).then(|| self.0.remove(i))
    }

    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
        }
    }

    fn positional(&mut self, what: &str) -> Result<String> {
        let i = self
            .0
            .iter()
            .position(|a| !a.starts_with("--"))
            .ok_or_else(|| anyhow!("missing {what}\n\n{USAGE}"))?;
        Ok(self.0.remove(i))
    }

    fn done(&self) -> Result<()> {
        if let Some(extra) = self.0.first() {
            bail!("unexpected argument {extra:?}\n\n{USAGE}");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Context

type Ctx = Sandboxes;

fn load(flag: Option<String>) -> Result<Ctx> {
    let root = tod_store::paths::resolve_startup_data_root(flag.as_deref().map(Path::new)).ok_or_else(|| {
        anyhow!("no data root: pass --data-root DIR or set TOD_DATA_ROOT (or run tod once to choose one)")
    })?;
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    Sandboxes::load(&root)
}

fn ensure(ctx: &mut Ctx, bx: &Blaxel, name: &str) -> Result<String> {
    ctx.ensure(bx, name, &mut |m| eprintln!("{m}"))
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(f)
}

fn prompt(question: &str) -> Result<String> {
    eprint!("{question}: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

// ---------------------------------------------------------------------------
// Commands

fn run() -> Result<i32> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // Everything after `--` belongs to the remote command.
    let (raw, remote) = match raw.iter().position(|a| a == "--") {
        Some(i) => (raw[..i].to_vec(), raw[i + 1..].to_vec()),
        None => (raw, Vec::new()),
    };
    let mut args = Args(raw);
    let data_root = args.opt("--data-root");
    if args.0.is_empty() || args.flag("--help") || args.flag("-h") || args.0[0] == "help" {
        print!("{USAGE}");
        return Ok(0);
    }
    let cmd = args.0.remove(0);
    let mut ctx = load(data_root)?;
    match cmd.as_str() {
        "setup" | "login" => setup(&mut ctx, args),
        "create" => create(&mut ctx, args),
        "fork" => fork(&mut ctx, args),
        "ensure" => {
            let name = args.positional("sandbox name")?;
            args.done()?;
            let bx = ctx.blaxel()?;
            ensure(&mut ctx, &bx, &name)?;
            println!("{name}: ready");
            Ok(0)
        }
        "bake" => bake(&mut ctx, args),
        "list" => list(&ctx),
        "status" => {
            let name = args.positional("sandbox name")?;
            args.done()?;
            match ctx.blaxel()?.get(&name)? {
                Some(info) => println!(
                    "{name}: {} ({}, {})",
                    info.state.as_deref().unwrap_or("state unknown"),
                    info.status,
                    info.image
                ),
                None => println!("{name}: does not exist"),
            }
            Ok(0)
        }
        "exec" => exec(&mut ctx, args, remote),
        "shell" => shell(&mut ctx, args),
        "agent" => agent(&mut ctx, args, remote),
        "zed" => zed(&mut ctx, args),
        "delete" => {
            let name = args.positional("sandbox name")?;
            args.done()?;
            ctx.blaxel()?.delete(&name)?;
            ctx.config.sandboxes.retain(|s| s.name != name);
            ctx.save()?;
            println!("{name}: deleted");
            Ok(0)
        }
        "doctor" => doctor(&mut ctx),
        "connect-info" => {
            let target = args.positional("sandbox name")?;
            args.done()?;
            let name = config::name_for_host(&target).unwrap_or(&target).to_string();
            let bx = ctx.blaxel()?;
            let url = ctx.url(&bx, &name)?;
            println!("{}", serde_json::json!({ "url": url, "token": bx.token() }));
            Ok(0)
        }
        other => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

fn setup(ctx: &mut Ctx, mut args: Args) -> Result<i32> {
    let interactive = std::io::stdin().is_terminal();
    let existing = ctx.config.blaxel.clone();
    let workspace = match args.opt("--workspace").or_else(|| existing.as_ref().map(|a| a.workspace.clone())) {
        Some(w) => w,
        None if interactive => prompt("Blaxel workspace")?,
        None => bail!("--workspace is required"),
    };
    if workspace.is_empty() {
        bail!("a workspace is required");
    }
    let bl_installed = Command::new("bl").arg("version").output().is_ok();
    let auth = match args.opt("--auth").as_deref() {
        Some("bl") => AuthMode::Bl,
        Some("api-key") => AuthMode::ApiKey,
        Some(other) => bail!("--auth is bl or api-key, not {other:?}"),
        None => existing.as_ref().map_or(AuthMode::ApiKey, |a| a.auth),
    };
    let key_from_stdin = args.flag("--api-key-stdin");
    let defaults = Account {
        workspace: workspace.clone(),
        region: config::DEFAULT_REGION.into(),
        auth,
        default_image: "blaxel/base-image:latest".into(),
        memory_mb: 4096,
        owner: std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok(),
    };
    let base = existing.filter(|a| a.workspace == workspace).unwrap_or(defaults);
    let acct = Account {
        workspace,
        auth,
        region: args.opt("--region").unwrap_or(base.region),
        default_image: args.opt("--image").unwrap_or(base.default_image),
        memory_mb: match args.opt("--memory") {
            Some(m) => m.parse().context("--memory is in MB")?,
            None => base.memory_mb,
        },
        owner: args.opt("--owner").or(base.owner),
    };
    args.done()?;

    if auth == AuthMode::ApiKey {
        let store = ctx.credentials();
        let key = if key_from_stdin {
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            Some(line.trim().to_string())
        } else if store.get(CredentialKind::BlaxelApiKey).is_some() && !interactive {
            None
        } else if interactive {
            let hint = if store.get(CredentialKind::BlaxelApiKey).is_some() { " (Enter keeps the stored one)" } else { "" };
            let k = rpassword::prompt_password(format!("Blaxel API key{hint}: "))?;
            (!k.trim().is_empty()).then(|| k.trim().to_string())
        } else {
            bail!("no API key: pass it on stdin with --api-key-stdin");
        };
        if let Some(key) = key {
            let backend = store.set(CredentialKind::BlaxelApiKey, &key).map_err(|e| anyhow!("{e}"))?;
            eprintln!("stored the API key ({backend:?})");
        }
    } else if !bl_installed {
        bail!("the Blaxel CLI (`bl`) is not installed; install it, or use --auth api-key");
    }

    ctx.config.blaxel = Some(acct.clone());
    ctx.save()?;
    ctx.forget_token();
    let token = match ctx.token() {
        Ok(t) => t,
        Err(e) if auth == AuthMode::Bl && interactive => {
            eprintln!("{e:#}\nsigning in with `bl login {}`…", acct.workspace);
            let ok = Command::new("bl").args(["login", &acct.workspace]).status()?.success();
            if !ok {
                bail!("`bl login` failed");
            }
            ctx.token()?
        }
        Err(e) => return Err(e),
    };
    let count = Blaxel::new(acct.workspace.clone(), token).list()?.len();
    println!(
        "set up: workspace {} ({:?} sign-in), region {}, {count} sandbox(es) there; settings in {}",
        acct.workspace,
        acct.auth,
        acct.region,
        ctx.config_path.display()
    );
    Ok(0)
}

fn create(ctx: &mut Ctx, mut args: Args) -> Result<i32> {
    let name = args.positional("sandbox name")?;
    let agents = args.flag("--agents");
    let image = args.opt("--image").unwrap_or_default();
    args.done()?;
    ctx.create(&name, &NewSandboxSource::Image(image), agents, true, &mut |m| eprintln!("{m}"))?;
    println!("{name}: ready. Open it with `tod-sandbox zed {name}` or `tod-sandbox shell {name}`.");
    Ok(0)
}

fn fork(ctx: &mut Ctx, mut args: Args) -> Result<i32> {
    let source = args.positional("sandbox to fork")?;
    let name = args.positional("new sandbox name")?;
    let agents = args.flag("--agents")
        || ctx.config.sandbox(&source).is_some_and(|s| s.agents);
    args.done()?;
    ctx.create(&name, &NewSandboxSource::Fork(source), agents, true, &mut |m| eprintln!("{m}"))?;
    println!("{name}: ready. Open it with `tod-sandbox zed {name}` or `tod-sandbox shell {name}`.");
    Ok(0)
}

fn bake(ctx: &mut Ctx, mut args: Args) -> Result<i32> {
    let base = args.positional("base image")?;
    let agents = args.flag("--agents");
    let name = args
        .opt("--name")
        .unwrap_or_else(|| format!("tod-baked-{}", sandboxes::label_value(sandboxes::short_image(&base))));
    args.done()?;
    let acct = ctx.account()?.clone();
    let relay_bin = sandboxes::relay_binary()?;
    let payload = sandboxes::payload(&relay_bin, agents);
    let dir = sandboxes::build_dir(&name)?;
    std::fs::write(dir.join("Dockerfile"), provision::bake_dockerfile(&base, &payload))?;
    std::fs::write(dir.join("bootstrap.sh"), BOOTSTRAP)?;
    std::fs::write(dir.join("tod-relay"), &relay_bin)?;
    std::fs::write(dir.join("tod-cli"), payload.tod_cli)?;
    std::fs::write(dir.join("blaxel.toml"), provision::blaxel_toml(&name, acct.memory_mb))?;
    eprintln!("baking {base} with tod's dependencies into sandbox/{name}…");
    ctx.bl_push(&dir, true, &mut |m| eprintln!("{m}"))?;
    let flag = if agents { " --agents" } else { "" };
    println!(
        "built sandbox/{name}:latest. Create sandboxes from it with\n  tod-sandbox create <name> --image sandbox/{name}:latest{flag}\n\
         or make it the default with `tod-sandbox setup --image sandbox/{name}:latest`. \
         Rebake after updating tod, or the first connect updates it in place."
    );
    Ok(0)
}

fn list(ctx: &Ctx) -> Result<i32> {
    let bx = ctx.blaxel()?;
    let mut all = bx.list()?;
    all.sort_by(|a, b| a.name.cmp(&b.name));
    println!("{:<28} {:<10} {:<12} {:<14} IMAGE", "NAME", "STATE", "STATUS", "OWNER");
    for s in all {
        let owner = s.labels.iter().find(|(k, _)| k == "tod-owner").map(|(_, v)| v.as_str()).unwrap_or("");
        let mark = if ctx.config.sandbox(&s.name).is_some() { "*" } else { "" };
        let state = s.state.as_deref().unwrap_or("-");
        println!(
            "{:<28} {:<10} {:<12} {:<14} {}",
            format!("{}{mark}", s.name),
            state,
            s.status,
            owner,
            s.image
        );
    }
    println!("(* = known to this tod)");
    Ok(0)
}

/// Runs `f` against the relay, provisioning the sandbox first if it cannot be reached.
fn with_relay<T>(ctx: &mut Ctx, name: &str, f: impl Fn(&str, &str) -> Result<T>) -> Result<T> {
    let bx = ctx.blaxel()?;
    let url = ctx.url(&bx, name)?;
    let first_try = block_on(relay::connect(&relay::ws_url(&url, "/exec"), bx.token()));
    let url = match first_try {
        Ok(_) => url,
        Err(_) => ensure(ctx, &bx, name)?,
    };
    f(&url, bx.token())
}

fn exec(ctx: &mut Ctx, mut args: Args, remote: Vec<String>) -> Result<i32> {
    let name = args.positional("sandbox name")?;
    let keep_awake = args.flag("--keep-awake");
    let cwd = args.opt("--cwd");
    args.done()?;
    if remote.is_empty() {
        bail!("nothing to run: tod-sandbox exec {name} -- <command...>");
    }
    let cmd = if remote.len() == 1 { remote[0].clone() } else { remote.iter().map(|a| relay::shell_quote(a)).collect::<Vec<_>>().join(" ") };
    with_relay(ctx, &name, |url, token| {
        let req = ExecRequest { cmd: Some(cmd.clone()), cwd: cwd.clone(), keep_awake, ..ExecRequest::default() };
        block_on(relay::run_stdio(&relay::ws_url(url, "/exec"), token, &req))
    })
}

/// `--env NAME` (this process's value) or `--env NAME=VALUE`, as often as given.
fn env_flags(args: &mut Args) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    while let Some(spec) = args.opt("--env") {
        let (name, value) = match spec.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => {
                let value = std::env::var(&spec).with_context(|| format!("--env {spec}: not set here"))?;
                (spec, value)
            }
        };
        env.insert(name, value);
    }
    Ok(env)
}

/// `--cli-relay`: `tod-cli` in the sandbox goes through the relay's tunnel
/// to the tod relay this process's environment names, or `from` (a JSON
/// object of the same variables, which is deleted once read). Adds what the
/// sandbox's `tod-cli` reads to `env`; returns the local port to carry to.
fn cli_relay_env(env: &mut HashMap<String, String>, from: Option<&Path>) -> Result<u16> {
    let vars: HashMap<String, String> = match from {
        Some(file) => {
            let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
            let _ = std::fs::remove_file(file);
            serde_json::from_str(&text).with_context(|| format!("parse {}", file.display()))?
        }
        None => std::env::vars().collect(),
    };
    let missing = || anyhow!("--cli-relay needs {} and {} set (tod sets them)", cli_relay::PORT_ENV, cli_relay::TOKEN_ENV);
    let port: u16 = vars.get(cli_relay::PORT_ENV).and_then(|p| p.parse().ok()).ok_or_else(missing)?;
    let token = vars.get(cli_relay::TOKEN_ENV).cloned().ok_or_else(missing)?;
    env.insert(cli_relay::HOST_ENV.into(), "127.0.0.1".into());
    env.insert(cli_relay::PORT_ENV.into(), sandboxes::TUNNEL_PORT.to_string());
    env.insert(cli_relay::TOKEN_ENV.into(), token);
    Ok(port)
}

/// `cmd` with the sandbox's `tod-cli` first on its `PATH`.
fn with_tod_cli(cmd: &str) -> String {
    let dir = sandboxes::TOD_CLI_PATH.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("/");
    format!("PATH={dir}:\"$PATH\"; export PATH; {cmd}")
}

fn shell(ctx: &mut Ctx, mut args: Args) -> Result<i32> {
    let name = args.positional("sandbox name")?;
    let cwd = args.opt("--cwd");
    let park: u64 = args.opt("--park").map(|s| s.parse()).transpose().context("--park is in seconds")?.unwrap_or(60);
    let relay_file = args.opt("--cli-relay-file").map(PathBuf::from);
    let carry = args.flag("--cli-relay") || relay_file.is_some();
    let run = args.opt("--run");
    let mut env = env_flags(&mut args)?;
    args.done()?;
    terminal::require_terminal()?;
    let tunnel_port = carry.then(|| cli_relay_env(&mut env, relay_file.as_deref())).transpose()?;
    // Not a login shell when carrying `tod-cli`: a login profile resets
    // `PATH`, and with it `tod-cli`. The shell stays after `--run` exits.
    let shell = "shell=bash; command -v bash >/dev/null 2>&1 || shell=sh; ";
    let cmd = match (&run, carry) {
        (Some(run), true) => Some(with_tod_cli(&format!(
            "{shell}exec \"$shell\" -c {}",
            relay::shell_quote(&format!("{run}; exec \"$0\""))
        ))),
        (Some(run), false) => Some(format!("{run}; exec \"${{SHELL:-/bin/sh}}\" -l")),
        (None, true) => Some(with_tod_cli(&format!("{shell}exec \"$shell\""))),
        (None, false) => None,
    };
    with_relay(ctx, &name, |url, token| {
        let opts = TerminalOptions {
            cmd: cmd.clone(),
            cwd: cwd.clone(),
            sandbox_url: url.to_string(),
            env: env.clone(),
            tunnel_port,
            park_after: (park > 0).then(|| Duration::from_secs(park)),
            log: |_| {},
        };
        block_on(terminal::run(&relay::ws_url(url, "/exec"), token, opts))
    })
}

fn agent(ctx: &mut Ctx, mut args: Args, remote: Vec<String>) -> Result<i32> {
    let name = args.positional("sandbox name")?;
    let agent_name = args.opt("--name").ok_or_else(|| anyhow!("--name is required"))?;
    if agent_name.is_empty() || !agent_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("--name is letters, digits, dashes, and underscores: {agent_name:?}");
    }
    let cwd = args.opt("--cwd");
    let idle: u64 = args.opt("--idle").map(|s| s.parse()).transpose().context("--idle is in seconds")?.unwrap_or(30);
    let carry = args.flag("--cli-relay");
    let mut env = env_flags(&mut args)?;
    args.done()?;
    if remote.is_empty() {
        bail!("nothing to run: tod-sandbox agent {name} --name AGENT -- <command...>");
    }
    let tunnel_port = carry.then(|| cli_relay_env(&mut env, None)).transpose()?;
    let program = remote.iter().map(|a| relay::shell_quote(a)).collect::<Vec<_>>().join(" ");
    let cmd = if carry { with_tod_cli(&format!("exec {program}")) } else { format!("exec {program}") };
    let awake_file = ctx
        .root
        .join(tod_store::fleet::code_editor::zed::SHIM_DIR)
        .join("awake")
        .join(&name)
        .join(format!("{agent_name}-{}", std::process::id()));
    with_relay(ctx, &name, |url, token| {
        let opts = tod_sandbox::agent::BridgeOptions {
            name: agent_name.clone(),
            cmd: cmd.clone(),
            env: env.clone(),
            cwd: cwd.clone(),
            idle: Duration::from_secs(idle),
            tunnel_port,
            awake_file: Some(awake_file.clone()),
        };
        block_on(tod_sandbox::agent::run(url, token, opts))
    })
}

fn zed(ctx: &mut Ctx, mut args: Args) -> Result<i32> {
    let name = args.positional("sandbox name")?;
    let path = args.0.first().filter(|a| !a.starts_with("--")).cloned();
    if path.is_some() {
        args.0.remove(0);
    }
    args.done()?;
    let path = path.unwrap_or_else(|| "/root".into());
    let bx = ctx.blaxel()?;
    ensure(ctx, &bx, &name)?;
    let shim_log = ctx.root.join(tod_store::fleet::code_editor::zed::SHIM_DIR).join("shim.log");
    let log_len = std::fs::metadata(&shim_log).map(|m| m.len()).unwrap_or(0);
    let url = format!("ssh://root@{}/{}", config::host_for(&name), path.trim_start_matches('/'));
    let env = tod_store::fleet::code_editor::zed::zed_env(&ctx.root)?;
    if env.is_empty() {
        bail!("tod-zed-shim is not installed next to tod-sandbox; reinstall tod (scripts/install)");
    }
    tod_store::fleet::code_editor::zed::spawn_zed_url(&url, &ctx.root)?;
    eprintln!("opening {url} in Zed…");
    // The shim logs each connection; hearing nothing means Zed was already
    // running without it (Windows Zed is single-instance).
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if std::fs::metadata(&shim_log).map(|m| m.len()).unwrap_or(0) > log_len {
            eprintln!("connected");
            return Ok(0);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    eprintln!(
        "Zed has not connected through tod yet. If Zed was already open (started some other way), \
         quit it and run this again, so it starts with tod's ssh."
    );
    Ok(0)
}

fn doctor(ctx: &mut Ctx) -> Result<i32> {
    let mut problems = 0;
    let mut report = |ok: bool, what: &str, detail: String| {
        println!("{} {what}: {detail}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            problems += 1;
        }
    };
    report(true, "data root", ctx.root.display().to_string());
    match sandboxes::relay_path() {
        Ok(p) => report(true, "relay binary", p.display().to_string()),
        Err(e) => report(false, "relay binary", format!("{e:#}")),
    }
    let shim = sandboxes::sibling_exe("tod-zed-shim");
    report(shim.is_file(), "zed shim", shim.display().to_string());
    match tod_store::fleet::code_editor::zed::resolve_zed_bin() {
        Some(p) => report(true, "zed", p.display().to_string()),
        None => report(false, "zed", "not found (only needed for `tod-sandbox zed`)".into()),
    }
    let acct = match ctx.account() {
        Ok(a) => a.clone(),
        Err(e) => {
            report(false, "account", format!("{e:#}"));
            return Ok(1);
        }
    };
    report(true, "account", format!("workspace {} ({:?}), region {}", acct.workspace, acct.auth, acct.region));
    match ctx.blaxel().and_then(|bx| bx.list()) {
        Ok(all) => {
            report(true, "sign-in", format!("{} sandbox(es) in the workspace", all.len()));
            for s in &ctx.config.sandboxes {
                let status = all.iter().find(|i| i.name == s.name).map(|i| i.status.as_str()).unwrap_or("missing");
                report(status != "missing", &format!("sandbox {}", s.name), status.to_string());
            }
        }
        Err(e) => report(false, "sign-in", format!("{e:#}")),
    }
    Ok(if problems == 0 { 0 } else { 1 })
}
