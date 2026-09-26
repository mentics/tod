//! `tod-orchestrator [--port N] [--base DIR] [--tod-cli PATH]`
//!
//! `--base` (or `TOD_ORCHESTRATOR_BASE`) defaults to `/data`; `--tod-cli`
//! (or `TOD_ORCHESTRATOR_TOD_CLI`) to the `tod-cli` beside this executable.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use tod_orchestrator::{Config, DEFAULT_BASE, DEFAULT_PORT, Server};

const USAGE: &str = "usage: tod-orchestrator [--port N] [--bind ADDR] [--base DIR] [--tod-cli PATH]";

fn main() {
    if let Err(err) = run() {
        eprintln!("tod-orchestrator: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut port = DEFAULT_PORT;
    let mut bind = "0.0.0.0".to_string();
    let mut base = std::env::var_os("TOD_ORCHESTRATOR_BASE").map(PathBuf::from).unwrap_or(DEFAULT_BASE.into());
    let mut tod_cli = std::env::var_os("TOD_ORCHESTRATOR_TOD_CLI").map(PathBuf::from);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--port" => port = value()?.parse().context("--port")?,
            "--bind" => bind = value()?,
            "--base" => base = value()?.into(),
            "--tod-cli" => tod_cli = Some(value()?.into()),
            // A stand-in tod-cli for the integration tests: echoes what it got.
            "--test-echo-cli" => return echo(args.collect()),
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let tod_cli = tod_cli.unwrap_or_else(|| {
        let name = if cfg!(windows) { "tod-cli.exe" } else { "tod-cli" };
        std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name))).unwrap_or(name.into())
    });
    std::fs::create_dir_all(&base).with_context(|| format!("create {}", base.display()))?;
    let listener = TcpListener::bind((bind.as_str(), port)).with_context(|| format!("bind {bind}:{port}"))?;
    eprintln!("tod-orchestrator: listening on {bind}:{port}, data in {}, tod-cli {}", base.display(), tod_cli.display());
    Server::new(Config { base, tod_cli, tod_cli_prefix: Vec::new() }).serve(listener)
}

fn echo(args: Vec<String>) -> Result<()> {
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;
    let mut out = std::io::stdout();
    writeln!(out, "args={}", args.join(" "))?;
    writeln!(out, "TOD_DATA_ROOT={}", std::env::var("TOD_DATA_ROOT").unwrap_or_default())?;
    writeln!(out, "TOD_ACTOR={}", std::env::var("TOD_ACTOR").unwrap_or_default())?;
    writeln!(out, "stdin={stdin}")?;
    eprint!("err");
    std::process::exit(3);
}
