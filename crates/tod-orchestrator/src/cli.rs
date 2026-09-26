//! `POST /cli`: a `tod-cli` command from an agent sandbox, run against the
//! user's data root.
//!
//! The body is the `cli_relay` request frame and the reply its reply frame
//! (`tod_store::fleet::cli_relay::{decode_request, encode_reply}`). The token
//! line is not checked: the orchestrator is reached only through the
//! sandboxes' proxies (see the design's Security section). A command that
//! exits nonzero is still a 200; its code is in the frame.

use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use tod_store::fleet::cli_relay::{self, RelayReply, RelayRequest};

pub fn parse(mut body: &[u8]) -> Result<RelayRequest> {
    let mut request = cli_relay::decode_request(&mut body, None)?;
    // The data root is the user's, whatever the sandbox says.
    request.env.retain(|(k, _)| k != "TOD_DATA_ROOT");
    Ok(request)
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
pub fn run(cli: &Path, prefix: &[String], data_root: &Path, request: RelayRequest) -> Vec<u8> {
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
    let failed = |stderr: String| RelayReply { code: 70, stdout: Vec::new(), stderr: stderr.into_bytes() };
    let reply = match command.spawn() {
        Ok(mut child) => {
            let mut stdin = child.stdin.take().expect("piped stdin");
            let input = request.stdin;
            let writer = std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
            match child.wait_with_output() {
                Ok(out) => {
                    let _ = writer.join();
                    RelayReply { code: out.status.code().unwrap_or(1), stdout: out.stdout, stderr: out.stderr }
                }
                Err(err) => failed(format!("tod-cli: {err}\n")),
            }
        }
        Err(err) => failed(format!("tod-cli: cannot run {} on the orchestrator: {err}\n", cli.display())),
    };
    cli_relay::encode_reply(&reply)
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
        let body = b"tod-cli-relay 1\n\n3\nTOD_A=1\0PATH=/bin\0TOD_DATA_ROOT=/y\0\x32\nnode\0list\0\x33\nabc";
        let r = parse(body).unwrap();
        assert_eq!(r.env, [("TOD_A".to_string(), "1".to_string())]);
        assert_eq!(r.args, ["node", "list"]);
        assert_eq!(r.stdin, b"abc");
    }
}
