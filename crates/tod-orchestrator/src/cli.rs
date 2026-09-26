//! `POST /cli`: a `tod-cli` command from an agent sandbox, run against the
//! user's data root.
//!
//! The body is the `cli_relay` request frame (`tod_store::fleet::cli_relay`):
//! `tod-cli-relay 1\n`, a token line, `<n>\n` + `n` NUL-terminated
//! `KEY=VALUE`, `<n>\n` + `n` NUL-terminated arguments, `<len>\n` + stdin.
//! The reply is the relay's: `<code> <stdout len> <stderr len>\n`, stdout,
//! stderr. The token line is read and ignored: the orchestrator is reached
//! only through the sandboxes' proxies (see the design's Security section).
//!
//! The frame is parsed here rather than with `cli_relay`'s own reader, which
//! checks the app relay's per-process token.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const MAX_ITEMS: usize = 4096;

pub struct CliRequest {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
}

pub fn parse(mut body: &[u8]) -> Result<CliRequest> {
    let reader = &mut body;
    if read_line(reader)? != "tod-cli-relay 1" {
        bail!("not a tod-cli relay request");
    }
    let _token = read_line(reader)?;
    let mut env = Vec::new();
    for entry in read_items(reader)? {
        let Some((key, value)) = entry.split_once('=') else { continue };
        // Only tod's own variables; never the relay's, and never a data root.
        if key.starts_with("TOD_") && !key.starts_with("TOD_CLI_RELAY_") && key != "TOD_DATA_ROOT" {
            env.push((key.to_string(), value.to_string()));
        }
    }
    let args = read_items(reader)?;
    let len: usize = read_line(reader)?.trim().parse().context("stdin length")?;
    if len != reader.len() {
        bail!("stdin length {len} does not match the {} bytes sent", reader.len());
    }
    let mut stdin = vec![0; len];
    reader.read_exact(&mut stdin)?;
    Ok(CliRequest { env, args, stdin })
}

fn read_line(reader: &mut impl BufRead) -> Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn read_items(reader: &mut impl BufRead) -> Result<Vec<String>> {
    let count: usize = read_line(reader)?.trim().parse().context("item count")?;
    if count > MAX_ITEMS {
        bail!("request with {count} items");
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let mut item = Vec::new();
        reader.read_until(0, &mut item)?;
        if item.pop() != Some(0) {
            bail!("truncated request");
        }
        items.push(String::from_utf8(item).context("item is not UTF-8")?);
    }
    Ok(items)
}

/// `args` with `data_root`, replacing any the caller gave.
pub fn with_data_root(args: Vec<String>, data_root: &Path) -> Vec<String> {
    let mut out = vec!["--data-root".to_string(), data_root.to_string_lossy().into_owned()];
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            out.push(arg);
            out.extend(args.by_ref());
            break;
        }
        if arg == "--data-root" {
            args.next();
            continue;
        }
        if arg.starts_with("--data-root=") {
            continue;
        }
        out.push(arg);
    }
    out
}

/// Runs `cli` (with `prefix` arguments first) and returns the reply frame.
pub fn run(cli: &Path, prefix: &[String], data_root: &Path, request: CliRequest) -> Vec<u8> {
    let mut command = Command::new(cli);
    command
        .args(prefix)
        .args(with_data_root(request.args, data_root))
        .envs(request.env)
        .env("TOD_DATA_ROOT", data_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let (code, stdout, stderr) = match command.spawn() {
        Ok(mut child) => {
            let mut stdin = child.stdin.take().expect("piped stdin");
            let input = request.stdin;
            let writer = std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
            match child.wait_with_output() {
                Ok(out) => {
                    let _ = writer.join();
                    (out.status.code().unwrap_or(1), out.stdout, out.stderr)
                }
                Err(err) => (70, Vec::new(), format!("tod-cli: {err}\n").into_bytes()),
            }
        }
        Err(err) => (
            70,
            Vec::new(),
            format!("tod-cli: cannot run {} on the orchestrator: {err}\n", cli.display()).into_bytes(),
        ),
    };
    let mut reply = format!("{code} {} {}\n", stdout.len(), stderr.len()).into_bytes();
    reply.extend_from_slice(&stdout);
    reply.extend_from_slice(&stderr);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_root_is_always_the_users() {
        let args = vec!["--data-root".into(), "/elsewhere".into(), "node".into(), "--data-root=/x".into(), "list".into()];
        assert_eq!(with_data_root(args, Path::new("/data/users/a")), ["--data-root", "/data/users/a", "node", "list"]);
    }

    #[test]
    fn parses_a_relay_frame_and_drops_foreign_variables() {
        let body = b"tod-cli-relay 1\nx\n3\nTOD_A=1\0PATH=/bin\0TOD_DATA_ROOT=/y\0\x32\nnode\0list\0\x33\nabc";
        let r = parse(body).unwrap();
        assert_eq!(r.env, [("TOD_A".to_string(), "1".to_string())]);
        assert_eq!(r.args, ["node", "list"]);
        assert_eq!(r.stdin, b"abc");
    }
}
