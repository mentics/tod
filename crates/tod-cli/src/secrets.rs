//! `tod-cli secrets` — run a command with a stored secret in its environment,
//! without the agent ever seeing the value.
//!
//! Secrets live in tod's `CredentialStore` (OS keyring, else the encrypted file
//! under the data root). `list` shows only their names and whether each is
//! set; `run` reads the ones it is asked for, hands them to a child process as
//! environment variables, and masks their values in whatever the child prints.
//! The masking keeps a secret out of the agent's transcript by accident; it is
//! not a sandbox against a command written to leak it.

use crate::Invocation;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use tod_store::{CredentialKind, CredentialStore};

pub(crate) const USAGE: &str = "\
tod-cli secrets — use stored secrets without seeing them

COMMANDS:
    list
    set       <NAME> [VALUE]
    run       --env <VAR>=<SECRET> [--env <VAR>=<SECRET> ...] -- <COMMAND> [ARGS...]

`list` shows each secret's name and whether it is set, never its value.
`set` stores NAME (e.g. `github_token`), preferring the OS keyring and
falling back to an encrypted file under the data root. VALUE can be given as
an argument, but reading it from stdin (no VALUE, pipe or type it, `-` also
means stdin) keeps it out of shell history.
`run` starts COMMAND with each named secret in environment variable VAR,
prints its output with every secret value replaced by `***`, and exits with
its exit code. A secret that is not set is an error saying how the user can
add it.
";

/// What replaces a secret's value in the child's output.
const MASK: &[u8] = b"***";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest[0] == "-h" || rest[0] == "--help" {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let store = CredentialStore::from_data_root(&inv.data_root);
    match command.as_str() {
        "list" => Ok(list(&inv, &store)),
        "set" => set(&rest, &store),
        "run" => {
            let code = run_with_secrets(&store, &rest)?;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            if code != 0 {
                std::process::exit(code);
            }
            Ok(String::new())
        }
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn list(inv: &Invocation, store: &CredentialStore) -> String {
    let rows: Vec<(CredentialKind, bool)> = CredentialKind::ALL
        .into_iter()
        .map(|kind| (kind, store.get(kind).is_some()))
        .collect();
    if inv.json {
        let rows: Vec<serde_json::Value> = rows
            .iter()
            .map(|(kind, set)| {
                serde_json::json!({ "name": kind.name(), "label": kind.label(), "set": set })
            })
            .collect();
        return serde_json::to_string_pretty(&rows).unwrap_or_default();
    }
    rows.iter()
        .map(|(kind, set)| {
            let state = if *set { "set" } else { "not set" };
            format!("{} ({}): {state}", kind.name(), kind.label())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `set <NAME> [VALUE]` — VALUE from stdin (or `-`) when omitted, so it
/// never has to sit in shell history.
fn set(rest: &[String], store: &CredentialStore) -> anyhow::Result<String> {
    let name = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: secrets set <NAME> [VALUE]"))?;
    let kind = CredentialKind::from_name(name).ok_or_else(|| {
        let known: Vec<&str> = CredentialKind::ALL.iter().map(|kind| kind.name()).collect();
        anyhow::anyhow!(
            "no secret named `{name}` (tod stores: {}). Only the user can add a new kind \
             of secret",
            known.join(", ")
        )
    })?;
    let value = match rest.get(1) {
        Some(value) if value != "-" => value.clone(),
        _ => {
            // A single line, not the whole stream: reading to EOF instead
            // is at the mercy of how the terminal signals it — on Windows
            // git-bash, a literal Ctrl+D keystroke lands in the buffer as a
            // control byte rather than closing stdin.
            let mut buf = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut buf)
                .map_err(|err| anyhow::anyhow!("could not read {} from stdin: {err}", kind.name()))?;
            buf
        }
    };
    let value: String = value.chars().filter(|c| !c.is_control()).collect();
    let backend = store.set(kind, &value).map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(format!("ok {} stored ({backend:?})", kind.name()))
}

/// How the user stores `kind`, for the error an agent passes on when it is
/// missing.
fn how_to_set(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::LinearApiKey => {
            "tod asks for it the first time you create a task from a Linear ticket \
             (paste a Linear ticket URL into the task list); or set LINEAR_API_KEY \
             in the environment tod is launched from"
        }
        CredentialKind::GithubToken => {
            "set GITHUB_TOKEN in the environment tod is launched from, or store it with \
             `tod-cli secrets set github_token <token>`"
        }
        // Not in `CredentialKind::ALL`, so `from_name` never yields it here.
        CredentialKind::BlaxelApiKey => "only `tod-sandbox setup` stores it; agents cannot use it",
    }
}

/// `--env VAR=SECRET` pairs, then `--`, then the command.
struct RunArgs {
    env: Vec<(String, CredentialKind)>,
    command: Vec<String>,
}

fn parse_run(args: &[String]) -> anyhow::Result<RunArgs> {
    let mut env = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                let command = args[i + 1..].to_vec();
                anyhow::ensure!(!command.is_empty(), "nothing to run after `--`");
                anyhow::ensure!(!env.is_empty(), "name at least one --env <VAR>=<SECRET>");
                return Ok(RunArgs { env, command });
            }
            "--env" => {
                i += 1;
                let pair = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--env requires <VAR>=<SECRET>"))?;
                let (var, name) = pair
                    .split_once('=')
                    .filter(|(var, name)| !var.is_empty() && !name.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("--env takes <VAR>=<SECRET> (got `{pair}`)"))?;
                let kind = CredentialKind::from_name(name).ok_or_else(|| {
                    let known: Vec<&str> =
                        CredentialKind::ALL.iter().map(|kind| kind.name()).collect();
                    anyhow::anyhow!(
                        "no secret named `{name}` (tod stores: {}). Only the user can add \
                         a new kind of secret",
                        known.join(", ")
                    )
                })?;
                env.push((var.to_string(), kind));
            }
            other => anyhow::bail!("unexpected `{other}`: the command goes after `--`"),
        }
        i += 1;
    }
    anyhow::bail!("missing `--` before the command to run")
}

