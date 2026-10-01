//! `tod-supervisor wake [options]` — see the library docs.
//!
//! Configuration is the node sandbox's environment (`tod_sandbox::node::node_env`):
//! `TOD_USER`, `TOD_NODE`, and `TOD_ORCHESTRATOR_CLI_URL` (the orchestrator's
//! base URL is that without `/cli`; `TOD_ORCHESTRATOR_URL` overrides it).

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tod_core::autopilot::Budget;
use tod_supervisor::agent::AgentKind;
use tod_supervisor::hold::RelayHolder;
use tod_supervisor::orchestrator::{Orchestrator, base_from_cli_url};
use tod_supervisor::transcripts::{DirStore, TranscriptStore, default_projects_dir, drive_dir};
use tod_supervisor::{Config, Woke, wake};

const USAGE: &str = "usage: tod-supervisor wake [--workspace DIR] [--agent claude|mock] [--state-dir DIR]
                          [--relay URL] [--media-root DIR] [--no-push] [--no-transcripts]
environment: TOD_USER, TOD_NODE, TOD_SANDBOX, TOD_ORCHESTRATOR_CLI_URL (or TOD_ORCHESTRATOR_URL),
             TOD_SUPERVISOR_AGENT (the default for --agent; claude when unset),
             TOD_SCHEDULER (orchestrator | blaxel; with blaxel, TOD_BLAXEL_WORKSPACE)";

fn env(name: &str) -> Result<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty()).with_context(|| format!("{name} is not set"))
}

fn main() {
    // First, before anything slow: the relay signals a supervisor it has
    // started when the next poke comes, which may be right away, and
    // SIGUSR1's default action ends the process.
    tod_supervisor::signal::install();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => {}
        Err(err) => {
            eprintln!("tod-supervisor: {err:#}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("--version") => {
            println!("tod-supervisor {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("wake") => {}
        _ => bail!("{USAGE}"),
    }
    let mut workspace = PathBuf::from("/workspace/repo");
    // The sandbox's environment picks the agent (`tod_sandbox::node::node_env`),
    // since the relay starts `wake` with no options.
    let mut agent = match env("TOD_SUPERVISOR_AGENT") {
        Ok(raw) => AgentKind::parse(&raw)?,
        Err(_) => AgentKind::Claude,
    };
    let mut state_dir: Option<PathBuf> = None;
    let mut relay = "http://127.0.0.1:2222".to_string();
    let mut media_root: Option<PathBuf> = None;
    let mut push_branch = true;
    let mut transcripts = true;
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--workspace" => workspace = value()?.into(),
            "--agent" => agent = AgentKind::parse(&value()?)?,
            "--state-dir" => state_dir = Some(value()?.into()),
            "--relay" => relay = value()?,
            "--media-root" => media_root = Some(value()?.into()),
            "--no-push" => push_branch = false,
            "--no-transcripts" => transcripts = false,
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }

    let user = env("TOD_USER")?;
    let node_raw = env("TOD_NODE")?;
    let node = uuid::Uuid::parse_str(&node_raw).with_context(|| format!("TOD_NODE {node_raw:?} is not a node's UUID"))?;
    let base = match env("TOD_ORCHESTRATOR_URL") {
        Ok(url) => url,
        Err(_) => base_from_cli_url(&env("TOD_ORCHESTRATOR_CLI_URL")?),
    };
    let orchestrator = Orchestrator::new(base, user, node.to_string());
    let media = match media_root.or_else(|| std::env::var_os("TOD_MEDIA_ROOT").map(PathBuf::from)) {
        Some(root) => tod_core::media::MediaPaths::from_media_root(root),
        None => tod_core::media::MediaPaths::discover(),
    }
    .map_err(|e| anyhow::anyhow!("media bundle: {e}"))?;
    let transcripts = transcripts.then(default_projects_dir).flatten().map(|projects| {
        // Agent Drive when the sandbox has it mounted, else the orchestrator.
        let sink: Arc<dyn TranscriptStore> = match drive_dir() {
            Some(dir) => {
                tracing::info!("mirroring transcripts to Agent Drive ({})", dir.display());
                Arc::new(DirStore::new(dir))
            }
            None => Arc::new(orchestrator.clone()),
        };
        (projects, sink)
    });
    let config = Config {
        orchestrator,
        node,
        workspace,
        state_dir: state_dir.unwrap_or_else(|| {
            PathBuf::from(tod_store::fleet::cli_relay::SUPERVISOR_STATE_ROOT).join(node.to_string())
        }),
        agent,
        holder: Arc::new(RelayHolder::new(relay)),
        transcripts,
        media,
        budget: Budget::default(),
        guards: tod_supervisor::guard::Guards {
            hang_after: env("TOD_HANG_MINUTES")
                .ok()
                .and_then(|m| m.parse::<u64>().ok())
                .map_or(tod_supervisor::guard::Guards::default().hang_after, |m| Duration::from_secs(m * 60)),
            ..Default::default()
        },
        poll: Duration::from_millis(500),
        push_branch,
        // `TOD_SCHEDULER`: a Blaxel schedule on this sandbox (its proxy adds
        // the token) or the orchestrator's timer.
        scheduler: match tod_core::scheduler::from_env() {
            Ok(s) => Some(Arc::from(s)),
            Err(err) => {
                tracing::warn!("no wake scheduler: {err:#}");
                None
            }
        },
        sandbox: env("TOD_SANDBOX").unwrap_or_else(|_| format!("node-{node}")),
    };
    match wake(config)? {
        Woke::StillWaiting(reason) => eprintln!("tod-supervisor: still waiting ({reason})"),
        Woke::LeftStopped(reason) => eprintln!("tod-supervisor: left stopped ({reason})"),
        Woke::Ran(outcome) => eprintln!("tod-supervisor: stopped: {}", serde_json::to_string(&outcome)?),
    }
    Ok(())
}
