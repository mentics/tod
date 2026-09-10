//! `tod-cli` — the interface agents use to read and change tod's data.
//!
//! Every mutation goes through `tod_store`'s `OutlineMutation` queue, the same
//! path the GUI uses, so invariants cannot be bypassed. Raw SQL is deliberately
//! not exposed: an agent gets a small verified vocabulary rather than the schema.

mod obligations;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
tod-cli — read and modify tod data

USAGE:
    tod-cli --data-root <PATH> <NOUN> <COMMAND> [OPTIONS]

GLOBAL OPTIONS:
    --data-root <PATH>   Directory holding the tod database (required)
    --json               Emit JSON instead of text
    -h, --help           Show this help

NOUNS:
    obligations          Requirements and constraints attached to a node

Run `tod-cli <NOUN> --help` for that noun's commands.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("tod-cli: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Parsed global options plus the remaining noun/command arguments.
pub struct Invocation {
    pub data_root: PathBuf,
    pub json: bool,
    pub rest: Vec<String>,
}

fn run(args: &[String]) -> anyhow::Result<String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") && args.len() == 1 {
        return Ok(USAGE.trim_end().to_string());
    }

    let mut data_root: Option<PathBuf> = None;
    let mut json = false;
    let mut rest: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-root" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--data-root requires a path"))?;
                data_root = Some(PathBuf::from(value));
            }
            "--json" => json = true,
            other => rest.push(other.to_string()),
        }
        i += 1;
    }

    if rest.is_empty() {
        return Ok(USAGE.trim_end().to_string());
    }

    let data_root = data_root.ok_or_else(|| {
        anyhow::anyhow!(
            "--data-root <PATH> is required (the agent context message supplies the value)"
        )
    })?;
    if !data_root.is_dir() {
        anyhow::bail!("data root {} does not exist", data_root.display());
    }

    let noun = rest.remove(0);
    let invocation = Invocation {
        data_root,
        json,
        rest,
    };

    match noun.as_str() {
        "obligations" => obligations::run(invocation),
        other => anyhow::bail!("unknown noun `{other}` (expected: obligations)"),
    }
}
