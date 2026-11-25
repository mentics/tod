//! Agent Client Protocol transport for the ZGX chat slot. Claude and Codex
//! adapters run as child processes, with JSON-RPC over stdio.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command as Process,
    sync::{mpsc, oneshot},
};

/// What the interface hears of the agent.
#[derive(Debug, Clone)]
pub enum Event {
    /// The session is open.
    Ready(Config),
    /// The session's choices changed (a model switch changes the efforts).
    Config(Config),
    /// A piece of the reply (or of its thinking).
    Chunk { thought: bool, text: String },
    /// A tool call began or changed (ACP's `tool_call` and `tool_call_update`
    /// merged: absent fields are unchanged).
    Tool(ToolCall),
    /// The agent's plan, as todo lines (`(text, status)`).
    Plan(Vec<(String, String)>),
    /// The agent asks to run something; answer with [`Command::Permission`].
    Permission {
        request: u64,
        tool: ToolCall,
        options: Vec<PermissionOption>,
    },
    /// The turn ended (`end_turn`, `cancelled`, `refusal`, ...).
    TurnEnded { stop_reason: String },
    /// Something failed; the text says what and why.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
}

/// What the session lets the person choose, as the adapter lists it: ACP's
/// `models` (older adapters) or the `model` / `effort` select options of
/// `configOptions` (newer ones), and the permission `modes`.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub models: Vec<Model>,
    pub model: Option<String>,
    /// Effort levels `(id, name)`, low to high.
    pub efforts: Vec<(String, String)>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    /// The ids of the `model` and `effort` options (Claude: `model`, `effort`;
    /// Codex: `model`, `reasoning_effort`), which set them.
    pub model_option: String,
    pub effort_option: String,
    /// Codex's plan mode is an option, not a mode.
    pub plan: Option<bool>,
}

/// Reads the choices from a `session/new` result or a `session/set_config_option`
/// result (which carries the whole `configOptions` again).
pub fn parse_config(v: &Value) -> Config {
    let mut config = Config {
        model_option: "model".into(),
        effort_option: "effort".into(),
        ..Config::default()
    };
    if let Some(models) = v["models"]["availableModels"].as_array() {
        config.models = models
            .iter()
            .filter_map(|m| {
                Some(Model {
                    id: m["modelId"].as_str()?.to_string(),
                    name: m["name"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect();
        config.model = v["models"]["currentModelId"].as_str().map(str::to_string);
    }
    for option in v["configOptions"].as_array().into_iter().flatten() {
        let choices = || {
            option["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    Some((
                        o["value"].as_str()?.to_string(),
                        o["name"].as_str().unwrap_or_default().to_string(),
                    ))
                })
        };
        let current = option["currentValue"].as_str().map(str::to_string);
        let key = match option["id"].as_str() {
            Some(id @ ("collaboration_mode" | "reasoning_effort")) => Some(id),
            id => option["category"].as_str().or(id),
        };
        match key {
            Some("model") => {
                config.models = choices().map(|(id, name)| Model { id, name }).collect();
                config.model = current;
                config.model_option = option["id"].as_str().unwrap_or("model").to_string();
            }
            Some("thought_level" | "effort" | "reasoning_effort") => {
                config.efforts = choices().filter(|(id, _)| id != "default").collect();
                config.effort = current;
                config.effort_option = option["id"].as_str().unwrap_or("effort").to_string();
            }
            Some("collaboration_mode") => config.plan = current.map(|c| c == "plan"),
            _ => {}
        }
    }
    config.mode = v["modes"]["currentModeId"].as_str().map(str::to_string);
    config
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub title: Option<String>,
    /// ACP's kind: read, edit, delete, move, search, execute, think, fetch,
    /// switch_mode, other.
    pub kind: Option<String>,
    /// pending, in_progress, completed, failed.
    pub status: Option<String>,
    pub raw_input: Option<Value>,
    /// The call's text output, when it gave any.
    pub output: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PermissionOption {
    pub id: String,
    /// allow_once, allow_always, reject_once, reject_always.
    pub kind: String,
}

/// What the interface asks of the agent.
#[derive(Debug)]
pub enum Command {
    Prompt(String),
    Cancel,
    /// Set a session option (`model`, `effort`) by id.
    SetConfig {
        id: String,
        value: String,
    },
    /// Set the permission mode (`default`, `acceptEdits`, `plan`, `auto`, `bypassPermissions`).
    SetMode(String),
    /// Answer a permission request: the chosen option, or none to cancel.
    Permission {
        request: u64,
        option: Option<String>,
    },
}

/// A running agent: send it [`Command`]s.
#[derive(Clone)]
pub struct Agent {
    commands: mpsc::UnboundedSender<Command>,
}

impl Agent {
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "the agent is not running".to_string())
    }
}

/// The adapter: `CLAUDE_ACP_BIN`, else `claude-agent-acp` on `PATH`.
/// An agent this host can drive over ACP. Each is its vendor's own CLI (or
/// the adapter that wraps it), signed in with the person's subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub const ALL: [Harness; 2] = [Harness::Claude, Harness::Codex];

    /// The id the page knows it by (`<id>/<model>` is a model's value).
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    /// The page's mark for it (ds/mark.luau).
    pub fn mark(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "openai",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    fn bin_override(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_ACP_BIN",
            Self::Codex => "CODEX_ACP_BIN",
        }
    }

    fn host(self) -> tod_agent::AcpHost {
        match self {
            Self::Claude => tod_agent::AcpHost::Claude,
            Self::Codex => tod_agent::AcpHost::OpenAi,
        }
    }

    /// The ACP mode each of the page's access levels is, for this agent.
    pub fn mode_for(self, access: &str) -> Option<&'static str> {
        Some(match (self, access) {
            (Self::Claude, "supervised") => "default",
            (Self::Claude, "edits") => "acceptEdits",
            (Self::Claude, "auto") => "auto",
            (Self::Claude, "full") => "bypassPermissions",
            (Self::Codex, "supervised") => "read-only",
            (Self::Codex, "edits") => "workspace-write",
            (Self::Codex, "auto") => "agent",
            (Self::Codex, "full") => "agent-full-access",
            _ => return None,
        })
    }

    /// The access level a mode of this agent is.
    pub fn access_of(self, mode: &str) -> Option<&'static str> {
        ["supervised", "edits", "auto", "full"]
            .into_iter()
            .find(|a| self.mode_for(a) == Some(mode))
    }
}

