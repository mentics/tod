use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState, DeepDiveContext, RunId,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const DEFAULT_MODEL: &str = "auto";
const AUTH_TIMEOUT: Duration = Duration::from_secs(120);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug)]
enum WorkerMessage {
    Completed(Result<String>),
}

struct ActiveRun {
    kind: AgentRunKind,
    state: AgentRunState,
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    receiver: Receiver<WorkerMessage>,
}

/// Cursor Agent CLI backend over ACP (Agent Client Protocol).
pub struct CursorAcpProvider {
    agent_bin: PathBuf,
    model: String,
    runs: HashMap<RunId, ActiveRun>,
}

impl CursorAcpProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            agent_bin: resolve_agent_bin()?,
            model: DEFAULT_MODEL.to_string(),
            runs: HashMap::new(),
        })
    }

    pub fn with_agent_bin(agent_bin: PathBuf) -> Self {
        Self {
            agent_bin,
            model: DEFAULT_MODEL.to_string(),
            runs: HashMap::new(),
        }
    }

    fn spawn_run(
        &mut self,
        kind: AgentRunKind,
        cwd: PathBuf,
        prompt: String,
    ) -> Result<AgentRunHandle> {
        let id = RunId::new();
        let (tx, rx) = mpsc::channel();
        let agent_bin = self.agent_bin.clone();
        let model = self.model.clone();
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let child_for_worker = child_slot.clone();
        let cancelled_for_worker = cancelled.clone();

        let worker = thread::spawn(move || {
            let result = run_acp_session(
                &agent_bin,
                &cwd,
                &model,
                &prompt,
                child_for_worker,
                cancelled_for_worker,
            );
            let _ = tx.send(WorkerMessage::Completed(result));
        });

        self.runs.insert(
            id,
            ActiveRun {
                kind,
                state: AgentRunState::InFlight,
                child: child_slot,
                cancelled,
                worker: Some(worker),
                receiver: rx,
            },
        );

        Ok(AgentRunHandle {
            id,
            kind,
            state: AgentRunState::InFlight,
        })
    }
}

impl Default for CursorAcpProvider {
    fn default() -> Self {
        Self::new().unwrap_or_else(|err| {
            eprintln!("Cursor ACP provider init failed: {err}; using placeholder agent path");
            Self::with_agent_bin(PathBuf::from("agent"))
        })
    }
}

impl AgentProvider for CursorAcpProvider {
    fn start_researcher_replenishment(
        &mut self,
        cwd: PathBuf,
        prompt: String,
    ) -> Result<AgentRunHandle> {
        self.spawn_run(AgentRunKind::ResearcherReplenishment, cwd, prompt)
    }

    fn start_answer_processor(&mut self, cwd: PathBuf, prompt: String) -> Result<AgentRunHandle> {
        self.spawn_run(AgentRunKind::AnswerProcessor, cwd, prompt)
    }

    fn start_deep_dive_chat(
        &mut self,
        cwd: PathBuf,
        context: DeepDiveContext,
        initial_message: Option<String>,
    ) -> Result<AgentRunHandle> {
        let prompt = build_deep_dive_prompt(&context, initial_message.as_deref());
        self.spawn_run(AgentRunKind::DeepDiveChat, cwd, prompt)
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        let run = self.runs.get_mut(&id)?;
        if matches!(run.state, AgentRunState::InFlight) {
            if let Ok(WorkerMessage::Completed(result)) = run.receiver.try_recv() {
                run.state = match result {
                    Ok(text) => AgentRunState::Success(Some(text)),
                    Err(err) => AgentRunState::Failure(err.to_string()),
                };
            }
        }
        Some(run.state.clone())
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        if let Some(mut run) = self.runs.remove(&id) {
            run.cancelled.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = run.child.lock() {
                if let Some(mut child) = guard.take() {
                    kill_child_tree(&mut child);
                }
            }
            if let Some(worker) = run.worker.take() {
                let _ = worker.join();
            }
        }
        Ok(())
    }
}

fn build_deep_dive_prompt(context: &DeepDiveContext, initial_message: Option<&str>) -> String {
    let mut prompt = format!(
        "Deep-dive chat for interview question {}.\n\
         Project: {}\n\
         Task: {}\n\
         Lifecycle state: {}\n\
         Interview purpose: {}\n\
         Interview phase: {}\n\
         Question:\n{}\n",
        context.question_id,
        context.project,
        context.task,
        context.lifecycle_state,
        context.interview_purpose,
        context.interview_phase,
        context.question_body,
    );
    if let Some(message) = initial_message {
        prompt.push_str("\nUser message:\n");
        prompt.push_str(message);
    }
    prompt
}

