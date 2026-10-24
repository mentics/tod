//! `tod-cli db` — log in to a database through the user's own machine, in a
//! cloud node's sandbox, without the agent ever holding the login.
//!
//! The databases are the ones the user declared on their machine
//! (`[[database]]` in `sandboxes.toml`). `run` asks the orchestrator's broker
//! for a login; the user's machine signs a 15-minute token with its own AWS
//! session, opens a port forward to the database on this sandbox (the
//! database only lets that machine in), and replies. `run` then maps the
//! database's name to loopback in `/etc/hosts`, so the token and the TLS
//! certificate both match the real name, puts the connection in `PG*`
//! variables for the command it starts, and masks the token in the command's
//! output. The agent never sees the token; it only runs the command.
//!
//! Design: `doc/cloud-sandboxes/database-access.md`.

use crate::Invocation;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tod_sandbox::database::Login;
use tod_store::fleet::cli_relay::{NODE_HEADER, NODE_ENV, ORCHESTRATOR_CLI_URL_ENV, USER_ENV, USER_HEADER};

pub(crate) const USAGE: &str = "\
tod-cli db — log in to the user's databases without holding the login

COMMANDS:
    list
    run       <NAME> -- <COMMAND> [ARGS...]
    psql      <NAME> [-- <PSQL ARGS...>]

The user declares which databases a node may use, on their own machine; `list`
shows their names. `run` starts COMMAND with the connection in the standard
PG* environment variables (PGHOST, PGPORT, PGUSER, PGDATABASE, PGPASSWORD,
PGSSLMODE, PGSSLROOTCERT), so psql and any libpq-based client connect
unchanged. The login is signed on the user's machine and lasts 15 minutes;
its output has it masked. `psql NAME` is `run NAME -- psql`. It works only
for a node running in the cloud, and only while the user's machine is on and
running tod; otherwise it says so.
";

/// How often a running command tells the user's machine it still needs the
/// connection open.
const KEEP_OPEN_EVERY: Duration = Duration::from_secs(60);

