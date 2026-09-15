//! How a short-lived process reaches interview data: `tod-cli`, and the mock
//! agent standing in for one.
//!
//! Writes go to a running `tod` over its mutation socket when one is live (it
//! holds the exclusive store lock); otherwise the store is opened directly.
//! Reads never need the lock and use a read-only connection.

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tod_store::fleet::{FleetPaths, FleetStore, schema};
use tod_store::interview::{ACTOR_ENV, ACTOR_USER, InterviewCommand};
use tod_store::outline::OutlineMutation;

pub struct InterviewClient {
    data_root: PathBuf,
    actor: String,
}

impl InterviewClient {
    pub fn new(data_root: impl Into<PathBuf>, actor: impl Into<String>) -> Self {
        Self {
            data_root: data_root.into(),
            actor: actor.into(),
        }
    }

    /// Acting as the interview agent session named in the environment, or the user.
    pub fn from_env(data_root: impl Into<PathBuf>) -> Self {
        Self::from_env_or(data_root, ACTOR_USER)
    }

    /// Acting as the interview agent session named in the environment, or `fallback`.
    pub fn from_env_or(data_root: impl Into<PathBuf>, fallback: &str) -> Self {
        let actor = std::env::var(ACTOR_ENV)
            .ok()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| fallback.to_string());
        Self::new(data_root, actor)
    }

    pub fn actor(&self) -> &str {
        &self.actor
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn interview(&self, command: InterviewCommand) -> Result<Value> {
        let request = json!({ "actor": self.actor, "interview": command });
        if let Some(reply) = self.forward(&request)? {
            return Ok(reply);
        }
        self.open_store()?
            .interview(&self.actor, command)
            .map_err(|err| anyhow::anyhow!("{err:#}"))
    }

    pub fn outline(&self, mutation: OutlineMutation) -> Result<()> {
        let request = json!({ "actor": self.actor, "outline": mutation });
        if self.forward(&request)?.is_some() {
            return Ok(());
        }
        let store = self.open_store()?;
        store
            .enqueue_outline_as(&self.actor, mutation)
            .map_err(|err| anyhow::anyhow!("{err:#}"))?;
        store
            .writer()
            .flush()
            .map_err(|err| anyhow::anyhow!("flush: {err}"))?;
        Ok(())
    }

    pub fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let paths = FleetPaths::new(&self.data_root)?;
        let migrated = paths.db().is_file()
            && schema::peek_user_version(paths.db())
                .is_ok_and(|v| v == schema::CURRENT_USER_VERSION);
        if migrated {
            let conn = schema::open_read_connection(paths.db())?;
            return f(&conn);
        }
        self.open_store()?.read(f)
    }

    fn open_store(&self) -> Result<FleetStore> {
        if !self.data_root.is_dir() {
            bail!("data root {} does not exist", self.data_root.display());
        }
        FleetStore::open(&self.data_root)
            .map_err(|err| anyhow::anyhow!("open store at {}: {err}", self.data_root.display()))
    }

    /// `Ok(None)` when no live instance is reachable (no port file, or a stale
    /// one left by a crash); errors only after a connection was made.
    fn forward(&self, request: &Value) -> Result<Option<Value>> {
        let Ok(paths) = FleetPaths::new(&self.data_root) else {
            return Ok(None);
        };
        let Some(port) = std::fs::read_to_string(paths.mutation_port())
            .ok()
            .and_then(|text| text.trim().parse::<u16>().ok())
        else {
            return Ok(None);
        };
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
            return Ok(None);
        };
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        writeln!(stream, "{request}").context("send to tod")?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .context("read reply from tod")?;
        let line = line.trim_end();
        if let Some(message) = line.strip_prefix("err ") {
            bail!("{message}");
        }
        let Some(rest) = line.strip_prefix("ok") else {
            bail!("unexpected reply from tod: {line}");
        };
        let rest = rest.trim();
        if rest.is_empty() {
            Ok(Some(Value::Null))
        } else {
            Ok(Some(serde_json::from_str(rest).context("parse reply from tod")?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use uuid::Uuid;

    /// A stand-in for a running `tod`: answers one request with `reply`.
    fn fake_instance(reply: &'static str) -> (PathBuf, std::thread::JoinHandle<String>) {
        let root = std::env::temp_dir().join(format!("tod-client-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            FleetPaths::new(&root).unwrap().mutation_port(),
            port.to_string(),
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            writeln!(&stream, "{reply}").unwrap();
            request
        });
        (root, server)
    }

    #[test]
    fn writes_go_to_a_running_instance_attributed_to_the_actor() {
        let (root, server) = fake_instance(r#"ok {"id":"q-7"}"#);
        let client = InterviewClient::new(&root, "session-123");
        let session_id = Uuid::new_v4();
        let value = client
            .interview(InterviewCommand::SetExhausted {
                session_id,
                reason: Some("done".into()),
            })
            .unwrap();
        assert_eq!(value["id"], "q-7");
        let request: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(request["actor"], "session-123");
        assert_eq!(request["interview"]["cmd"], "set-exhausted");
        assert_eq!(request["interview"]["session_id"], session_id.to_string());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_refusal_from_the_running_instance_is_an_error() {
        let (root, server) =
            fake_instance("err conflict: this changed after your context was built");
        let err = InterviewClient::new(&root, ACTOR_USER)
            .interview(InterviewCommand::SetExhausted {
                session_id: Uuid::new_v4(),
                reason: None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("conflict"), "{err}");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_stale_port_file_falls_back_to_the_store() {
        let root = std::env::temp_dir().join(format!("tod-client-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // Nothing listens on this port any more.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(
            FleetPaths::new(&root).unwrap().mutation_port(),
            port.to_string(),
        )
        .unwrap();
        let client = InterviewClient::new(&root, ACTOR_USER);
        let err = client
            .interview(InterviewCommand::SetExhausted {
                session_id: Uuid::new_v4(),
                reason: None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "reached the store: {err}");
        let head = client
            .read(|conn| tod_store::interview::InterviewRepo::new(conn).head_rev())
            .unwrap();
        assert_eq!(head, 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
