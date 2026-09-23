//! `tod-cli` for agents and terminals inside a dev container.
//!
//! The real `tod-cli` is built for this machine and reads the data root on
//! it, neither of which a container can do. So the container gets a small
//! bash script named `tod-cli` ([`SHIM_SCRIPT`]) that sends its arguments,
//! its `TOD_*` environment, and its stdin over TCP to this relay; the relay
//! runs the real `tod-cli` here and sends back its exit code, stdout, and
//! stderr. Every write therefore takes the same path as an agent on this
//! machine: `tod-cli` → the mutation socket → the app's writer.
//!
//! The relay listens on loopback only; Docker Desktop forwards
//! `host.docker.internal` to it. Each request must carry the relay's token,
//! which only processes tod starts are given. The data root is the app's
//! own whatever the request says (an agent in a Linux shell may pass a
//! Windows path with its backslashes eaten).
//!
//! Wire format, request: `tod-cli-relay 1\n`, `<token>\n`, `<n>\n` then `n`
//! NUL-terminated `KEY=VALUE` entries, `<n>\n` then `n` NUL-terminated
//! arguments, `<len>\n` then `len` bytes of stdin. Reply: `<code> <stdout
//! len> <stderr len>\n`, then stdout, then stderr.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Where the shim is written in the container.
pub const SHIM_DIR: &str = "/tmp/tod-cli-relay";
pub const PORT_ENV: &str = "TOD_CLI_RELAY_PORT";
pub const TOKEN_ENV: &str = "TOD_CLI_RELAY_TOKEN";

/// The `tod-cli` a dev container runs.
pub const SHIM_SCRIPT: &str = r#"#!/usr/bin/env bash
# tod-cli, relayed: runs the real tod-cli on the machine running tod, which
# holds the data. Written into this container by tod; edits are overwritten.
host="${TOD_CLI_RELAY_HOST:-host.docker.internal}"
port="${TOD_CLI_RELAY_PORT:-}"
token="${TOD_CLI_RELAY_TOKEN:-}"
if [ -z "$port" ] || [ -z "$token" ]; then
  echo "tod-cli: this shell was not started by tod (TOD_CLI_RELAY_PORT is not set)" >&2
  exit 70
fi
if ! exec 3<>"/dev/tcp/$host/$port"; then
  echo "tod-cli: cannot reach tod at $host:$port; is the app still running?" >&2
  exit 70
fi
tmp="$(mktemp -d)" || exit 70
trap 'rm -rf "$tmp"' EXIT
: > "$tmp/in"
# stdin goes along only to a command that reads it (a `-` value, or
# `questions add`): an agent's stdin may be a pipe that never closes.
reads_stdin=; questions=; add=
for arg in "$@"; do
  case "$arg" in
    -) reads_stdin=1 ;;
    questions) questions=1 ;;
    add) add=1 ;;
  esac
done
if [ -n "$questions" ] && [ -n "$add" ]; then reads_stdin=1; fi
if [ -n "$reads_stdin" ]; then cat > "$tmp/in"; fi
names=()
for name in $(compgen -e); do
  case "$name" in
    TOD_CLI_RELAY_*) ;;
    TOD_*) names+=("$name") ;;
  esac
done
{
  printf 'tod-cli-relay 1\n%s\n%d\n' "$token" "${#names[@]}"
  for name in "${names[@]}"; do printf '%s=%s\0' "$name" "${!name}"; done
  printf '%d\n' "$#"
  for arg in "$@"; do printf '%s\0' "$arg"; done
  printf '%d\n' "$(wc -c < "$tmp/in")"
  cat "$tmp/in"
} >&3
if ! read -r code out_len _ <&3; then
  echo "tod-cli: no reply from tod" >&2
  exit 70
fi
cat <&3 > "$tmp/out"
head -c "$out_len" "$tmp/out"
tail -c +"$((out_len + 1))" "$tmp/out" >&2
exit "$code"
"#;

const MAX_ITEMS: usize = 4096;
const MAX_STDIN: usize = 16 * 1024 * 1024;

/// A running relay: what a container process needs to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub port: u16,
    pub token: String,
}

impl RelayEndpoint {
    /// The environment a container process reaches the relay with.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (PORT_ENV.to_string(), self.port.to_string()),
            (TOKEN_ENV.to_string(), self.token.clone()),
        ]
    }
}

