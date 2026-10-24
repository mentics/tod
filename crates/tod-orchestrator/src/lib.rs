//! The orchestrator: one server for the whole team, holding a copy of each
//! user's tod database and serving `tod-cli` to the agent sandboxes.
//! Design: `doc/cloud-sandboxes/autonomous-nodes.md`; running it:
//! `doc/cloud-sandboxes/orchestrator.md`.
//!
//! Routes (every one but `/health` names a user, by `X-Tod-User` or in the
//! path; both, when given, must agree):
//! - `GET /health`
//! - `POST /cli` — a `tod-cli` command (see [`cli`]).
//! - `POST /users/<u>/seed` — a snapshot of the user's database.
//! - `POST /users/<u>/changes` — the app's changes.
//! - `GET /users/<u>/changes?after=<n>` — the changes after number `n`.
//!   Clients name themselves with `X-Tod-Client` (`app-<id>`,
//!   `supervisor-<node>`): what one sends reaches every other client's pulls
//!   and never comes back to it (see [`sync_backend`]).
//! - `GET /users/<u>/notify` — the ntfy server and topics the app subscribes
//!   to (see [`notify`]).
//! - `GET /users/<u>/snapshot` — the whole database, for a supervisor's copy.
//! - `GET /users/<u>/nodes/<n>/transcripts` — the node's mirrored agent
//!   transcripts; `GET`/`POST .../transcripts/<name>` reads or appends one
//!   (see [`transcripts`]).
//! - `POST /users/<u>/nodes/<n>/flags` — the watchdog flags a node (see [`flags`]).
//! - `POST /users/<u>/nodes/<n>/problems`, `POST /users/<u>/problems/pull` —
//!   failures of the cloud's executables on their way to the app's log panel
//!   (see [`problems`]).
//! - `POST /users/<u>/broker/request`, `GET .../broker/next`,
//!   `POST .../broker/reply/<id>` — a sandbox asking the user's own machine
//!   for something only it can do (see [`broker`]).
//! - `POST /webhooks/github`, `POST /webhooks/linear` — no user: signed, and
//!   routed to nodes by branch or open waits (see [`webhooks`]).

pub mod answers;
pub mod broker;
pub mod cli;
pub mod flags;
pub mod github_gate;
pub mod http;
pub mod impact_handler;
pub mod notify;
pub mod problems;
pub mod sync_backend;
pub mod transcripts;
pub mod users;
pub mod wait_changes;
pub mod wakes;
pub mod webhooks;

use anyhow::{Context, Result};
use http::{Request, Response};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use users::Users;

pub const USER_HEADER: &str = tod_store::fleet::cli_relay::USER_HEADER;

/// The sync client a request is from (`X-Tod-Client`, else `?client=`).
fn client(request: &Request) -> Option<&str> {
    request
        .header(tod_store::sync::CLIENT_HEADER)
        .or_else(|| request.query_param("client"))
        .filter(|c| !c.is_empty())
}
/// Not 8080: every Blaxel sandbox's own API listens there.
pub const DEFAULT_PORT: u16 = 8090;
pub const DEFAULT_BASE: &str = "/data";

pub struct Config {
    /// Users' data roots are `<base>/users/<user>/`.
    pub base: PathBuf,
    /// The real `tod-cli`.
    pub tod_cli: PathBuf,
    /// Arguments put before the command's own (tests use this).
    pub tod_cli_prefix: Vec<String>,
}

pub struct Server {
    config: Config,
    users: Arc<Users>,
    wakes: Arc<wakes::Wakes>,
    notifier: Arc<notify::Notifier>,
    webhook_secrets: webhooks::Secrets,
    broker: broker::Broker,
}

impl Server {
    /// Loads the pending wakes from `<base>/wakes.json`; pokes go through
    /// [`wakes::RelayPoker`].
    pub fn new(config: Config) -> Result<Arc<Self>> {
        Self::with_poker(config, Box::new(wakes::RelayPoker::from_env()))
    }

    pub fn with_poker(config: Config, poker: Box<dyn wakes::Poker>) -> Result<Arc<Self>> {
        Self::with_sink(config, poker, notify::Notifier::sink_from_env())
    }

    /// [`Self::with_poker`], publishing change notices to `sink` (see [`notify`]).
    pub fn with_sink(config: Config, poker: Box<dyn wakes::Poker>, sink: Box<dyn notify::Sink>) -> Result<Arc<Self>> {
        let users = Arc::new(Users::new(config.base.clone()));
        let wakes = wakes::Wakes::load(&config.base, poker)?;
        wakes.set_lost_handler(impact_handler::lost_handler(users.clone()));
        wakes.set_gate(Box::new(github_gate::GithubGate::new(users.clone())));
        let notifier = notify::Notifier::start(Box::new(notify::StoreProbe(users.clone())), sink);
        let webhook_secrets = webhooks::Secrets::load(&config.base)?;
        Ok(Arc::new(Self { config, users, wakes, notifier, webhook_secrets, broker: broker::Broker::default() }))
    }

