//! `tod-supervisor wake [options]` — see the library docs.
//!
//! Configuration is the node sandbox's environment (`tod_sandbox::node::node_env`):
//! `TOD_USER`, `TOD_NODE`, and `TOD_ORCHESTRATOR_CLI_URL` (the orchestrator's
//! base URL is that without `/cli`; `TOD_ORCHESTRATOR_URL` overrides it).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
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

/// What the error arm needs to schedule a retry once `wake` has taken the config.
struct Retry {
    scheduler: Option<Arc<dyn tod_core::scheduler::Scheduler>>,
    node: uuid::Uuid,
    sandbox: String,
    state_dir: PathBuf,
}

impl Retry {
    /// Any `Err` out of `wake` would strand the node (its wake was replaced
    /// by a dead-man retry at best): schedule a backoff retry through the
    /// scheduler (the local Blaxel API). A failure to schedule is part of the
    /// error returned, never dropped.
    fn after_error(&self, err: anyhow::Error) -> anyhow::Error {
        let Some(scheduler) = &self.scheduler else {
            return err.context("no scheduler: nothing will retry this wake but a poke");
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        match tod_supervisor::waits::retry_after_error(scheduler.as_ref(), self.node, &self.sandbox, &self.state_dir, now) {
            Ok(at) => err.context(format!("the wake failed; retrying at {at}")),
            Err(sched) => err.context(format!("the wake failed and its retry could not be scheduled: {sched:#}")),
        }
    }
}

fn main() {
    // First, before anything slow: the relay signals a supervisor it has
    // started when the next poke comes, which may be right away, and
    // SIGUSR1's default action ends the process.
    tod_supervisor::signal::install();
    {
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
                ),
            )
            // WARN and ERROR also go to the node's problem log, which the
            // supervisor forwards to the app along with its next request.
            .with(tod_supervisor::problems::ProblemLayer)
            .init();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => {}
        Err(err) => {
            eprintln!("tod-supervisor: {err:#}");
            tod_supervisor::problems::report(tod_problems::Level::Error, &format!("{err:#}"));
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
    // The clone tod made, else the checkout a golden image already holds.
    let mut workspace = PathBuf::from(if Path::new("/workspace/repo/.git").exists() {
        "/workspace/repo"
    } else {
        "/workspace"
    });
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
    // Kept for the error arm: `wake` takes the config.
    let retry = Retry {
        scheduler: config.scheduler.clone(),
        node,
        sandbox: config.sandbox.clone(),
        state_dir: config.state_dir.clone(),
    };
    let woke = match wake(config) {
        Ok(woke) => woke,
        Err(err) => return Err(retry.after_error(err)),
    };
    // Another supervisor has the node: nothing here ran, so nothing here
    // ended; the schedule and the failure count are the running one's.
    if let Woke::AlreadyRunning { holder } = &woke {
        match holder {
            Some(pid) => eprintln!("tod-supervisor: another supervisor (pid {pid}) is already running this node; poked it and exiting"),
            None => eprintln!("tod-supervisor: another supervisor is already running this node; exiting"),
        }
        return Ok(());
    }
    // An orderly end: the next failure backs off from the start again.
    tod_supervisor::waits::save_count(&retry.state_dir, tod_supervisor::waits::WAKE_ERRORS_FILE, 0)?;
    match woke {
        Woke::StillWaiting(reason) => eprintln!("tod-supervisor: still waiting ({reason})"),
        Woke::LeftStopped(reason) => eprintln!("tod-supervisor: left stopped ({reason})"),
        Woke::AlreadyRunning { .. } => unreachable!("returned above"),
        Woke::Ran(outcome) => eprintln!("tod-supervisor: stopped: {}", serde_json::to_string(&outcome)?),
    }
    Ok(())
}
