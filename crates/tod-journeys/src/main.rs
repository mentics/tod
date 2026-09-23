//! `tod-journeys` — the standalone receiver for sealed journey bundles sent
//! from `tod` (`doc/journeys/spec.md` §9.5). Depends only on `tod-journey`
//! and `tod-integration`: no GPUI, no database.
//!
//! Argument parsing is hand-rolled `--flag value` / positional, matching
//! `tod-cli`'s own style (`crates/tod-cli/src/args.rs`) rather than pulling
//! in a parsing crate for three small subcommands.

mod config;
mod init;
mod pull;
mod show;
mod stats;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_usage();
        std::process::exit(2);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "init" => cmd_init(rest),
        "pull" => cmd_pull(rest),
        "show" => cmd_show(rest),
        "stats" => cmd_stats(rest),
        "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command `{other}`; expected init, pull, show, or stats"),
    }
}

fn print_usage() {
    eprintln!(
        "tod-journeys - receive journeys sent from tod\n\
\n\
USAGE:\n\
    tod-journeys init [--server URL] [--home DIR] [--force]\n\
    tod-journeys pull [--once] [--home DIR]\n\
    tod-journeys show <file> [--full]\n\
    tod-journeys stats <dir>\n"
    );
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn home_dir(args: &[String]) -> Result<PathBuf> {
    match flag(args, "--home") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => config::default_home(),
    }
}

fn cmd_init(args: &[String]) -> Result<()> {
    let home = home_dir(args)?;
    let server = flag(args, "--server").unwrap_or("https://ntfy.sh");
    let force = has_flag(args, "--force");
    init::run(&home, server, force)
}

fn cmd_pull(args: &[String]) -> Result<()> {
    let home = home_dir(args)?;
    let once = has_flag(args, "--once");
    let identity_path = config::identity_path(&home);
    let identity = std::fs::read_to_string(&identity_path).with_context(|| {
        format!(
            "reading {} — run `tod-journeys init` first",
            identity_path.display()
        )
    })?;
    let mut cfg = config::Config::load(&home)?;
    pull::run_pull(&mut cfg, identity.trim(), &home, once)
}

fn cmd_show(args: &[String]) -> Result<()> {
    let full = has_flag(args, "--full");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .context("usage: tod-journeys show <file> [--full]")?;
    show::run(std::path::Path::new(path), full)
}

fn cmd_stats(args: &[String]) -> Result<()> {
    let dir = args.first().context("usage: tod-journeys stats <dir>")?;
    stats::run(std::path::Path::new(dir))
}
