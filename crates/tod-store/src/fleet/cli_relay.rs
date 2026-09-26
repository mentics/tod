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
//! len> <stderr len>\n`, then stdout, then stderr. [`encode_request`],
//! [`decode_request`], [`encode_reply`], and [`decode_reply`] are that
//! format; the orchestrator's `POST /cli` uses it too.
//!
//! An autonomous node's sandbox runs [`HTTP_SHIM_SCRIPT`] instead: the same
//! request body `curl`ed to the orchestrator through the sandbox's proxy
//! (which adds the Blaxel token), with the user and node in `X-Tod-User` /
//! `X-Tod-Node`. Design: `doc/cloud-sandboxes/autonomous-nodes.md`.

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
/// Where the shim finds the relay (`host.docker.internal` when unset).
pub const HOST_ENV: &str = "TOD_CLI_RELAY_HOST";

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

/// The orchestrator's `/cli` URL, for [`HTTP_SHIM_SCRIPT`] (e.g.
/// `https://<orchestrator>.bl.run/port/8080/cli`).
pub const ORCHESTRATOR_CLI_URL_ENV: &str = "TOD_ORCHESTRATOR_CLI_URL";
/// The user whose database the orchestrator uses (`X-Tod-User`).
pub const USER_ENV: &str = "TOD_USER";
/// The node the sandbox works on (`X-Tod-Node`).
pub const NODE_ENV: &str = "TOD_NODE";
pub const USER_HEADER: &str = "X-Tod-User";
pub const NODE_HEADER: &str = "X-Tod-Node";

/// The `tod-cli` an autonomous node's sandbox runs: [`SHIM_SCRIPT`]'s request
/// body (with an empty token) sent with `curl` to the orchestrator, retried
/// with backoff on connection errors and 5xx/407/429 answers (the proxy
/// answers 407 for a moment after the sandbox is created).
pub const HTTP_SHIM_SCRIPT: &str = r#"#!/usr/bin/env bash
# tod-cli, relayed over HTTP to the tod orchestrator, which runs the real
# tod-cli against this user's database. Written by tod; edits are overwritten.
url="${TOD_ORCHESTRATOR_CLI_URL:-}"
user="${TOD_USER:-}"
node="${TOD_NODE:-}"
if [ -z "$url" ] || [ -z "$user" ] || [ -z "$node" ]; then
  echo "tod-cli: TOD_ORCHESTRATOR_CLI_URL, TOD_USER, and TOD_NODE must be set" >&2
  exit 70
fi
tmp="$(mktemp -d)" || exit 70
trap 'rm -rf "$tmp"' EXIT
: > "$tmp/in"
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
    TOD_CLI_RELAY_*|TOD_ORCHESTRATOR_*) ;;
    TOD_*) names+=("$name") ;;
  esac
done
{
  printf 'tod-cli-relay 1\n\n%d\n' "${#names[@]}"
  for name in "${names[@]}"; do printf '%s=%s\0' "$name" "${!name}"; done
  printf '%d\n' "$#"
  for arg in "$@"; do printf '%s\0' "$arg"; done
  printf '%d\n' "$(wc -c < "$tmp/in")"
  cat "$tmp/in"
} > "$tmp/req"
status=000
delay=1
for attempt in 1 2 3 4 5 6; do
  status="$(curl -sS -o "$tmp/reply" -w '%{http_code}' --max-time 300 \
    -X POST -H 'Content-Type: application/octet-stream' \
    -H "X-Tod-User: $user" -H "X-Tod-Node: $node" \
    --data-binary @"$tmp/req" "$url" 2>"$tmp/curl-err")" || status=000
  case "$status" in
    000|407|429|5??) [ "$attempt" -lt 6 ] && sleep "$delay"; delay=$((delay * 2)) ;;
    *) break ;;
  esac
done
if [ "$status" != 200 ]; then
  echo "tod-cli: the orchestrator did not answer (HTTP $status): $(cat "$tmp/curl-err" 2>/dev/null; head -c 400 "$tmp/reply" 2>/dev/null)" >&2
  exit 70
fi
if ! read -r code out_len _ < "$tmp/reply"; then
  echo "tod-cli: empty reply from the orchestrator" >&2
  exit 70
fi
header_len=$(head -n 1 "$tmp/reply" | wc -c)
tail -c +"$((header_len + 1))" "$tmp/reply" > "$tmp/body"
head -c "$out_len" "$tmp/body"
tail -c +"$((out_len + 1))" "$tmp/body" >&2
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

/// One `tod-cli` invocation as carried by the relay's wire format (see the
/// module docs). The orchestrator's `POST /cli` takes the same body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelayRequest {
    /// `TOD_*` variables only (never `TOD_CLI_RELAY_*`) after decoding.
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
}

