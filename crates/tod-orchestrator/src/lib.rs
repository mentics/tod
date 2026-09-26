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

pub mod cli;
pub mod http;
pub mod sync_backend;
pub mod users;

use anyhow::{Context, Result};
use http::{Request, Response};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use users::Users;

pub const USER_HEADER: &str = "X-Tod-User";
pub const DEFAULT_PORT: u16 = 8080;
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
    users: Users,
}

impl Server {
    pub fn new(config: Config) -> Arc<Self> {
        let users = Users::new(config.base.clone());
        Arc::new(Self { config, users })
    }

    /// Accepts connections until the listener fails; one thread each.
    pub fn serve(self: &Arc<Self>, listener: TcpListener) -> Result<()> {
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

    pub fn handle(&self, request: &Request) -> Response {
        let segments: Vec<&str> = request.path.trim_matches('/').split('/').collect();
        let method = request.method.as_str();
        match segments.as_slice() {
            ["health"] => Response::text(200, "ok"),
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
            ["users", user, rest @ ..] => {
                let user = match self.user(request, Some(user)) {
                    Ok(u) => u,
                    Err(r) => return r,
                };
                match (method, rest) {
                    ("POST", ["seed"]) => sync_backend::seed(&self.users, &user, &request.body),
                    ("POST", ["changes"]) => match self.users.get(&user) {
                        Ok(data) => sync_backend::apply_changes(&data, &request.body),
                        Err(err) => Response::text(500, format!("{err:#}")),
                    },
                    ("GET", ["changes"]) => {
                        let after = match request.query_param("after").unwrap_or("0").parse::<i64>() {
                            Ok(n) => n,
                            Err(_) => return Response::text(400, "after must be a number"),
                        };
                        match self.users.get(&user) {
                            Ok(data) => sync_backend::export_changes(&data, after),
                            Err(err) => Response::text(500, format!("{err:#}")),
                        }
                    }
                    (_, ["seed" | "changes"]) => Response::text(405, "method not allowed"),
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