/// The `tod-cli` installed next to this executable.
pub fn tod_cli_path() -> PathBuf {
    let name = if cfg!(windows) { "tod-cli.exe" } else { "tod-cli" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

static RELAY: OnceLock<Mutex<Option<RelayEndpoint>>> = OnceLock::new();

/// The process's relay, started on first use for `data_root` (one app, one
/// data root).
pub fn ensure_started(data_root: &Path) -> Result<RelayEndpoint> {
    let slot = RELAY.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(endpoint) = guard.as_ref() {
        return Ok(endpoint.clone());
    }
    let endpoint = start(data_root.to_path_buf(), tod_cli_path())?;
    *guard = Some(endpoint.clone());
    Ok(endpoint)
}

/// Start a relay running `cli` against `data_root`. Lives for the process.
pub fn start(data_root: PathBuf, cli: PathBuf) -> Result<RelayEndpoint> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind the tod-cli relay")?;
    let port = listener.local_addr()?.port();
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let endpoint = RelayEndpoint {
        port,
        token: token.clone(),
    };
    std::thread::Builder::new()
        .name("tod-cli-relay".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (data_root, cli, token) = (data_root.clone(), cli.clone(), token.clone());
                let _ = std::thread::Builder::new()
                    .name("tod-cli-relay-client".into())
                    .spawn(move || {
                        if let Err(err) = serve(stream, &data_root, &cli, &token) {
                            tracing::warn!("tod-cli relay: {err:#}");
                        }
                    });
            }
        })
        .context("spawn the tod-cli relay")?;
    tracing::info!(port, "tod-cli relay listening for dev containers");
    Ok(endpoint)
}

struct Request {
    env: Vec<(String, String)>,
    args: Vec<String>,
    stdin: Vec<u8>,
}

fn serve(stream: TcpStream, data_root: &Path, cli: &Path, token: &str) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = read_request(&mut reader, token)?;
    let mut command = Command::new(cli);
    command
        .args(with_data_root(request.args, data_root))
        .envs(request.env)
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
            let mut stdin = child.stdin.take().context("tod-cli stdin")?;
            let input = request.stdin;
            let writer = std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
            let out = child.wait_with_output()?;
            let _ = writer.join();
            (out.status.code().unwrap_or(1), out.stdout, out.stderr)
        }
        Err(err) => (
            70,
            Vec::new(),
            format!("tod-cli: cannot run {} on the host: {err}\n", cli.display()).into_bytes(),
        ),
    };
    let mut stream = stream;
    stream.write_all(format!("{code} {} {}\n", stdout.len(), stderr.len()).as_bytes())?;
    stream.write_all(&stdout)?;
    stream.write_all(&stderr)?;
    stream.flush()?;
    Ok(())
}

fn read_request(reader: &mut impl BufRead, token: &str) -> Result<Request> {
    if read_line(reader)? != "tod-cli-relay 1" {
        bail!("not a tod-cli relay request");
    }
    let given = read_line(reader)?;
    if !constant_time_eq(given.as_bytes(), token.as_bytes()) {
        bail!("tod-cli relay request with a wrong token");
    }
    let mut env = Vec::new();
    for entry in read_items(reader)? {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        // Only tod's own variables, and never the relay's.
        if key.starts_with("TOD_") && !key.starts_with("TOD_CLI_RELAY_") {
            env.push((key.to_string(), value.to_string()));
        }
    }
    let args = read_items(reader)?;
    let len: usize = read_line(reader)?.trim().parse().context("stdin length")?;
    if len > MAX_STDIN {
        bail!("tod-cli relay stdin too large ({len} bytes)");
    }
    let mut stdin = vec![0; len];
    reader.read_exact(&mut stdin)?;
    Ok(Request { env, args, stdin })
}

fn read_line(reader: &mut impl BufRead) -> Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// A count line, then that many NUL-terminated items.
fn read_items(reader: &mut impl BufRead) -> Result<Vec<String>> {
    let count: usize = read_line(reader)?.trim().parse().context("item count")?;
    if count > MAX_ITEMS {
        bail!("tod-cli relay request with {count} items");
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let mut item = Vec::new();
        reader.read_until(0, &mut item)?;
        if item.pop() != Some(0) {
            bail!("truncated tod-cli relay request");
        }
        items.push(String::from_utf8(item).context("tod-cli relay item is not UTF-8")?);
    }
    Ok(items)
}

