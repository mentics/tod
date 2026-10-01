//! A client of the daemon as a store's writer: the app and `tod-cli` open
//! their `FleetStore` through [`open_store`], and every write it makes is a
//! request to the daemon that owns the database (`doc/agentd.md`, "One
//! writer"). Reads stay on the process's own read-only connection.

use crate::client::{self, ConnectionLost, Connection};
use crate::{Command, Event, Paths};
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_store::fleet::maintenance::Maintenance;
use tod_store::fleet::store::FleetStore;
use tod_store::fleet::writer::{FleetMutation, RemoteWriter};
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

/// The daemon as a [`RemoteWriter`]. One request at a time; a connection
/// that fails is replaced (starting a daemon if none runs) and the request
/// sent again under its id, which the daemon answers rather than re-applies.
pub struct DaemonWriter {
    data_root: PathBuf,
    /// The `tod-agentd` to start when none is running; `None` never starts one
    /// (a container or sandbox, whose daemon is the host's).
    executable: Option<PathBuf>,
    connection: Mutex<Option<Connection>>,
}

impl DaemonWriter {
    pub fn new(data_root: &Path, executable: Option<PathBuf>) -> Self {
        Self { data_root: data_root.to_path_buf(), executable, connection: Mutex::new(None) }
    }

    fn reconnect(&self) -> Result<Connection> {
        let paths = Paths::new(&self.data_root);
        if let Some(connection) = client::connect(&paths) {
            return Ok(connection);
        }
        let executable = self
            .executable
            .as_deref()
            .ok_or_else(|| anyhow!("tod-agentd is not running for {}", self.data_root.display()))?;
        match client::ensure_running(&self.data_root, executable) {
            Ok(_) => {}
            // A newer daemon is still a daemon.
            Err(err) if err.downcast_ref::<client::DaemonNewer>().is_some() => {}
            Err(err) => return Err(err),
        }
        client::connect(&paths).ok_or_else(|| anyhow!("tod-agentd did not answer"))
    }

    fn call(&self, command: Command) -> Result<crate::Response> {
        let id = Uuid::new_v4();
        let mut slot = self.connection.lock().expect("daemon connection mutex");
        let mut last = None;
        for _ in 0..3 {
            if slot.is_none() {
                match self.reconnect() {
                    Ok(connection) => *slot = Some(connection),
                    Err(err) => {
                        last = Some(err);
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                }
            }
            let connection = slot.as_mut().expect("just connected");
            match connection.request_with_id(Some(id), command.clone()) {
                Ok(response) => return Ok(response),
                Err(err) if err.downcast_ref::<ConnectionLost>().is_some() => {
                    // Whether the daemon got it is unknown; the id makes
                    // sending it again safe.
                    *slot = None;
                    last = Some(err);
                }
                Err(err) => return Err(err),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("tod-agentd did not answer")))
    }
}

impl RemoteWriter for DaemonWriter {
    fn enqueue(&self, actor: &str, mutation: FleetMutation) -> Result<()> {
        self.call(Command::Enqueue { actor: actor.to_string(), mutation })?;
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.call(Command::Flush)?;
        Ok(())
    }

    fn interview(&self, actor: &str, command: InterviewCommand) -> Result<serde_json::Value> {
        let response = self.call(Command::Interview { actor: actor.to_string(), command })?;
        Ok(response.value.unwrap_or(serde_json::Value::Null))
    }

    fn undo_last(&self) -> Result<Option<String>> {
        let response = self.call(Command::UndoLast)?;
        Ok(response.value.and_then(|v| v.as_str().map(str::to_string)))
    }

    fn undo_through(&self, entry_id: Uuid) -> Result<Vec<String>> {
        let response = self.call(Command::UndoThrough { entry_id })?;
        Ok(serde_json::from_value(response.value.unwrap_or_default()).unwrap_or_default())
    }

    fn switch_database(&self, path: &Path) -> Result<()> {
        self.call(Command::SwitchDatabase { path: path.to_path_buf() })?;
        Ok(())
    }

    fn maintenance(&self, op: Maintenance) -> Result<serde_json::Value> {
        let response = self.call(Command::Maintenance { op })?;
        Ok(response.value.unwrap_or(serde_json::Value::Null))
    }
}

/// Call `notify` each time the daemon says the store changed, and once after
/// every reconnect (a change may have been missed while it was down). Ends
/// when `notify` is dropped. It never starts a daemon: only a write does.
pub fn follow_changes(data_root: &Path, notify: &Arc<tokio::sync::Notify>) -> Result<()> {
    let writer = DaemonWriter::new(data_root, None);
    let notify = Arc::downgrade(notify);
    std::thread::Builder::new()
        .name("tod-agentd-follow".into())
        .spawn(move || {
            loop {
                if let Ok(mut connection) = writer.reconnect() {
                    if connection.request(Command::Subscribe).is_ok() {
                        while let Some(Event::Changed { .. }) = connection.next_event() {
                            let Some(notify) = notify.upgrade() else { return };
                            notify.notify_waiters();
                        }
                    }
                }
                // Gone: look again shortly, and tell the store to re-read.
                std::thread::sleep(Duration::from_millis(500));
                let Some(notify) = notify.upgrade() else { return };
                notify.notify_waiters();
            }
        })
        .context("start the change feed")?;
    Ok(())
}

/// Open the store as a client of the daemon for `data_root`, starting the
/// daemon (or replacing an older one) when `executable` is given.
pub fn open_store(data_root: &Path, executable: Option<&Path>) -> Result<FleetStore> {
    if let Some(executable) = executable {
        match client::ensure_running(data_root, executable) {
            Ok(_) => {}
            Err(err) if err.downcast_ref::<client::DaemonNewer>().is_some() => {}
            Err(err) => return Err(err),
        }
    }
    let notify = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(DaemonWriter::new(data_root, executable.map(Path::to_path_buf)));
    follow_changes(data_root, &notify)?;
    FleetStore::open_client(data_root, writer, notify).map_err(|err| anyhow!("{err}"))
}