fn resolve_agent_bin() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("AGENT_BIN") {
        return Ok(PathBuf::from(path));
    }

    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(&local_app_data).join("cursor-agent").join("agent.cmd"));
            candidates.push(
                PathBuf::from(&local_app_data)
                    .join("cursor-agent")
                    .join("cursor-agent.cmd"),
            );
        }
    } else if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".local").join("bin").join("agent"));
    }
    candidates.push(PathBuf::from("agent"));

    for candidate in candidates {
        if candidate == PathBuf::from("agent") || candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!("Cursor agent CLI not found. Install from https://cursor.com/install or set AGENT_BIN.")
}

fn spawn_agent(agent_bin: &Path) -> Result<Child> {
    let mut command = if agent_bin
        .extension()
        .is_some_and(|ext| ext == "cmd" || ext == "bat")
    {
        // Windows `.cmd`/`.bat` must go through `cmd /C`. Cancel must use
        // `kill_child_tree` so the agent grandchild is terminated, not only cmd.
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", &agent_bin.to_string_lossy(), "acp"]);
        cmd
    } else {
        let mut cmd = Command::new(agent_bin);
        cmd.arg("acp");
        cmd
    };

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    command
        .spawn()
        .with_context(|| format!("failed to spawn {}", agent_bin.display()))
}

/// Terminate `child` and, on Windows, its entire process tree.
///
/// Spawning `*.cmd`/`*.bat` via `cmd /C` makes `Child::kill` only stop the
/// wrapper; the real agent is often a grandchild and would otherwise orphan.
fn kill_child_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = child.wait();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn run_acp_session(
    agent_bin: &Path,
    cwd: &Path,
    model: &str,
    prompt: &str,
    child_slot: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
) -> Result<String> {
    let mut child = spawn_agent(agent_bin)?;
    let stdin = child.stdin.take().context("agent stdin unavailable")?;
    let stdout = child.stdout.take().context("agent stdout unavailable")?;
    {
        let mut guard = child_slot.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(child);
    }

    let (request_tx, request_rx) = mpsc::channel::<AcpRequest>();
    let reader_handle = thread::spawn(move || read_stdout_lines(stdout, request_tx));

    let mut session = AcpClient {
        stdin,
        next_id: 1,
        request_rx,
        assistant_text: String::new(),
        cancelled: cancelled.clone(),
    };

    let result = (|| -> Result<String> {
        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        session.send_request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": { "name": "tod-interview-ui", "version": "0.1.0" }
            }),
        )?;
        session.await_response(AUTH_TIMEOUT)?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        session.send_request("authenticate", json!({ "methodId": "cursor_login" }))?;
        session.await_response(AUTH_TIMEOUT)?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        session.send_request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))?;
        let session_result = session.await_response(AUTH_TIMEOUT)?;
        let session_id = session_result
            .get("sessionId")
            .and_then(Value::as_str)
            .context("session/new missing sessionId")?;

        if let Some(model_option) = pick_model_option(session_result.get("configOptions")) {
            let target = if has_model_value(&model_option, model) {
                model
            } else {
                model_option
                    .get("currentValue")
                    .and_then(Value::as_str)
                    .unwrap_or(model)
            };
            if model_option.get("currentValue").and_then(Value::as_str) != Some(target) {
                session.send_request(
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        "configId": model_option.get("id").cloned().unwrap_or(json!("model")),
                        "value": target
                    }),
                )?;
                session.await_response(AUTH_TIMEOUT)?;
            }
        }

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        session.send_request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": prompt }]
            }),
        )?;
        session.await_response(PROMPT_TIMEOUT)?;
        Ok(session.assistant_text)
    })();

    if let Ok(mut guard) = child_slot.lock() {
        if let Some(mut child) = guard.take() {
            kill_child_tree(&mut child);
        }
    }
    let _ = reader_handle.join();

    if cancelled.load(Ordering::SeqCst) {
        bail!("ACP run cancelled");
    }
    result
}

struct AcpClient {
    stdin: std::process::ChildStdin,
    next_id: i64,
    request_rx: Receiver<AcpRequest>,
    assistant_text: String,
    cancelled: Arc<AtomicBool>,
}