/// `args` with this app's data root, replacing any the caller gave.
fn with_data_root(args: Vec<String>, data_root: &Path) -> Vec<String> {
    let root = data_root.to_string_lossy().into_owned();
    let mut out = vec!["--data-root".to_string(), root];
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
        out.push(arg);
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request(token: &str, env: &[&str], args: &[&str], stdin: &str) -> Vec<u8> {
        let mut out = format!("tod-cli-relay 1\n{token}\n{}\n", env.len()).into_bytes();
        for entry in env {
            out.extend_from_slice(entry.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(format!("{}\n", args.len()).as_bytes());
        for arg in args {
            out.extend_from_slice(arg.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(format!("{}\n", stdin.len()).as_bytes());
        out.extend_from_slice(stdin.as_bytes());
        out
    }

    #[test]
    fn requests_carry_tod_variables_arguments_and_stdin() {
        let raw = request(
            "t0k",
            &[
                "TOD_INTERVIEW_ACTOR=conversation:1",
                "TOD_CLI_RELAY_TOKEN=t0k",
                "HOME=/root",
                "TOD_NOTE=a=b\nc",
            ],
            &["node", "", "multi\nline", "-"],
            "body\n",
        );
        let parsed = read_request(&mut Cursor::new(raw), "t0k").unwrap();
        assert_eq!(
            parsed.env,
            [
                ("TOD_INTERVIEW_ACTOR".to_string(), "conversation:1".to_string()),
                ("TOD_NOTE".to_string(), "a=b\nc".to_string()),
            ]
        );
        assert_eq!(parsed.args, ["node", "", "multi\nline", "-"]);
        assert_eq!(parsed.stdin, b"body\n");
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let raw = request("nope", &[], &["node"], "");
        assert!(read_request(&mut Cursor::new(raw), "t0k").is_err());
    }

    /// Runs the shim in a real container against the real `tod-cli`. Set
    /// `TOD_TEST_DEV_CONTAINER` (a running container) and `TOD_TEST_TOD_CLI`
    /// (a built `tod-cli`); skipped otherwise.
    #[test]
    fn a_container_runs_tod_cli_through_the_relay() {
        let (Ok(container), Ok(cli)) = (
            std::env::var("TOD_TEST_DEV_CONTAINER"),
            std::env::var("TOD_TEST_TOD_CLI"),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("tod-relay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let relay = start(root.clone(), PathBuf::from(cli)).unwrap();
        let shim = format!("{SHIM_DIR}/tod-cli");
        tod_agent::devcontainer::write_file(
            &container,
            &tod_agent::devcontainer::ContainerFile {
                path: shim.clone(),
                contents: SHIM_SCRIPT.to_string(),
                executable: true,
            },
        )
        .unwrap();
        let run = |args: &[&str], stdin: &str| {
            let mut command = Command::new(tod_agent::devcontainer::docker_bin());
            command.args(["exec", "-i", "-e", PORT_ENV, "-e", TOKEN_ENV, &container, &shim]);
            command
                .args(args)
                .envs(relay.env())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };

        // The caller's data root (a Windows path, mangled by a shell) is
        // replaced by the relay's.
        let (code, _, stderr) = run(&["--data-root", r"C:\nowhere", "node", "show", "foo"], "");
        assert_eq!(code, Some(1), "{stderr}");
        assert!(stderr.contains("node `foo` not found"), "{stderr}");

        let (code, stdout, stderr) = run(&["help", "node", "create"], "");
        assert_eq!(code, Some(0), "{stderr}");
        assert!(stdout.contains("tod-cli node create --title"), "{stdout}");

        // Without the relay's variables the shim says why.
        let out = Command::new(tod_agent::devcontainer::docker_bin())
            .args(["exec", &container, &shim, "node", "show", "foo"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(70));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_data_root_is_always_the_apps() {
        let root = Path::new("/data/root");
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            with_data_root(args(&["--data-root", r"C:datagitroot", "node", "list"]), root),
            args(&["--data-root", "/data/root", "node", "list"])
        );
        assert_eq!(
            with_data_root(args(&["secrets", "run", "--", "x", "--data-root", "y"]), root),
            args(&["--data-root", "/data/root", "secrets", "run", "--", "x", "--data-root", "y"])
        );
    }
}