    pub fn wakes(&self) -> &Arc<wakes::Wakes> {
        &self.wakes
    }

    /// Starts the wake timer, then accepts connections until the listener
    /// fails; one thread each.
    pub fn serve(self: &Arc<Self>, listener: TcpListener) -> Result<()> {
        self.wakes.spawn_timer(Box::new(wakes::LocalRelayAwake::default()))?;
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(err) => {
                    eprintln!("tod-orchestrator: accept: {err}");
                    continue;
                }
            };
            let server = self.clone();
            std::thread::Builder::new()
                .name("tod-orchestrator-conn".into())
                .spawn(move || server.connection(stream))
                .context("spawn a connection thread")?;
        }
        Ok(())
    }

    fn connection(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
        let response = match http::read_request(&stream) {
            Ok(request) => self.handle(&request),
            Err(err) => Response::text(400, format!("{err:#}")),
        };
        if let Err(err) = http::write_response(&stream, &response) {
            eprintln!("tod-orchestrator: reply: {err:#}");
        }
    }

    /// [`Self::route`], then a change notice for the user after any POST that succeeded
    /// (the notifier skips it when nothing was committed).
    pub fn handle(&self, request: &Request) -> Response {
        let response = self.route(request);
        // This server's own failure goes where the app will find it.
        if response.status >= 500 {
            let user = request.path.trim_matches('/').strip_prefix("users/").and_then(|r| r.split('/').next());
            if let Some(root) = user.or_else(|| request.header(USER_HEADER)).and_then(|u| self.users.root_of(u).ok()) {
                problems::report_own(
                    &root,
                    &format!("{} {} failed: {}", request.method, request.path, String::from_utf8_lossy(&response.body).trim()),
                );
                if let Some(u) = user.or_else(|| request.header(USER_HEADER)) {
                    self.notifier.poke(u);
                }
            }
        }
        if request.method == "POST" && response.status == 200 {
            let path_user = request.path.trim_matches('/').strip_prefix("users/").and_then(|r| r.split('/').next());
            if let Some(user) = path_user.or_else(|| request.header(USER_HEADER)) {
                self.notifier.poke(user);
            }
        }
        response
    }

    fn route(&self, request: &Request) -> Response {
        let segments: Vec<&str> = request.path.trim_matches('/').split('/').collect();
        let method = request.method.as_str();
        match segments.as_slice() {
            ["health"] => Response::text(200, "ok"),
            ["webhooks", source] => {
                let (response, touched) =
                    webhooks::handle(&self.users, &self.wakes, &self.webhook_secrets, source, request);
                for user in touched {
                    self.notifier.poke(&user);
                }
                response
            }
            ["cli"] => {
                if method != "POST" {
                    return Response::text(405, "POST /cli");
                }
                let user = match self.user(request, None) {
                    Ok(u) => u,
                    Err(r) => return r,
                };
                self.cli(&user, &request.body)
            }
            ["wakes", rest @ ..] if rest.len() <= 1 => {
                let user = match self.user(request, None) {
                    Ok(u) => u,
                    Err(r) => return r,
                };
                self.wakes.handle(request, &user, rest.first().copied())
            }
            ["users", user, rest @ ..] => {
                let user = match self.user(request, Some(user)) {
                    Ok(u) => u,
                    Err(r) => return r,
                };
                match (method, rest) {
                    ("POST", ["seed"]) => sync_backend::seed(&self.users, &user, &request.body),
                    ("POST", ["changes"]) => match self.users.get(&user) {
                        Ok(data) => sync_backend::apply_changes(
                            &data,
                            &request.body,
                            client(request).unwrap_or("unknown"),
                            Some((&user, &self.wakes)),
                        ),
                        Err(err) => Response::text(500, format!("{err:#}")),
                    },
                    ("GET", ["changes"]) => {
                        let after = match request.query_param("after").unwrap_or("0").parse::<i64>() {
                            Ok(n) => n,
                            Err(_) => return Response::text(400, "after must be a number"),
                        };
                        match self.users.get(&user) {
                            Ok(data) => sync_backend::export_changes(&data, after, client(request)),
                            Err(err) => Response::text(500, format!("{err:#}")),
                        }
                    }
                    ("POST", ["broker", "request"]) => self.broker.request(&user, request),
                    ("GET", ["broker", "next"]) => self.broker.next(&user, request),
                    ("POST", ["broker", "reply", id]) => self.broker.reply(&user, id, &request.body),
                    ("GET", ["notify"]) => self.notify_topics(&user),
                    ("GET" | "PUT", ["github-routes"]) => self.github_routes(&user, request),
                    ("GET", ["snapshot"]) => match self.users.get(&user) {
                        Ok(data) => sync_backend::snapshot(&data),
                        Err(err) => Response::text(500, format!("{err:#}")),
                    },
                    ("POST", ["nodes", node, "flags"]) => match self.users.get(&user) {
                        Ok(data) => flags::handle(
                            &self.config.tod_cli,
                            &self.config.tod_cli_prefix,
                            &data.root,
                            node,
                            &request.body,
                        ),
                        Err(err) => Response::text(500, format!("{err:#}")),
                    },
                    ("POST", ["nodes", node, "problems"]) => match self.users.root_of(&user) {
                        Ok(root) => match users::validate(node) {
                            Ok(()) => problems::push(&root, node, &request.body),
                            Err(err) => Response::text(400, format!("node: {err:#}")),
                        },
                        Err(err) => Response::text(400, format!("{err:#}")),
                    },
                    ("POST", ["problems", "pull"]) => match self.users.root_of(&user) {
                        Ok(root) => problems::pull(&root, &request.body),
                        Err(err) => Response::text(400, format!("{err:#}")),
                    },
                    (_, ["nodes", node, "transcripts", rest @ ..]) => match self.users.root_of(&user) {
                        Ok(root) => transcripts::handle(&root, node, method, rest, request),
                        Err(err) => Response::text(400, format!("{err:#}")),
                    },
                    (_, ["seed" | "changes" | "snapshot"]) => Response::text(405, "method not allowed"),
                    _ => Response::text(404, "not found"),
                }
            }
            _ => Response::text(404, "not found"),
        }
    }

    /// The request's user: the path's, else the header's; both must agree.
    fn user(&self, request: &Request, in_path: Option<&str>) -> std::result::Result<String, Response> {
        let header = request.header(USER_HEADER);
        let user = match (in_path, header) {
            (Some(p), Some(h)) if p != h => {
                return Err(Response::text(400, format!("{USER_HEADER} {h:?} does not match the path's user {p:?}")));
            }
            (Some(p), _) => p,
            (None, Some(h)) => h,
            (None, None) => return Err(Response::text(400, format!("missing {USER_HEADER}"))),
        };
        users::validate(user).map_err(|e| Response::text(400, format!("{e:#}")))?;
        Ok(user.to_string())
    }

    /// `GET`/`PUT /users/<u>/github-routes`: which of the orchestrator's
    /// GitHub tokens reads which repository (`tod_store::github_routes`).
    /// It names proxy secrets and holds no token.
    fn github_routes(&self, user: &str, request: &Request) -> Response {
        use tod_store::github_routes::GithubRoutes;
        let root = match self.users.root_of(user) {
            Ok(r) => r,
            Err(err) => return Response::text(400, format!("{err:#}")),
        };
        if request.method == "GET" {
            return match GithubRoutes::load(&root) {
                Ok(routes) => Response {
                    status: 200,
                    content_type: "application/json",
                    body: serde_json::to_vec(&routes).unwrap_or_default(),
                },
                Err(err) => Response::text(500, format!("{err:#}")),
            };
        }
        let routes: GithubRoutes = match serde_json::from_slice(&request.body) {
            Ok(r) => r,
            Err(err) => return Response::text(400, format!("bad routes: {err}")),
        };
        for route in &routes.routes {
            let secret_ok = !route.secret.is_empty() && route.secret.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if route.pattern.trim().is_empty() || !secret_ok {
                return Response::text(400, format!("bad route {:?}: a pattern, and a secret of letters, digits and _", route.pattern));
            }
        }
        match routes.save(&root) {
            Ok(()) => Response::text(200, "ok"),
            Err(err) => Response::text(500, format!("{err:#}")),
        }
    }

    /// `GET /users/<u>/notify`: the ntfy server and the user's topics.
    fn notify_topics(&self, user: &str) -> Response {
        let Some(server) = notify::server_from_env() else {
            return Response::text(404, "notifications are off on this orchestrator");
        };
        let topic = match self.users.root_of(user).and_then(|root| notify::topic(&root)) {
            Ok(t) => t,
            Err(err) => return Response::text(500, format!("{err:#}")),
        };
        let topics = notify::Topics { server, alerts_topic: notify::alerts_topic(&topic), topic };
        Response {
            status: 200,
            content_type: "application/json",
            body: serde_json::to_vec(&topics).unwrap_or_default(),
        }
    }

    fn cli(&self, user: &str, body: &[u8]) -> Response {
        let request = match cli::parse(body) {
            Ok(r) => r,
            Err(err) => return Response::text(400, format!("{err:#}")),
        };
        let data = match self.users.get(user) {
            Ok(d) => d,
            Err(err) => return Response::text(500, format!("{err:#}")),
        };
        Response::bytes(cli::run(&self.config.tod_cli, &self.config.tod_cli_prefix, &data.root, request))
    }
}