impl AcpClient {
    fn send_request(&mut self, method: &str, params: Value) -> Result<i64> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{message}")?;
        self.stdin.flush()?;
        Ok(id)
    }

    fn await_response(&mut self, timeout: Duration) -> Result<Value> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                bail!("ACP run cancelled");
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                bail!("ACP request timed out");
            }
            match self
                .request_rx
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(AcpRequest::Response { id: _, result }) => return Ok(result),
                Ok(AcpRequest::ResponseError { message }) => bail!("ACP error: {message}"),
                Ok(AcpRequest::Notification { method, params }) => {
                    self.handle_notification(&method, params)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("ACP stdout reader disconnected")
                }
            }
        }
    }

    fn handle_notification(&mut self, method: &str, params: Value) -> Result<()> {
        match method {
            "session/update" => {
                if let Some(update) = params.get("update") {
                    if update.get("sessionUpdate").and_then(Value::as_str)
                        == Some("agent_message_chunk")
                    {
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                        {
                            self.assistant_text.push_str(text);
                        }
                    }
                }
            }
            "session/request_permission" => {
                if let Some(id) = params.get("_response_id").and_then(Value::as_i64) {
                    respond(
                        &mut self.stdin,
                        id,
                        json!({ "outcome": { "outcome": "selected", "optionId": "allow-once" } }),
                    )?;
                }
            }
            "cursor/ask_question" => {
                if let Some(id) = params.get("_response_id").and_then(Value::as_i64) {
                    respond(
                        &mut self.stdin,
                        id,
                        json!({ "outcome": { "outcome": "skipped", "reason": "tod interview ui" } }),
                    )?;
                }
            }
            "cursor/create_plan" => {
                if let Some(id) = params.get("_response_id").and_then(Value::as_i64) {
                    respond(
                        &mut self.stdin,
                        id,
                        json!({ "outcome": { "outcome": "accepted" } }),
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
enum AcpRequest {
    Response { id: i64, result: Value },
    ResponseError { message: String },
    Notification { method: String, params: Value },
}

fn read_stdout_lines(stdout: std::process::ChildStdout, tx: Sender<AcpRequest>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        if let (Some(id), Some(result)) = (value.get("id"), value.get("result")) {
            let _ = tx.send(AcpRequest::Response {
                id: id.as_i64().unwrap_or_default(),
                result: result.clone(),
            });
            continue;
        }

        if let (Some(id), Some(error)) = (value.get("id"), value.get("error")) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown ACP error")
                .to_string();
            let _ = tx.send(AcpRequest::ResponseError {
                message: message.clone(),
            });
            // Also satisfy await_response for request/response pairs.
            let _ = tx.send(AcpRequest::Response {
                id: id.as_i64().unwrap_or_default(),
                result: json!({}),
            });
            continue;
        }

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let mut params = value.get("params").cloned().unwrap_or(json!({}));
            if let Some(id) = value.get("id") {
                if let Some(obj) = params.as_object_mut() {
                    obj.insert("_response_id".to_string(), id.clone());
                }
            }
            let _ = tx.send(AcpRequest::Notification {
                method: method.to_string(),
                params,
            });
        }
    }
}

fn respond(stdin: &mut std::process::ChildStdin, id: i64, result: Value) -> Result<()> {
    let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    writeln!(stdin, "{message}")?;
    stdin.flush()?;
    Ok(())
}

fn pick_model_option(config_options: Option<&Value>) -> Option<Value> {
    let options = config_options?.as_array()?;
    options
        .iter()
        .find(|o| o.get("category").and_then(Value::as_str) == Some("model"))
        .or_else(|| {
            options
                .iter()
                .find(|o| o.get("id").and_then(Value::as_str) == Some("model"))
        })
        .cloned()
}

fn has_model_value(model_option: &Value, model_id: &str) -> bool {
    model_option
        .get("options")
        .and_then(Value::as_array)
        .is_some_and(|options| {
            options
                .iter()
                .any(|o| o.get("value").and_then(Value::as_str) == Some(model_id))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_dive_prompt_includes_context() {
        let prompt = build_deep_dive_prompt(
            &DeepDiveContext {
                project: "interview-ui".into(),
                task: "core-ui".into(),
                lifecycle_state: "active".into(),
                interview_purpose: "implementation".into(),
                interview_phase: "design".into(),
                question_id: "q-001".into(),
                question_body: "How should settings persist?".into(),
            },
            Some("Let's explore trade-offs"),
        );
        assert!(prompt.contains("q-001"));
        assert!(prompt.contains("Let's explore trade-offs"));
    }

    #[cfg(windows)]
    #[test]
    fn kill_child_tree_terminates_cmd_and_grandchild() {
        // Mimic ACP spawn: cmd /C wraps a long-running child (ping). Plain
        // Child::kill would leave ping; taskkill /T must clear the tree.
        let mut child = Command::new("cmd")
            .args(["/C", "ping", "-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd wrapper");
        let wrapper_pid = child.id();
        thread::sleep(Duration::from_millis(400));

        let child_pids = windows_child_pids(wrapper_pid);
        assert!(
            !child_pids.is_empty(),
            "expected grandchild under cmd wrapper pid {wrapper_pid}"
        );

        kill_child_tree(&mut child);
        thread::sleep(Duration::from_millis(300));

        assert!(
            !process_alive(wrapper_pid),
            "cmd wrapper pid {wrapper_pid} still alive after kill_child_tree"
        );
        for pid in child_pids {
            assert!(
                !process_alive(pid),
                "grandchild pid {pid} still alive after kill_child_tree"
            );
        }
    }

    #[cfg(windows)]
    fn windows_child_pids(parent_pid: u32) -> Vec<u32> {
        let filter = format!("ParentProcessId={parent_pid}");
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "(Get-CimInstance Win32_Process -Filter '{filter}').ProcessId"
                ),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .expect("powershell child query");
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .filter_map(|tok| tok.parse().ok())
            .collect()
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .expect("tasklist");
        let text = String::from_utf8_lossy(&output.stdout);
        text.contains(&pid.to_string())
    }
}
