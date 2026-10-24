//! Answers a cloud node's requests for a database login, on this machine
//! (`doc/cloud-sandboxes/database-access.md`). Run by the resident daemon.
//!
//! A node's agent runs `tod-cli db run prod -- psql`. In its sandbox that
//! asks the orchestrator's broker, which this holds a long poll on. For each
//! request this:
//!
//! 1. finds the node's sandbox and the `[[database]]` it asked for (it
//!    answers only what the user declared, and only for a node it knows runs
//!    in a sandbox of its own),
//! 2. opens the port forward on that sandbox, to the database, through the
//!    relay's tunnel (held while the node keeps asking, closed after
//!    [`IDLE`], so an idle sandbox can sleep),
//! 3. signs a 15-minute login token with the user's AWS profile,
//! 4. replies. The sandbox never holds an AWS credential.
//!
//! A failure is the reply: the agent sees why (the profile has expired, the
//! machine cannot reach the database, the node is unknown), never a hang.

use crate::cloud_sync::{self, HttpOrchestrator};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tod_sandbox::config::{self, Config, Forward};
use tod_sandbox::database::Database;
use tod_sandbox::hold::{Held, hold};
use tod_store::fleet::FleetStore;

/// How long a sandbox's forwards stay open after the node last asked.
pub const IDLE: Duration = Duration::from_secs(10 * 60);
const POLL_SECS: u64 = 25;

/// What the broker needs from its surroundings; tests and the live check
/// replace the parts that would otherwise need a running app.
pub struct Service {
    /// The data root (the sandbox account and its credentials).
    pub root: PathBuf,
    /// The declared databases.
    pub databases: Box<dyn Fn() -> Result<Vec<Database>> + Send + Sync>,
    /// The sandbox a node runs in, if it is a cloud node this machine knows.
    pub sandbox_of: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// The orchestrator to answer for, and the user's name there.
    pub orchestrator: Box<dyn Fn() -> Result<(HttpOrchestrator, String)> + Send + Sync>,
}

impl Service {
    /// The app's: databases from `sandboxes.toml`, nodes from the store, the
    /// orchestrator the data root syncs with.
    pub fn for_store(fleet: Arc<FleetStore>) -> Self {
        let root = fleet.paths().root().to_path_buf();
        let (databases_root, orchestrator_root) = (root.clone(), root.clone());
        Self {
            root,
            databases: Box::new(move || databases(&databases_root)),
            sandbox_of: Box::new(move |node| {
                cloud_sync::cloud_node(&fleet, node).filter(|c| c.lost_at.is_none()).map(|c| c.sandbox)
            }),
            orchestrator: Box::new(move || cloud_sync::resolve(&orchestrator_root)),
        }
    }
}

/// Starts answering on a thread of its own. It does nothing until
/// `sandboxes.toml` declares a database, and says (once) when it cannot.
pub fn start(fleet: Arc<FleetStore>) {
    spawn(Service::for_store(fleet));
}

/// [`start`] for a given [`Service`].
pub fn spawn(service: Service) {
    let spawned = std::thread::Builder::new().name("db-broker".into()).spawn(move || run(Arc::new(service)));
    if let Err(err) = spawned {
        tracing::warn!("the database broker did not start: {err}");
    }
}

/// Logs each distinct message once.
#[derive(Default)]
struct Quiet(HashSet<String>);

impl Quiet {
    fn warn(&mut self, what: String) {
        if self.0.insert(what.clone()) {
            tracing::warn!("database broker: {what}");
        }
    }
}

fn databases(root: &Path) -> Result<Vec<Database>> {
    Config::load(&root.join(config::FILE_NAME))?.databases()
}