pub fn find_adapter(harness: Harness) -> Result<PathBuf, String> {
    let var = harness.bin_override();
    if let Some(path) = std::env::var_os(var) {
        let path = PathBuf::from(path);
        return path
            .canonicalize()
            .map_err(|error| format!("{var} points to {}. {error}", path.display()))
            .and_then(|canonical| {
                if canonical.is_file() {
                    // Keep the executable's filename, including when it is a CLI shim.
                    if path.is_absolute() {
                        Ok(path)
                    } else {
                        std::env::current_dir()
                            .map(|cwd| cwd.join(path))
                            .map_err(|error| error.to_string())
                    }
                } else {
                    Err(format!(
                        "{var} points to {}, which is not a file.",
                        path.display()
                    ))
                }
            });
    }
    harness
        .host()
        .resolve_bin()
        .map_err(|error| format!("{error:#}"))
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Starts the adapter in `cwd` and opens a session. Events arrive on the
/// returned receiver, ending after an [`Event::Error`] if the agent dies.
pub fn spawn(
    handle: &tokio::runtime::Handle,
    cwd: PathBuf,
    harness: Harness,
) -> (Agent, mpsc::UnboundedReceiver<Event>) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let failed = event_tx.clone();
    handle.spawn(async move {
        if let Err(error) = run(cwd, harness, command_rx, event_tx).await {
            let _ = failed.send(Event::Error(error));
        }
    });
    (
        Agent {
            commands: command_tx,
        },
        event_rx,
    )
}