pub fn run(inv: Invocation) -> Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest[0] == "-h" || rest[0] == "--help" {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    match command.as_str() {
        "list" => list(&inv),
        "run" => {
            let (name, argv) = parse_run(&rest)?;
            finish(run_command(&name, &argv)?)
        }
        "psql" => {
            let name = rest.first().cloned().ok_or_else(|| anyhow!("usage: db psql <NAME> [-- <PSQL ARGS...>]"))?;
            let mut argv = vec!["psql".to_string()];
            match rest.get(1).map(String::as_str) {
                None => {}
                Some("--") => argv.extend(rest[2..].iter().cloned()),
                Some(other) => bail!("unexpected `{other}`: psql's own arguments go after `--`"),
            }
            finish(run_command(&name, &argv)?)
        }
        other => bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn finish(code: i32) -> Result<String> {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    if code != 0 {
        std::process::exit(code);
    }
    Ok(String::new())
}

fn parse_run(args: &[String]) -> Result<(String, Vec<String>)> {
    let name = args.first().filter(|a| !a.starts_with('-')).ok_or_else(|| anyhow!("usage: db run <NAME> -- <COMMAND> [ARGS...]"))?;
    match args.get(1).map(String::as_str) {
        Some("--") if args.len() > 2 => Ok((name.clone(), args[2..].to_vec())),
        Some("--") => bail!("nothing to run after `--`"),
        _ => bail!("missing `--` before the command to run"),
    }
}

fn list(inv: &Invocation) -> Result<String> {
    let reply = Broker::from_env()?.ask(&serde_json::json!({"op": "list"}))?;
    let dbs = reply["databases"].as_array().cloned().unwrap_or_default();
    if inv.json {
        return Ok(serde_json::to_string_pretty(&dbs)?);
    }
    if dbs.is_empty() {
        return Ok("the user has set up no databases for you".to_string());
    }
    Ok(dbs
        .iter()
        .map(|d| format!("{} (database {}, user {})", d["name"].as_str().unwrap_or("?"), d["dbname"].as_str().unwrap_or("?"), d["user"].as_str().unwrap_or("?")))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The orchestrator's broker, from the node's environment.
struct Broker {
    url: String,
    user: String,
    node: String,
    agent: ureq::Agent,
}

impl Broker {
    fn from_env() -> Result<Self> {
        let var = |name: &str| {
            std::env::var(name).map_err(|_| {
                anyhow!("{name} is not set: `db` works only in the sandbox of a node running in the cloud (the user's machine does the login)")
            })
        };
        let cli = var(ORCHESTRATOR_CLI_URL_ENV)?;
        let base = cli.trim_end_matches('/').trim_end_matches("/cli").to_string();
        let user = var(USER_ENV)?;
        Ok(Self {
            url: format!("{base}/users/{user}/broker/request"),
            user,
            node: var(NODE_ENV)?,
            // A login can wait for the user's machine to open a tunnel.
            agent: tod_core::scheduler::sandbox_agent(Duration::from_secs(120)),
        })
    }

    /// The reply to `request`; an error carries the user's machine's reason.
    fn ask(&self, request: &serde_json::Value) -> Result<serde_json::Value> {
        let mut last = None;
        for attempt in 0..4u64 {
            let sent = self
                .agent
                .post(&self.url)
                .header(USER_HEADER, &self.user)
                .header(NODE_HEADER, &self.node)
                .send_json(request);
            match sent {
                // The sandbox's proxy answers 407 for a moment after it starts.
                Ok(resp) if resp.status().as_u16() == 407 => last = Some(anyhow!("the sandbox's proxy answered 407")),
                Ok(mut resp) => {
                    let status = resp.status();
                    let body = resp.body_mut().read_to_string().unwrap_or_default();
                    let value: serde_json::Value = serde_json::from_str(&body)
                        .map_err(|_| anyhow!("the orchestrator answered {status}: {}", body.trim().chars().take(300).collect::<String>()))?;
                    if value["ok"].as_bool() == Some(true) {
                        return Ok(value);
                    }
                    match value["error"].as_str() {
                        Some(why) => bail!("{why}"),
                        None => bail!("the orchestrator answered {status}: {}", body.trim().chars().take(300).collect::<String>()),
                    }
                }
                Err(err) => last = Some(anyhow!(err)),
            }
            std::thread::sleep(Duration::from_millis(500 * (attempt + 1)));
        }
        Err(last.unwrap_or_else(|| anyhow!("no answer"))).context("could not reach the orchestrator's broker")
    }
}

fn run_command(name: &str, argv: &[String]) -> Result<i32> {
    let broker = Broker::from_env()?;
    let reply = broker.ask(&serde_json::json!({"op": "token", "name": name}))?;
    let login: Login = serde_json::from_value(reply["login"].clone()).context("the login reply")?;
    login.validate()?;
    map_to_loopback(&login)?;
    let ca = ca_bundle(&login)?;

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env("PGHOST", &login.host)
        .env("PGPORT", login.port.to_string())
        .env("PGUSER", &login.user)
        .env("PGDATABASE", &login.dbname)
        .env("PGPASSWORD", &login.token)
        .env("PGSSLMODE", "verify-full")
        .env("PGSSLROOTCERT", &ca)
        .env("PGCONNECT_TIMEOUT", "20")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| anyhow!("could not start `{}`: {err}", argv[0]))?;
    let secrets = vec![login.token.as_bytes().to_vec()];
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let out_secrets = secrets.clone();
    let out = std::thread::spawn(move || crate::secrets::copy_masked(stdout, std::io::stdout(), &out_secrets));
    let err = std::thread::spawn(move || crate::secrets::copy_masked(stderr, std::io::stderr(), &secrets));

    // While the command runs, keep the user's machine's connection to this
    // sandbox open (it closes it after a while without a request).
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let name = name.to_string();
    let keeper = std::thread::spawn(move || {
        while done_rx.recv_timeout(KEEP_OPEN_EVERY).is_err() {
            if let Err(e) = broker.ask(&serde_json::json!({"op": "touch", "name": name})) {
                eprintln!("tod-cli db: could not keep the connection open: {e:#}");
            }
        }
    });
    let status = child.wait()?;
    let _ = done_tx.send(());
    let _ = keeper.join();
    let _ = out.join();
    let _ = err.join();
    Ok(status.code().unwrap_or(1))
}

const HOSTS: &str = "/etc/hosts";

/// Makes the database's real name resolve to loopback in this sandbox, where
/// the port forward is, so the token (signed for that name) and the server's
/// certificate (made for it) both match what the client connects to.
fn map_to_loopback(login: &Login) -> Result<()> {
    map_in(std::path::Path::new(HOSTS), login)
}

fn map_in(hosts: &std::path::Path, login: &Login) -> Result<()> {
    let marker = format!("# tod db {}", login.name);
    let text = std::fs::read_to_string(hosts).with_context(|| format!("read {}", hosts.display()))?;
    let line = format!("127.0.0.1 {} {marker}", login.host);
    if text.lines().any(|l| l == line) {
        return Ok(());
    }
    // Replace this database's own earlier line; leave every other line alone.
    let mut out: String = text.lines().filter(|l| !l.ends_with(&marker)).map(|l| format!("{l}\n")).collect();
    out.push_str(&line);
    out.push('\n');
    std::fs::write(hosts, out).with_context(|| {
        format!("map {} to 127.0.0.1 in {} (this must run as root in the sandbox)", login.host, hosts.display())
    })
}

/// The RDS CA bundle, kept under `~/.tod/db`, fetched through the sandbox's
/// proxy the first time.
fn ca_bundle(login: &Login) -> Result<PathBuf> {
    let file = login.ca_url.rsplit('/').next().unwrap_or("rds-bundle.pem");
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/root"));
    let dir = home.join(".tod").join("db");
    let path = dir.join(file);
    if std::fs::read_to_string(&path).is_ok_and(|t| t.contains("BEGIN CERTIFICATE")) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let agent = tod_core::scheduler::sandbox_agent(Duration::from_secs(60));
    let mut resp = agent.get(&login.ca_url).call().with_context(|| format!("fetch {}", login.ca_url))?;
    let body = resp.body_mut().read_to_string().with_context(|| format!("read {}", login.ca_url))?;
    if !resp.status().is_success() || !body.contains("BEGIN CERTIFICATE") {
        bail!("{} did not return a certificate bundle ({})", login.ca_url, resp.status());
    }
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(host: &str) -> Login {
        Login {
            name: "prod".into(),
            host: host.into(),
            port: 15432,
            user: "u".into(),
            dbname: "d".into(),
            ca_url: "https://truststore.pki.rds.amazonaws.com/us-east-1/us-east-1-bundle.pem".into(),
            token: "t".into(),
        }
    }

    #[test]
    fn the_hosts_file_gets_one_line_per_database_and_nothing_else_changes() {
        let dir = std::env::temp_dir().join(format!("tod-db-hosts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hosts = dir.join("hosts");
        std::fs::write(&hosts, "127.0.0.1 localhost\n10.0.0.5 other\n").unwrap();
        map_in(&hosts, &login("a.rds.example")).unwrap();
        map_in(&hosts, &login("a.rds.example")).unwrap(); // again: no duplicate
        let text = std::fs::read_to_string(&hosts).unwrap();
        assert_eq!(text.matches("a.rds.example").count(), 1, "{text}");
        assert!(text.starts_with("127.0.0.1 localhost\n10.0.0.5 other\n"));
        // The database moved: its own line is replaced, not kept.
        map_in(&hosts, &login("b.rds.example")).unwrap();
        let text = std::fs::read_to_string(&hosts).unwrap();
        assert!(!text.contains("a.rds.example") && text.contains("127.0.0.1 b.rds.example # tod db prod"), "{text}");
    }

    #[test]
    fn run_needs_a_command_after_the_double_dash() {
        assert!(parse_run(&["prod".into()]).is_err());
        assert!(parse_run(&["prod".into(), "--".into()]).is_err());
        assert!(parse_run(&["--".into(), "psql".into()]).is_err());
        assert_eq!(parse_run(&["prod".into(), "--".into(), "psql".into(), "-c".into()]).unwrap(), ("prod".into(), vec!["psql".into(), "-c".into()]));
    }
}