fn run(service: Arc<Service>) {
    let tunnels = Arc::new(Tunnels::default());
    tunnels.clone().spawn_reaper();
    let mut quiet = Quiet::default();
    let mut orchestrator: Option<(Arc<HttpOrchestrator>, String)> = None;
    loop {
        match (service.databases)() {
            Ok(dbs) if dbs.is_empty() => {
                std::thread::sleep(Duration::from_secs(30));
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                quiet.warn(format!("{err:#}"));
                std::thread::sleep(Duration::from_secs(60));
                continue;
            }
        }
        let (orch, user) = match &orchestrator {
            Some((o, u)) => (o.clone(), u.clone()),
            None => match (service.orchestrator)() {
                Ok((o, u)) => {
                    let o = Arc::new(o);
                    orchestrator = Some((o.clone(), u.clone()));
                    (o, u)
                }
                Err(err) => {
                    quiet.warn(format!("no orchestrator to answer for: {err:#}"));
                    std::thread::sleep(Duration::from_secs(30));
                    continue;
                }
            },
        };
        match orch.broker_next(&user, POLL_SECS) {
            Ok(None) => {}
            Ok(Some((id, node, request))) => {
                let (service, tunnels, orch, user) = (service.clone(), tunnels.clone(), orch.clone(), user.clone());
                let spawned = std::thread::Builder::new().name("db-broker-request".into()).spawn(move || {
                    let reply = match answer(&service, &tunnels, &node, &request) {
                        Ok(v) => v,
                        Err(err) => json!({"ok": false, "error": format!("{err:#}")}),
                    };
                    if let Err(err) = orch.broker_reply(&user, id, &reply) {
                        tracing::warn!("database broker: could not send the reply for request {id}: {err:#}");
                    }
                });
                if let Err(err) = spawned {
                    tracing::warn!("database broker: could not start a handler: {err}");
                }
            }
            Err(err) => {
                // E.g. an orchestrator that predates the broker, or no network.
                quiet.warn(format!("{err:#}"));
                orchestrator = None;
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

/// One request's reply. The request is `{"op": "list" | "token" | "touch", "name": …}`.
fn answer(service: &Service, tunnels: &Tunnels, node: &str, request: &Value) -> Result<Value> {
    let dbs = (service.databases)()?;
    let op = request["op"].as_str().unwrap_or_default();
    if op == "list" {
        let list: Vec<Value> = dbs.iter().map(|d| json!({"name": d.name, "dbname": d.dbname, "user": d.user})).collect();
        return Ok(json!({"ok": true, "databases": list}));
    }
    let name = request["name"].as_str().context("the request names no database")?;
    let db = dbs.iter().find(|d| d.name == name).ok_or_else(|| {
        let known: Vec<&str> = dbs.iter().map(|d| d.name.as_str()).collect();
        anyhow!("no database named {name:?} is set up on the user's machine (it has: {})", known.join(", "))
    })?;
    // Only a node this machine knows to run in a sandbox of its own.
    let sandbox = (service.sandbox_of)(node).ok_or_else(|| anyhow!("node {node} is not a cloud node this machine knows"))?;
    tunnels.ensure(&service.root, &sandbox, db.forward())?;
    match op {
        "touch" => Ok(json!({"ok": true})),
        "token" => {
            let login = db.login()?;
            Ok(json!({"ok": true, "login": login}))
        }
        other => Err(anyhow!("unknown request {other:?}")),
    }
}

struct Entry {
    held: Held,
    last_used: Instant,
}

/// Forwards held open, one tunnel per sandbox.
#[derive(Default)]
struct Tunnels {
    map: Mutex<HashMap<String, Entry>>,
}

impl Tunnels {
    /// `forward` is open on `sandbox` when this returns. Opening one may
    /// wake the sandbox, so it can take a few seconds.
    fn ensure(&self, root: &Path, sandbox: &str, forward: Forward) -> Result<()> {
        let mut map = self.map.lock().unwrap();
        if let Some(entry) = map.get_mut(sandbox) {
            if entry.held.forwards.contains(&forward) {
                entry.last_used = Instant::now();
                return Ok(());
            }
        }
        // A new database for a sandbox that has others: reopen with them all.
        let mut forwards = map.remove(sandbox).map(|e| e.held.forwards.clone()).unwrap_or_default();
        forwards.retain(|f| f.port != forward.port);
        forwards.push(forward);
        let (url, token) = endpoint(root, sandbox)?;
        let held = hold(&url, &token, forwards).with_context(|| format!("open the port forward on sandbox {sandbox}"))?;
        map.insert(sandbox.to_string(), Entry { held, last_used: Instant::now() });
        Ok(())
    }

    fn spawn_reaper(self: Arc<Self>) {
        let spawned = std::thread::Builder::new().name("db-broker-reaper".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            self.map.lock().unwrap().retain(|_, e| e.last_used.elapsed() < IDLE);
        });
        if let Err(err) = spawned {
            tracing::warn!("database broker: the idle check did not start: {err}");
        }
    }
}

/// The sandbox's relay URL and the workspace token that reaches it.
fn endpoint(root: &Path, sandbox: &str) -> Result<(String, String)> {
    let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    let url = sandboxes.url(&bx, sandbox)?;
    Ok((url, bx.token().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(nodes: &'static [(&'static str, &'static str)]) -> Service {
        let db = Database {
            name: "prod".into(),
            host: "h.example".into(),
            port: 5432,
            user: "u".into(),
            dbname: "d".into(),
            region: "us-east-1".into(),
            profile: None,
            sandbox_port: 15432,
        };
        Service {
            root: std::env::temp_dir(),
            databases: Box::new(move || Ok(vec![db.clone()])),
            sandbox_of: Box::new(move |node| nodes.iter().find(|(n, _)| *n == node).map(|(_, s)| s.to_string())),
            orchestrator: Box::new(|| Err(anyhow!("not used"))),
        }
    }

    #[test]
    fn list_names_the_databases_and_nothing_secret() {
        let reply = answer(&service(&[]), &Tunnels::default(), "n", &json!({"op": "list"})).unwrap();
        assert_eq!(reply["databases"][0]["name"], "prod");
        assert!(!reply.to_string().contains("h.example"), "{reply}");
    }

    #[test]
    fn only_a_declared_database_and_a_known_cloud_node_get_a_login() {
        let svc = service(&[("known", "sbx")]);
        let tunnels = Tunnels::default();
        let unknown_db = answer(&svc, &tunnels, "known", &json!({"op": "token", "name": "other"})).unwrap_err();
        assert!(format!("{unknown_db:#}").contains("no database named"), "{unknown_db:#}");
        let unknown_node = answer(&svc, &tunnels, "stranger", &json!({"op": "token", "name": "prod"})).unwrap_err();
        assert!(format!("{unknown_node:#}").contains("not a cloud node"), "{unknown_node:#}");
        let no_name = answer(&svc, &tunnels, "known", &json!({"op": "token"})).unwrap_err();
        assert!(format!("{no_name:#}").contains("names no database"));
    }
}