/// What `tod-cli` did: the reply half of the wire format.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelayReply {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Encodes a request the way the shims send it. `token` is the relay's
/// token; the HTTP shim sends an empty one (the orchestrator is reached only
/// through the workspace's proxy, and names user and node in headers).
pub fn encode_request(token: &str, request: &RelayRequest) -> Vec<u8> {
    let mut out = format!("tod-cli-relay 1\n{token}\n{}\n", request.env.len()).into_bytes();
    for (key, value) in &request.env {
        out.extend_from_slice(format!("{key}={value}").as_bytes());
        out.push(0);
    }
    out.extend_from_slice(format!("{}\n", request.args.len()).as_bytes());
    for arg in &request.args {
        out.extend_from_slice(arg.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(format!("{}\n", request.stdin.len()).as_bytes());
    out.extend_from_slice(&request.stdin);
    out
}

/// Decodes a request. With `expected_token`, a request carrying any other
/// token is refused (compared in constant time); `None` skips the check (the
/// orchestrator, which has no token). Non-`TOD_*` and `TOD_CLI_RELAY_*`
/// variables are dropped.
pub fn decode_request(reader: &mut impl BufRead, expected_token: Option<&str>) -> Result<RelayRequest> {
    if read_line(reader)? != "tod-cli-relay 1" {
        bail!("not a tod-cli relay request");
    }
    let given = read_line(reader)?;
    if let Some(token) = expected_token
        && !constant_time_eq(given.as_bytes(), token.as_bytes())
    {
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
    Ok(RelayRequest { env, args, stdin })
}

/// Encodes a reply: `<code> <stdout len> <stderr len>\n`, stdout, stderr.
pub fn encode_reply(reply: &RelayReply) -> Vec<u8> {
    let mut out = format!("{} {} {}\n", reply.code, reply.stdout.len(), reply.stderr.len()).into_bytes();
    out.extend_from_slice(&reply.stdout);
    out.extend_from_slice(&reply.stderr);
    out
}

/// Decodes a reply written by [`encode_reply`].
pub fn decode_reply(bytes: &[u8]) -> Result<RelayReply> {
    let newline = bytes.iter().position(|&b| b == b'\n').context("tod-cli reply has no header")?;
    let header = std::str::from_utf8(&bytes[..newline]).context("tod-cli reply header")?;
    let mut parts = header.split_whitespace();
    let mut next = |what: &str| -> Result<&str> { parts.next().with_context(|| format!("tod-cli reply: no {what}")) };
    let code: i32 = next("exit code")?.parse().context("tod-cli reply exit code")?;
    let out_len: usize = next("stdout length")?.parse().context("tod-cli reply stdout length")?;
    let err_len: usize = next("stderr length")?.parse().context("tod-cli reply stderr length")?;
    let body = &bytes[newline + 1..];
    if body.len() != out_len + err_len {
        bail!("tod-cli reply body is {} bytes, header says {}", body.len(), out_len + err_len);
    }
    Ok(RelayReply { code, stdout: body[..out_len].to_vec(), stderr: body[out_len..].to_vec() })
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

fn read_request(reader: &mut impl BufRead, token: &str) -> Result<RelayRequest> {
    decode_request(reader, Some(token))
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
    fn frames_round_trip() {
        let request = RelayRequest {
            env: vec![("TOD_NODE".into(), "n1".into()), ("TOD_X".into(), "a=b\nc".into())],
            args: vec!["plan".into(), "".into(), "multi\nline".into()],
            stdin: b"body\0bytes".to_vec(),
        };
        let raw = encode_request("", &request);
        assert_eq!(decode_request(&mut Cursor::new(&raw), None).unwrap(), request);
        assert_eq!(decode_request(&mut Cursor::new(&raw), Some("")).unwrap(), request);
        assert!(decode_request(&mut Cursor::new(&raw), Some("t")).is_err());

        let reply = RelayReply { code: 3, stdout: b"out\n2".to_vec(), stderr: b"err".to_vec() };
        assert_eq!(decode_reply(&encode_reply(&reply)).unwrap(), reply);
        assert!(decode_reply(b"0 5 0\nabc").is_err());
        assert!(decode_reply(b"").is_err());
    }

    #[test]
    fn the_http_shim_posts_the_same_body_with_user_and_node() {
        let s = HTTP_SHIM_SCRIPT;
        assert!(s.contains(r"printf 'tod-cli-relay 1\n\n%d\n'"));
        assert!(s.contains(&format!("{USER_HEADER}: $user")));
        assert!(s.contains(&format!("{NODE_HEADER}: $node")));
        for var in [ORCHESTRATOR_CLI_URL_ENV, USER_ENV, NODE_ENV] {
            assert!(s.contains(&format!("${{{var}:-}}")), "{var}");
        }
        assert!(s.contains("--data-binary"));
        assert!(s.contains("delay=$((delay * 2))"));
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