async fn run(
    cwd: PathBuf,
    harness: Harness,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Event>,
) -> Result<(), String> {
    let adapter = find_adapter(harness)?;
    let mut child = Process::new(&adapter)
        .current_dir(&cwd)
        .env_remove("CLAUDECODE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("starting {}: {e}", adapter.display()))?;
    let mut stdin = child.stdin.take().ok_or("the adapter has no stdin")?;
    let stdout = child.stdout.take().ok_or("the adapter has no stdout")?;

    // One writer owns stdin.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            let mut line = message.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });

    let pending: Pending = Arc::default();
    let next_id = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let session = Arc::new(Mutex::new(String::new()));

    // The reader: responses to the pending calls, updates and requests out.
    {
        let (pending, events, out_tx, session) = (
            pending.clone(),
            events.clone(),
            out_tx.clone(),
            session.clone(),
        );
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(message) => route(message, &pending, &events, &out_tx, &session),
                            Err(error) => {
                                let _ = events.send(Event::Error(format!(
                                    "the adapter said something that is not JSON ({error}): {line}"
                                )));
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = events.send(Event::Error("the agent exited".into()));
                        break;
                    }
                    Err(error) => {
                        let _ = events.send(Event::Error(format!("reading the agent: {error}")));
                        break;
                    }
                }
            }
            pending.lock().unwrap().clear();
        });
    }

    let call = {
        let (pending, out_tx, next_id) = (pending.clone(), out_tx.clone(), next_id.clone());
        move |method: &'static str, params: Value| {
            let (pending, out_tx, next_id) = (pending.clone(), out_tx.clone(), next_id.clone());
            async move {
                let id = next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                out_tx
                    .send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
                    .map_err(|_| format!("{method}: the agent is gone"))?;
                rx.await
                    .map_err(|_| format!("{method}: the agent went away before answering"))?
            }
        }
    };

    call(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        }),
    )
    .await
    .map_err(|e| format!("initialize failed: {e}"))?;
    let opened = call("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
        .await
        .map_err(|e| format!("opening a session failed: {e}"))?;
    let session_id = opened["sessionId"]
        .as_str()
        .ok_or("session/new gave no sessionId")?
        .to_string();
    *session.lock().unwrap() = session_id.clone();
    let _ = events.send(Event::Ready(parse_config(&opened)));

    while let Some(command) = commands.recv().await {
        match command {
            Command::Prompt(text) => {
                let (events, session_id, call) = (events.clone(), session_id.clone(), call.clone());
                tokio::spawn(async move {
                    let reply = call(
                        "session/prompt",
                        json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] }),
                    )
                    .await;
                    let _ = events.send(match reply {
                        Ok(v) => Event::TurnEnded {
                            stop_reason: v["stopReason"].as_str().unwrap_or("end_turn").to_string(),
                        },
                        Err(error) => Event::Error(format!("the turn failed: {error}")),
                    });
                });
            }
            Command::SetConfig { id, value } => {
                // Awaited here, not spawned: a prompt sent after it must follow it.
                match call(
                    "session/set_config_option",
                    json!({ "sessionId": session_id, "configId": id, "value": value }),
                )
                .await
                {
                    Ok(v) => {
                        let _ = events.send(Event::Config(parse_config(&v)));
                    }
                    Err(error) => {
                        let _ = events.send(Event::Error(format!(
                            "could not set {id} to {value}: {error}"
                        )));
                    }
                }
            }
            Command::SetMode(mode) => {
                if let Err(error) = call(
                    "session/set_mode",
                    json!({ "sessionId": session_id, "modeId": mode }),
                )
                .await
                {
                    let _ = events.send(Event::Error(format!(
                        "could not set the mode to {mode}: {error}"
                    )));
                }
            }
            Command::Cancel => {
                let _ = out_tx.send(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session_id } }));
            }
            Command::Permission { request, option } => {
                let outcome = match option {
                    Some(id) => json!({ "outcome": "selected", "optionId": id }),
                    None => json!({ "outcome": "cancelled" }),
                };
                let _ = out_tx.send(
                    json!({ "jsonrpc": "2.0", "id": request, "result": { "outcome": outcome } }),
                );
            }
        }
    }
    drop(child);
    Ok(())
}

fn route(
    message: Value,
    pending: &Pending,
    events: &mpsc::UnboundedSender<Event>,
    out: &mpsc::UnboundedSender<Value>,
    _session: &Arc<Mutex<String>>,
) {
    let method = message["method"].as_str();
    let id = message["id"].as_u64();
    match (method, id) {
        // A response to one of ours.
        (None, Some(id)) => {
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let result = match message.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| error.to_string())),
                    None => Ok(message["result"].clone()),
                };
                let _ = tx.send(result);
            }
        }
        (Some("session/update"), None) => {
            if let Some(event) = update(&message["params"]["update"]) {
                let _ = events.send(event);
            }
        }
        (Some("session/request_permission"), Some(id)) => {
            let params = &message["params"];
            let options = params["options"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|o| {
                            Some(PermissionOption {
                                id: o["optionId"].as_str()?.to_string(),

                                kind: o["kind"].as_str().unwrap_or_default().to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let _ = events.send(Event::Permission {
                request: id,
                tool: tool_call(&params["toolCall"]),
                options,
            });
        }
        // Anything else the agent asks of us (fs, terminal): not offered.
        (Some(method), Some(id)) => {
            let _ = out.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("{method} is not offered by this client") },
            }));
        }
        _ => {}
    }
}

fn update(update: &Value) -> Option<Event> {
    match update["sessionUpdate"].as_str()? {
        kind @ ("agent_message_chunk" | "agent_thought_chunk") => {
            let text = update["content"]["text"].as_str()?.to_string();
            Some(Event::Chunk {
                thought: kind == "agent_thought_chunk",
                text,
            })
        }
        "tool_call" | "tool_call_update" => Some(Event::Tool(tool_call(update))),
        "plan" => Some(Event::Plan(
            update["entries"]
                .as_array()?
                .iter()
                .map(|e| {
                    (
                        e["content"].as_str().unwrap_or_default().to_string(),
                        e["status"].as_str().unwrap_or("pending").to_string(),
                    )
                })
                .collect(),
        )),
        _ => None,
    }
}

fn tool_call(value: &Value) -> ToolCall {
    let text = |v: &Value| v.as_str().map(str::to_string);
    let output = value["content"].as_array().map(|parts| {
        parts
            .iter()
            .filter_map(|p| {
                p["content"]["text"]
                    .as_str()
                    .or_else(|| p["newText"].as_str())
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    ToolCall {
        id: text(&value["toolCallId"]).unwrap_or_default(),
        title: text(&value["title"]),
        kind: text(&value["kind"]),
        status: text(&value["status"]),
        raw_input: value.get("rawInput").cloned().filter(|v| !v.is_null()),
        output: output.filter(|s| !s.is_empty()),
        path: value["locations"][0]["path"]
            .as_str()
            .map(|p| Path::new(p).to_string_lossy().into_owned()),
    }
}