fn run_with_secrets(store: &CredentialStore, args: &[String]) -> anyhow::Result<i32> {
    let RunArgs { env, command } = parse_run(args)?;
    let mut values: Vec<(String, String)> = Vec::new();
    for (var, kind) in env {
        let value = store.get(kind).ok_or_else(|| {
            anyhow::anyhow!(
                "secret `{}` ({}) is not set. The user can add it: {}.",
                kind.name(),
                kind.label(),
                how_to_set(kind)
            )
        })?;
        values.push((var, value));
    }
    let secrets: Vec<Vec<u8>> = values
        .iter()
        .map(|(_, value)| value.as_bytes().to_vec())
        .filter(|value| !value.is_empty())
        .collect();

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .envs(values.iter().map(|(var, value)| (var, value)))
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| anyhow::anyhow!("could not start `{}`: {err}", command[0]))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_secrets = secrets.clone();
    let out = std::thread::spawn(move || {
        copy_masked(stdout, std::io::stdout(), &out_secrets);
    });
    let err = std::thread::spawn(move || {
        copy_masked(stderr, std::io::stderr(), &secrets);
    });
    let status = child.wait()?;
    let _ = out.join();
    let _ = err.join();
    // A child killed by a signal has no code; report it as a failure.
    Ok(status.code().unwrap_or(1))
}

/// Copy `from` to `to` a line at a time, masking every secret. Line by line so
/// a secret is never split across two reads; output is flushed per line so
/// progress still shows as it happens.
fn copy_masked(from: impl Read, mut to: impl Write, secrets: &[Vec<u8>]) {
    let mut reader = BufReader::new(from);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if to.write_all(&mask(&line, secrets)).is_err() {
                    break;
                }
                let _ = to.flush();
            }
        }
    }
}

/// `text` with every occurrence of each secret replaced by [`MASK`], longest
/// secret first so one that contains another is masked whole.
fn mask(text: &[u8], secrets: &[Vec<u8>]) -> Vec<u8> {
    let mut secrets: Vec<&[u8]> = secrets.iter().map(Vec::as_slice).collect();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    'scan: while i < text.len() {
        for secret in &secrets {
            if !secret.is_empty() && text[i..].starts_with(secret) {
                out.extend_from_slice(MASK);
                i += secret.len();
                continue 'scan;
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn mask_replaces_every_occurrence() {
        let secrets = vec![b"lin_abc".to_vec()];
        assert_eq!(
            mask(b"key=lin_abc and lin_abc\n", &secrets),
            b"key=*** and ***\n".to_vec()
        );
        assert_eq!(mask(b"nothing here", &secrets), b"nothing here".to_vec());
    }

    #[test]
    fn mask_prefers_the_longer_secret() {
        let secrets = vec![b"abc".to_vec(), b"abcdef".to_vec()];
        assert_eq!(mask(b"xabcdefx", &secrets), b"x***x".to_vec());
    }

    #[test]
    fn copy_masked_masks_each_line() {
        let mut out = Vec::new();
        copy_masked(
            &b"first s3cret\nsecond s3cret"[..],
            &mut out,
            &[b"s3cret".to_vec()],
        );
        assert_eq!(out, b"first ***\nsecond ***".to_vec());
    }

    #[test]
    fn run_args_need_env_and_a_command() {
        let parsed = parse_run(&strings(&[
            "--env",
            "LINEAR_API_KEY=linear_api_key",
            "--",
            "python",
            "--json",
        ]))
        .unwrap();
        assert_eq!(
            parsed.env,
            vec![("LINEAR_API_KEY".to_string(), CredentialKind::LinearApiKey)]
        );
        assert_eq!(parsed.command, strings(&["python", "--json"]));

        assert!(parse_run(&strings(&["--", "python"])).is_err());
        assert!(parse_run(&strings(&["--env", "X=linear_api_key"])).is_err());
        assert!(parse_run(&strings(&["--env", "X=linear_api_key", "--"])).is_err());
        let unknown = parse_run(&strings(&["--env", "X=bogus_credential", "--", "sh"]))
            .err()
            .unwrap()
            .to_string();
        assert!(unknown.contains("linear_api_key"), "{unknown}");
    }
}
