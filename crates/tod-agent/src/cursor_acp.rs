use super::acp_host::{AcpHost, is_standalone_acp_server, spawn_acp_process};
use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState, PermissionOption,
    PermissionRequest, RunId, SessionPurpose, SessionTurn,
};
use crate::agent_launch::{AgentLaunchOptions, effort_for_acp};
use crate::agent_traffic::{
    InterviewAgentCounts, SharedAgentTrafficLog, TrafficDirection, TrafficTag,
};
use crate::ReplyPart;
use crate::process_tree::AgentProcess;
use crate::reply::{self, SharedReplyParts};
use crate::util::normalize_absolute;
use crate::util::path_is_under;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const AUTH_TIMEOUT: Duration = Duration::from_secs(120);
/// Idle bound on a prompt turn — reset by every notification the agent sends
/// (tool calls, permission requests, message chunks), so a turn only times
/// out once the agent goes silent for this long, not after this long overall.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
/// Idle time after which a conversation's agent process is released. The
/// agent-side session survives, so the next message resumes it.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long to wait for Claude Code to start a new session's log before naming
/// it. The log appears once the first prompt reaches the agent.
const SESSION_LOG_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug)]
enum WorkerMessage {
    Completed(Result<String>),
}

/// A permission request awaiting the user's decision, and how to deliver it
/// back to the blocked `AcpClient` thread.
struct PendingPermission {
    request: PermissionRequest,
    reply: Sender<String>,
}

type PendingPermissionSlot = Arc<Mutex<Option<PendingPermission>>>;

struct ActiveRun {
    kind: AgentRunKind,
    state: AgentRunState,
    child: Arc<Mutex<Option<AgentProcess>>>,
    cancelled: Arc<AtomicBool>,
    /// Latest human-readable activity reported by the agent, shared with the
    /// `AcpClient` driving this run.
    activity: Arc<Mutex<Option<String>>>,
    /// Set by the `AcpClient` while it is blocked on `session/request_permission`.
    pending_permission: PendingPermissionSlot,
    /// Agent-side session id, set once `session/new` returns it. Fleet-agent
    /// runs (`start_fleet_agent`) are one-shot processes with no `conversations`
    /// entry, so this is their only way to expose the id a caller needs to
    /// persist for later resume — see `fleet_run_session_id`.
    session_id: Arc<Mutex<Option<String>>>,
    worker: Option<JoinHandle<()>>,
    receiver: Receiver<WorkerMessage>,
}

/// How a conversation process attaches to its agent-side session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionStart {
    New,
    Resume(String),
}

#[derive(Debug)]
enum ConversationCommand {
    Turn {
        run_id: RunId,
        blocks: Vec<String>,
        /// Name for the agent-side session, if this turn creates one.
        title: String,
        reply: Sender<WorkerMessage>,
    },
    Shutdown,
}

/// Everything a conversation worker needs to (re)connect its process.
#[derive(Clone)]
struct ConversationSpec {
    host: AcpHost,
    agent_bin: PathBuf,
    cwd: PathBuf,
    model: String,
    effort: String,
    write_roots: Arc<Vec<PathBuf>>,
    traffic_log: Option<SharedAgentTrafficLog>,
    env: Arc<Vec<(String, String)>>,
    purpose: SessionPurpose,
    tag: TrafficTag,
}

/// A long-lived conversation: a worker thread owning at most one agent process.
struct LiveConversation {
    cmd_tx: Sender<ConversationCommand>,
    child: Arc<Mutex<Option<AgentProcess>>>,
    cancelled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    session_id: Arc<Mutex<Option<String>>>,
    /// Latest human-readable activity reported by the agent for the turn in
    /// progress, if any.
    activity: Arc<Mutex<Option<String>>>,
    /// Set while the current turn is blocked on `session/request_permission`.
    pending_permission: PendingPermissionSlot,
    purpose: SessionPurpose,
    /// Characters that entered this conversation's context (see
    /// [`AgentProvider::session_context_chars`]).
    context_chars: Arc<AtomicU64>,
    /// The parts of the latest turn (see
    /// [`AgentProvider::session_reply_parts`]).
    reply_parts: SharedReplyParts,
}

impl LiveConversation {
    fn spawn(spec: ConversationSpec, resume_session_id: Option<String>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let purpose = spec.purpose;
        let worker = ConversationWorker {
            spec,
            child: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            session_id: Arc::new(Mutex::new(resume_session_id)),
            activity: Arc::new(Mutex::new(None)),
            pending_permission: Arc::new(Mutex::new(None)),
            context_chars: Arc::new(AtomicU64::new(0)),
            reply_parts: SharedReplyParts::default(),
        };
        let conversation = Self {
            cmd_tx,
            child: worker.child.clone(),
            cancelled: worker.cancelled.clone(),
            closed: worker.closed.clone(),
            session_id: worker.session_id.clone(),
            activity: worker.activity.clone(),
            pending_permission: worker.pending_permission.clone(),
            purpose,
            context_chars: worker.context_chars.clone(),
            reply_parts: worker.reply_parts.clone(),
        };
        thread::spawn(move || worker.run(cmd_rx));
        conversation
    }

    fn session_id(&self) -> Option<String> {
        self.session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stop the worker; it releases the process on its way out. A turn in
    /// progress is cancelled rather than waited for.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.cmd_tx.send(ConversationCommand::Shutdown);
    }
}

struct ConversationWorker {
    spec: ConversationSpec,
    child: Arc<Mutex<Option<AgentProcess>>>,
    cancelled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    session_id: Arc<Mutex<Option<String>>>,
    activity: Arc<Mutex<Option<String>>>,
    pending_permission: PendingPermissionSlot,
    context_chars: Arc<AtomicU64>,
    reply_parts: SharedReplyParts,
}

impl ConversationWorker {
    fn run(self, commands: Receiver<ConversationCommand>) {
        let mut live: Option<PersistentAcpSession> = None;
        loop {
            let command = match commands.recv_timeout(SESSION_IDLE_TIMEOUT) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(session) = live.take() {
                        tracing::info!(
                            event = "agent",
                            action = "acp_session_idle",
                            session_id = %session.session_id,
                            "releasing idle agent process; the session stays resumable"
                        );
                        session.shutdown(self.child.clone());
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let ConversationCommand::Turn {
                run_id,
                blocks,
                title,
                reply,
            } = command
            else {
                break;
            };
            if self.closed.load(Ordering::SeqCst) {
                let _ = reply.send(WorkerMessage::Completed(Err(anyhow::anyhow!(
                    "agent session closed"
                ))));
                break;
            }
            self.cancelled.store(false, Ordering::SeqCst);
            let result = self.turn(&mut live, run_id, &blocks, &title);
            if result.is_err() {
                // A failed or cancelled turn can leave the process mid-reply;
                // the next message starts clean by resuming the session.
                if let Some(session) = live.take() {
                    session.shutdown(self.child.clone());
                }
            }
            let _ = reply.send(WorkerMessage::Completed(result));
        }
        if let Some(session) = live.take() {
            session.shutdown(self.child.clone());
        }
    }

    fn current_session_id(&self) -> Option<String> {
        self.session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn turn(
        &self,
        live: &mut Option<PersistentAcpSession>,
        run_id: RunId,
        blocks: &[String],
        title: &str,
    ) -> Result<String> {
        *self.activity.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .pending_permission
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let mut created = false;
        if live.is_none() {
            let start = match self.current_session_id() {
                Some(id) => SessionStart::Resume(id),
                None => SessionStart::New,
            };
            let spec = &self.spec;
            let session = PersistentAcpSession::connect(
                spec.host,
                &spec.agent_bin,
                &spec.cwd,
                &spec.model,
                &spec.effort,
                self.child.clone(),
                self.cancelled.clone(),
                self.activity.clone(),
                self.pending_permission.clone(),
                &spec.write_roots,
                &start,
                spec.traffic_log.clone(),
                spec.tag.clone(),
                spec.purpose.run_kind(),
                &spec.env,
                Some(self.context_chars.clone()),
                Some(self.reply_parts.clone()),
            )?;
            *self.session_id.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(session.session_id.clone());
            *live = Some(session);
            created = start == SessionStart::New;
        }
        let name = created.then_some((self.spec.host, title));
        live.as_mut()
            .expect("connected above")
            .prompt_blocks(blocks, run_id, name)
    }
}

/// ACP agent backend (Cursor, Claude, …).
pub struct CursorAcpProvider {
    host: AcpHost,
    agent_bin: PathBuf,
    /// Extra directories the agent may write to, supplied by the caller.
    /// This crate does not resolve the data root itself.
    extra_write_roots: Arc<Vec<PathBuf>>,
    runs: HashMap<RunId, ActiveRun>,
    /// Where each run's traffic is filed.
    fleet_run_context: HashMap<RunId, TrafficTag>,
    /// Long-lived conversations by caller key (see [`SessionTurn`]).
    conversations: HashMap<String, LiveConversation>,
    traffic_log: Option<SharedAgentTrafficLog>,
}

impl CursorAcpProvider {
    /// Grant the agent write access to additional roots (e.g. the tod data
    /// root). Resolving those paths is the caller's job - this crate does not
    /// discover them.
    pub fn with_write_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.extra_write_roots = Arc::new(roots);
        self
    }

    pub fn for_host(host: AcpHost) -> Result<Self> {
        Ok(Self {
            host,
            agent_bin: host.resolve_bin()?,
            extra_write_roots: Arc::new(Vec::new()),
            runs: HashMap::new(),
            fleet_run_context: HashMap::new(),
            conversations: HashMap::new(),
            traffic_log: None,
        })
    }

    pub fn with_traffic_log(mut self, traffic_log: SharedAgentTrafficLog) -> Self {
        self.traffic_log = Some(traffic_log);
        self
    }

    fn run_id_string(id: RunId) -> String {
        format!("{id:?}")
    }

    fn log_traffic(
        &self,
        kind: AgentRunKind,
        run_id: RunId,
        direction: TrafficDirection,
        content: &str,
    ) {
        let Some(log) = &self.traffic_log else {
            return;
        };
        let tag = self.fleet_run_context.get(&run_id).cloned().unwrap_or_else(|| {
            TrafficTag::new(Self::run_id_string(run_id), "", kind.traffic_label())
        });
        tag.record(log, kind.traffic_category(), direction, content);
    }

    pub fn with_agent_bin(host: AcpHost, agent_bin: PathBuf) -> Self {
        Self {
            host,
            agent_bin,
            extra_write_roots: Arc::new(Vec::new()),
            runs: HashMap::new(),
            fleet_run_context: HashMap::new(),
            conversations: HashMap::new(),
            traffic_log: None,
        }
    }

    fn spawn_run(
        &mut self,
        kind: AgentRunKind,
        owner_id: &str,
        cwd: PathBuf,
        prompt: String,
        model: String,
        effort: String,
        session_title: String,
    ) -> Result<AgentRunHandle> {
        let id = RunId::new();
        let (tx, rx) = mpsc::channel();
        let agent_bin = self.agent_bin.clone();
        let host = self.host;
        let child_slot: Arc<Mutex<Option<AgentProcess>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let activity: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let pending_permission: PendingPermissionSlot = Arc::new(Mutex::new(None));
        let child_for_worker = child_slot.clone();
        let cancelled_for_worker = cancelled.clone();
        let activity_for_worker = activity.clone();
        let pending_permission_for_worker = pending_permission.clone();

        tracing::info!(
            event = "agent",
            action = "acp_spawn",
            run_id = ?id,
            ?kind,
            host = host.label(),
            agent_bin = %agent_bin.display(),
            cwd = %cwd.display(),
            model = %model,
            effort = %effort,
            prompt_chars = prompt.len(),
            "starting ACP run"
        );

        let tag = TrafficTag::new(owner_id, &session_title, kind.traffic_label());
        self.fleet_run_context.insert(id, tag.clone());
        self.log_traffic(kind, id, TrafficDirection::Request, &prompt);

        let traffic_log = self.traffic_log.clone();
        let write_roots = self.extra_write_roots.clone();
        let session_id_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let session_id_for_worker = session_id_slot.clone();
        let worker = thread::spawn(move || {
            let result = run_acp_session(
                host,
                &agent_bin,
                &cwd,
                &model,
                &effort,
                &prompt,
                child_for_worker,
                cancelled_for_worker,
                activity_for_worker,
                pending_permission_for_worker,
                session_id_for_worker,
                traffic_log,
                tag,
                id,
                kind,
                &write_roots,
                &session_title,
            );
            match &result {
                Ok(text) => tracing::info!(
                    event = "agent",
                    action = "acp_completed",
                    host = host.label(),
                    assistant_chars = text.len(),
                    "ACP run completed successfully"
                ),
                Err(err) => tracing::error!(
                    event = "agent",
                    action = "acp_failed",
                    host = host.label(),
                    error = %err,
                    "ACP run failed"
                ),
            }
            let _ = tx.send(WorkerMessage::Completed(result));
        });

        self.runs.insert(
            id,
            ActiveRun {
                kind,
                state: AgentRunState::InFlight(None),
                child: child_slot,
                cancelled,
                activity,
                pending_permission,
                session_id: session_id_slot,
                worker: Some(worker),
                receiver: rx,
            },
        );

        Ok(AgentRunHandle { id })
    }
}

impl Drop for CursorAcpProvider {
    fn drop(&mut self) {
        // Workers release their processes as they exit, but the app may exit
        // first; do not leave agent processes behind.
        for (_, conversation) in self.conversations.drain() {
            conversation.close();
            let child = conversation
                .child
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(mut child) = child {
                child.kill_tree();
            }
        }
    }
}

impl Default for CursorAcpProvider {
    fn default() -> Self {
        Self::for_host(AcpHost::Cursor).unwrap_or_else(|err| {
            eprintln!("Cursor ACP provider init failed: {err}; using placeholder agent path");
            Self::with_agent_bin(AcpHost::Cursor, PathBuf::from("agent"))
        })
    }
}

impl AgentProvider for CursorAcpProvider {

    fn start_fleet_agent(
        &mut self,
        owner_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
        session_title: String,
    ) -> Result<AgentRunHandle> {
        self.spawn_run(
            AgentRunKind::FleetAgent,
            owner_id,
            cwd,
            prompt,
            options.model,
            options.effort,
            session_title,
        )
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> Result<AgentRunHandle> {
        let blocks = turn.prompt_blocks();
        let SessionTurn {
            key,
            title,
            cwd,
            options,
            resume_session_id,
            purpose,
            env,
            ..
        } = turn;
        // Keyed by the session, not its owner: one owner can hold several.
        let tag = TrafficTag::new(key.clone(), &title, purpose.run_kind().traffic_label());
        let spec = ConversationSpec {
            host: self.host,
            agent_bin: self.agent_bin.clone(),
            cwd,
            model: options.model,
            effort: options.effort,
            write_roots: self.extra_write_roots.clone(),
            traffic_log: self.traffic_log.clone(),
            env: Arc::new(env),
            purpose,
            tag: tag.clone(),
        };

        let id = RunId::new();
        let (reply, receiver) = mpsc::channel();
        let command = ConversationCommand::Turn {
            run_id: id,
            blocks: blocks.clone(),
            title,
            reply,
        };
        let conversation = self
            .conversations
            .entry(key.clone())
            .or_insert_with(|| LiveConversation::spawn(spec.clone(), resume_session_id.clone()));
        if let Err(mpsc::SendError(command)) = conversation.cmd_tx.send(command) {
            // The worker is gone: start another, resuming the session it reached.
            let resume = conversation.session_id().or(resume_session_id);
            let fresh = LiveConversation::spawn(spec, resume);
            fresh
                .cmd_tx
                .send(command)
                .map_err(|_| anyhow::anyhow!("agent session worker unavailable"))?;
            self.conversations.insert(key.clone(), fresh);
        }

        self.fleet_run_context.insert(id, tag);
        self.log_traffic(
            purpose.run_kind(),
            id,
            TrafficDirection::Request,
            &blocks.join("\n\n"),
        );
        let conversation = &self.conversations[&key];
        self.runs.insert(
            id,
            ActiveRun {
                kind: purpose.run_kind(),
                state: AgentRunState::InFlight(None),
                child: conversation.child.clone(),
                cancelled: conversation.cancelled.clone(),
                activity: conversation.activity.clone(),
                pending_permission: conversation.pending_permission.clone(),
                // Chat-turn callers read the session id via `session_id(key)`
                // (the `conversations` map), not this slot — it's only
                // populated for one-shot fleet-agent runs.
                session_id: Arc::new(Mutex::new(None)),
                worker: None,
                receiver,
            },
        );
        Ok(AgentRunHandle { id })
    }

    fn session_id(&self, key: &str) -> Option<String> {
        self.conversations
            .get(key)
            .and_then(LiveConversation::session_id)
    }

    fn fleet_run_session_id(&self, id: RunId) -> Option<String> {
        self.runs.get(&id).and_then(|run| {
            run.session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        })
    }

    fn fetch_full_transcript(
        &self,
        _platform: crate::platform::AgentPlatform,
        cwd: &Path,
        agent_session_id: &str,
    ) -> Result<String> {
        fetch_transcript(
            self.host,
            &self.agent_bin,
            cwd,
            agent_session_id,
            self.traffic_log.clone(),
        )
    }

    fn session_context_chars(&self, key: &str) -> Option<u64> {
        self.conversations
            .get(key)
            .map(|conversation| conversation.context_chars.load(Ordering::Relaxed))
    }

    fn session_reply_parts(&self, key: &str) -> Option<Vec<ReplyPart>> {
        self.conversations.get(key).map(|conversation| {
            conversation
                .reply_parts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        })
    }

    fn close_session(&mut self, key: &str) {
        if let Some(conversation) = self.conversations.remove(key) {
            conversation.close();
        }
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        let mut completed: Option<(AgentRunKind, String)> = None;
        if let Some(run) = self.runs.get_mut(&id) {
            if matches!(run.state, AgentRunState::InFlight(_)) {
                if let Ok(WorkerMessage::Completed(result)) = run.receiver.try_recv() {
                    let logged = match &result {
                        Ok(text) => text.clone(),
                        Err(err) => format!("ERROR: {err}"),
                    };
                    completed = Some((run.kind, logged));
                    run.state = match result {
                        Ok(text) => AgentRunState::Success(Some(text)),
                        Err(err) => AgentRunState::Failure(err.to_string()),
                    };
                }
            }
            let state = match &run.state {
                AgentRunState::InFlight(_) => {
                    let pending = run
                        .pending_permission
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .map(|p| p.request.clone());
                    match pending {
                        Some(request) => AgentRunState::NeedsPermission(request),
                        None => AgentRunState::InFlight(
                            run.activity
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone(),
                        ),
                    }
                }
                other => other.clone(),
            };
            if let Some((kind, logged)) = completed {
                self.log_traffic(kind, id, TrafficDirection::Response, &logged);
                if !matches!(state, AgentRunState::InFlight(_)) {
                    self.fleet_run_context.remove(&id);
                }
            }
            return Some(state);
        }
        None
    }

    fn respond_to_permission(&mut self, id: RunId, option_id: &str) -> Result<()> {
        let run = self
            .runs
            .get(&id)
            .context("no run with a pending permission request")?;
        let pending = run
            .pending_permission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .context("run has no pending permission request")?;
        pending
            .reply
            .send(option_id.to_string())
            .map_err(|_| anyhow::anyhow!("agent no longer waiting on this permission request"))
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        if let Some(mut run) = self.runs.remove(&id) {
            run.cancelled.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = run.child.lock() {
                if let Some(mut child) = guard.take() {
                    child.kill_tree();
                }
            }
            if let Some(worker) = run.worker.take() {
                let _ = worker.join();
            }
            self.fleet_run_context.remove(&id);
        }
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        let mut counts = InterviewAgentCounts::default();
        for run in self.runs.values() {
            if !matches!(run.state, AgentRunState::InFlight(_)) {
                continue;
            }
            match run.kind {
                AgentRunKind::QuestionMakerReplenishment => counts.question_maker_in_flight += 1,
                AgentRunKind::AnswerProcessor => counts.answer_active += 1,
                AgentRunKind::FleetAgent => {}
            }
        }
        counts.answer_pool = self
            .conversations
            .values()
            .filter(|c| c.purpose == SessionPurpose::AnswerProcessor)
            .count() as u32;
        if counts.answer_pool > 0 {
            counts.answer_max = 2;
        }
        counts
    }
}

fn auth_method_from_initialize(
    init_result: &Value,
    host: AcpHost,
    agent_bin: &Path,
) -> Option<String> {
    if host == AcpHost::Claude && is_standalone_acp_server(agent_bin) {
        return None;
    }
    let methods = init_result.get("authMethods")?.as_array()?;
    if methods.is_empty() {
        return None;
    }
    let preferred = host.auth_method_id();
    if let Some(found) = methods
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .find(|id| *id == preferred)
    {
        return Some(found.to_string());
    }
    methods
        .first()
        .and_then(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

fn run_acp_authenticate(
    session: &mut AcpClient,
    host: AcpHost,
    agent_bin: &Path,
    init_result: &Value,
) -> Result<()> {
    let Some(method_id) = auth_method_from_initialize(init_result, host, agent_bin) else {
        tracing::debug!(
            event = "agent",
            action = "acp_authenticate_skip",
            host = host.label(),
            "skipping ACP authenticate (adapter uses existing CLI login)"
        );
        return Ok(());
    };
    tracing::debug!(
        event = "agent",
        action = "acp_authenticate",
        host = host.label(),
        method_id,
        "ACP authenticate"
    );
    session.send_request("authenticate", json!({ "methodId": method_id }))?;
    session.await_response(AUTH_TIMEOUT)?;
    Ok(())
}

/// The ACP request that reattaches to an existing session, preferring
/// `session/resume` (no history replay) over `session/load`.
fn resume_method(init_result: &Value) -> Option<&'static str> {
    let capabilities = init_result.get("agentCapabilities")?;
    if capabilities
        .pointer("/sessionCapabilities/resume")
        .is_some_and(Value::is_object)
    {
        return Some("session/resume");
    }
    capabilities
        .get("loadSession")
        .and_then(Value::as_bool)
        .filter(|loads| *loads)
        .map(|_| "session/load")
}

/// Give the agent-side session a human-readable name.
///
/// ACP has no rename request. Claude Code's `/rename` only runs in interactive
/// sessions — headless ones, which the ACP adapter drives, drop it — so for
/// Claude this records the title the way `/rename` does: a `custom-title` entry
/// in the session log, which `claude --resume` and the adapter's session list
/// read. Cursor names sessions itself and exposes no way to override it.
fn name_session(host: AcpHost, session_id: &str, title: &str) {
    if title.trim().is_empty() {
        return;
    }
    match host {
        AcpHost::Claude => {
            let result = claude_config_dir()
                .context("no home directory for Claude Code session logs")
                .and_then(|dir| {
                    append_claude_custom_title(&dir, session_id, title, SESSION_LOG_WAIT)
                });
            match result {
                Ok(()) => tracing::info!(
                    event = "agent",
                    action = "acp_session_named",
                    session_id,
                    title,
                    "named Claude session"
                ),
                Err(err) => tracing::warn!(
                    event = "agent",
                    action = "acp_session_name_failed",
                    session_id,
                    error = %err,
                    "could not name Claude session"
                ),
            }
        }
        AcpHost::Cursor => tracing::debug!(
            event = "agent",
            action = "acp_session_name_skipped",
            session_id,
            "Cursor names its own sessions"
        ),
    }
}

/// [`name_session`] without holding up the turn: Claude Code starts the
/// session log as the prompt arrives, so naming waits on it alongside the turn
/// instead of after it.
fn name_session_in_background(host: AcpHost, session_id: String, title: String) {
    if title.trim().is_empty() {
        return;
    }
    thread::spawn(move || name_session(host, &session_id, &title));
}

use crate::run_state::{claude_config_dir, find_claude_session_log};

fn append_claude_custom_title(
    config_dir: &Path,
    session_id: &str,
    title: &str,
    wait: Duration,
) -> Result<()> {
    // The log is written as the first turn streams; give a just-finished turn
    // a moment to land.
    let deadline = std::time::Instant::now() + wait;
    let path = loop {
        if let Some(path) = find_claude_session_log(config_dir, session_id) {
            break path;
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "session log {session_id}.jsonl not found under {}",
                config_dir.display()
            );
        }
        thread::sleep(Duration::from_millis(200));
    };
    let record = json!({ "type": "custom-title", "customTitle": title, "sessionId": session_id });
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    // One write, so the line cannot interleave with Claude Code's own appends.
    log.write_all(format!("{record}\n").as_bytes())?;
    Ok(())
}

/// Drain the ACP child's stderr into the same traffic log used for its
/// JSON-RPC stdio, so process-level errors (e.g. the ACP bridge's own
/// "Internal error" diagnostics) end up somewhere other than an inherited
/// terminal the user may not be watching.
fn spawn_stderr_logger(
    stderr: std::process::ChildStderr,
    traffic_log: Option<SharedAgentTrafficLog>,
    tag: TrafficTag,
    kind: AgentRunKind,
) {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            match &traffic_log {
                Some(log) => tag.record(
                    log,
                    kind.traffic_category(),
                    TrafficDirection::Response,
                    format!("ACP stderr\n{line}"),
                ),
                None => tracing::warn!(event = "agent", action = "acp_stderr", %line, "ACP stderr"),
            }
        }
    });
}

fn run_acp_session(
    host: AcpHost,
    agent_bin: &Path,
    cwd: &Path,
    model: &str,
    effort: &str,
    prompt: &str,
    child_slot: Arc<Mutex<Option<AgentProcess>>>,
    cancelled: Arc<AtomicBool>,
    activity: Arc<Mutex<Option<String>>>,
    pending_permission: PendingPermissionSlot,
    session_id_out: Arc<Mutex<Option<String>>>,
    traffic_log: Option<SharedAgentTrafficLog>,
    tag: TrafficTag,
    run_id: RunId,
    kind: AgentRunKind,
    extra_write_roots: &[PathBuf],
    session_title: &str,
) -> Result<String> {
    let mut child = spawn_acp_process(host, agent_bin, &[])?;
    let stdin = child.stdin.take().context("agent stdin unavailable")?;
    let stdout = child.stdout.take().context("agent stdout unavailable")?;
    let stderr = child.stderr.take().context("agent stderr unavailable")?;
    spawn_stderr_logger(stderr, traffic_log.clone(), tag.clone(), kind);
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
        replay_transcript: Vec::new(),
        cancelled: cancelled.clone(),
        traffic_log,
        tag,
        run_id,
        kind,
        write_roots: acp_write_roots(cwd, extra_write_roots),
        activity,
        pending_permission,
        context_chars: None,
        reply_parts: None,
    };

    let client_name = host.client_name();

    let result = (|| -> Result<String> {
        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        tracing::debug!(
            event = "agent",
            action = "acp_initialize",
            host = host.label(),
            "ACP initialize"
        );
        session.send_request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": { "name": client_name, "version": "0.1.0" }
            }),
        )?;
        let init_result = session.await_response(AUTH_TIMEOUT)?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        run_acp_authenticate(&mut session, host, agent_bin, &init_result)?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        tracing::debug!(
            event = "agent",
            action = "acp_session_new",
            cwd = %cwd.display(),
            "ACP session/new"
        );
        session.send_request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))?;
        let session_result = session.await_response(AUTH_TIMEOUT)?;
        let session_id = session_result
            .get("sessionId")
            .and_then(Value::as_str)
            .context("session/new missing sessionId")?;

        *session_id_out.lock().unwrap_or_else(|e| e.into_inner()) = Some(session_id.to_string());

        apply_session_config_options(
            &mut session,
            session_id,
            session_result.get("configOptions"),
            model,
            effort,
        )?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        tracing::info!(
            event = "agent",
            action = "acp_prompt",
            session_id,
            prompt_chars = prompt.len(),
            "ACP session/prompt"
        );
        session.send_request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": prompt }]
            }),
        )?;
        name_session_in_background(host, session_id.to_string(), session_title.to_string());
        session.await_response(PROMPT_TIMEOUT)?;
        Ok(session.assistant_text)
    })();

    if let Ok(mut guard) = child_slot.lock() {
        if let Some(mut child) = guard.take() {
            child.kill_tree();
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
    /// Every message chunk seen on this connection, by role, coalesced across
    /// consecutive chunks of the same role. Populated during live turns and
    /// (more importantly) during a `session/resume`/`session/load` replay, so
    /// [`fetch_transcript`] can read back full history without a separate
    /// tracking mechanism.
    replay_transcript: Vec<(&'static str, String)>,
    cancelled: Arc<AtomicBool>,
    traffic_log: Option<SharedAgentTrafficLog>,
    tag: TrafficTag,
    /// The run a permission request is reported against.
    run_id: RunId,
    kind: AgentRunKind,
    write_roots: Vec<PathBuf>,
    /// Short human-readable description of what the agent is doing right now,
    /// shared with the run's `poll_run` caller so a UI can show live status.
    activity: Arc<Mutex<Option<String>>>,
    /// Set while this client is blocked waiting on the user's decision for a
    /// `session/request_permission` call.
    pending_permission: PendingPermissionSlot,
    /// Running count of characters entering the session's context, when the
    /// caller tracks it.
    context_chars: Option<Arc<AtomicU64>>,
    /// The current turn's reply, part by part, when the caller keeps it.
    reply_parts: Option<SharedReplyParts>,
}

impl AcpClient {
    fn count_context(&self, chars: usize) {
        if let Some(counter) = &self.context_chars {
            counter.fetch_add(chars as u64, Ordering::Relaxed);
        }
    }

    fn log_raw(&self, direction: TrafficDirection, content: &str) {
        let Some(log) = &self.traffic_log else {
            return;
        };
        self.tag
            .record(log, self.kind.traffic_category(), direction, content);
    }

    fn update_reply(&self, update: impl FnOnce(&mut Vec<ReplyPart>)) {
        if let Some(parts) = &self.reply_parts {
            update(&mut parts.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }

    /// Forget the reply so far: a new turn starts, or a replay ended.
    fn clear_reply(&mut self) {
        self.assistant_text.clear();
        self.update_reply(Vec::clear);
    }

    fn set_activity(&self, activity: Option<String>) {
        *self.activity.lock().unwrap_or_else(|e| e.into_inner()) = activity;
    }

    fn push_replay_chunk(&mut self, role: &'static str, text: &str) {
        match self.replay_transcript.last_mut() {
            Some((last_role, buf)) if *last_role == role => buf.push_str(text),
            _ => self.replay_transcript.push((role, text.to_string())),
        }
    }

    /// Render every captured chunk (from live turns and any resume/load
    /// replay on this connection) as a plain-text transcript.
    fn replay_transcript_text(&self) -> String {
        self.replay_transcript
            .iter()
            .map(|(role, text)| {
                let label = if *role == "user" { "User" } else { "Assistant" };
                format!("{label}:\n{text}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Publish a permission request for `poll_run` to surface, then block
    /// this thread until [`AgentProvider::respond_to_permission`] answers it
    /// (or the run is cancelled). Runs on the same background thread that
    /// drives the whole turn, so blocking here just pauses that turn.
    fn ask_user_for_permission(&self, title: &str, options: &[Value]) -> Result<String> {
        let options: Vec<PermissionOption> = options
            .iter()
            .filter_map(|option| {
                let id = option.get("optionId").and_then(Value::as_str)?.to_string();
                let label = option
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                Some(PermissionOption { id, label })
            })
            .collect();
        let request = PermissionRequest {
            run: self.run_id,
            title: title.to_string(),
            options,
        };
        let (reply, response_rx) = mpsc::channel();
        *self
            .pending_permission
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(PendingPermission { request, reply });

        let result = loop {
            if self.cancelled.load(Ordering::SeqCst) {
                break Err(anyhow::anyhow!("ACP run cancelled"));
            }
            match response_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(option_id) => break Ok(option_id),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(anyhow::anyhow!("permission response channel dropped"));
                }
            }
        };

        *self
            .pending_permission
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        result
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<i64> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.log_raw(
            TrafficDirection::Request,
            &format!("ACP → {method}\n{message}"),
        );
        writeln!(self.stdin, "{message}")?;
        self.stdin.flush()?;
        Ok(id)
    }

    /// Wait for the response to the last request, applying `idle_timeout` as
    /// an *idle* bound rather than a bound on the whole call: any inbound
    /// notification (a tool call, a permission request, a message chunk — the
    /// agent doing visible work) pushes the deadline back out. A turn with
    /// heavy tool use can run indefinitely as long as it keeps reporting
    /// activity; it only times out once the agent goes silent for the full
    /// `idle_timeout`.
    fn await_response(&mut self, idle_timeout: Duration) -> Result<Value> {
        let mut deadline = std::time::Instant::now() + idle_timeout;
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
                Ok(AcpRequest::Response { _id: _, result }) => {
                    self.log_raw(
                        TrafficDirection::Response,
                        &format!(
                            "ACP ←\n{}",
                            serde_json::to_string_pretty(&result)
                                .unwrap_or_else(|_| result.to_string())
                        ),
                    );
                    return Ok(result);
                }
                Ok(AcpRequest::ResponseError { message, error }) => {
                    self.log_raw(
                        TrafficDirection::Response,
                        &format!(
                            "ACP ← error\n{}",
                            serde_json::to_string_pretty(&error)
                                .unwrap_or_else(|_| error.to_string())
                        ),
                    );
                    let code = error.get("code").and_then(Value::as_i64);
                    let data = error.get("data");
                    match (code, data) {
                        (Some(code), Some(data)) => {
                            bail!("ACP error {code}: {message} ({data})")
                        }
                        (Some(code), None) => bail!("ACP error {code}: {message}"),
                        (None, Some(data)) => bail!("ACP error: {message} ({data})"),
                        (None, None) => bail!("ACP error: {message}"),
                    }
                }
                Ok(AcpRequest::Notification { method, params }) => {
                    self.handle_notification(&method, params)?;
                    deadline = std::time::Instant::now() + idle_timeout;
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
                    let kind = update
                        .get("sessionUpdate")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    if kind == "agent_message_chunk" {
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                        {
                            self.assistant_text.push_str(text);
                            self.update_reply(|parts| reply::push_text(parts, false, text));
                            self.push_replay_chunk("assistant", text);
                        }
                        self.set_activity(Some("Writing reply…".to_string()));
                    } else if kind == "user_message_chunk" {
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                        {
                            self.push_replay_chunk("user", text);
                        }
                    } else if kind == "agent_thought_chunk" {
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                        {
                            self.update_reply(|parts| reply::push_text(parts, true, text));
                        }
                        self.set_activity(Some("Thinking…".to_string()));
                    } else if kind == "tool_call" || kind == "tool_call_update" {
                        let title = update.get("title").and_then(Value::as_str).unwrap_or("");
                        let status = update.get("status").and_then(Value::as_str).unwrap_or("");
                        let call_id = update
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.update_reply(|parts| reply::push_tool(parts, call_id, title, status));
                        // Tool input and output stay in the agent's context.
                        let tool_chars: usize = ["content", "rawInput", "rawOutput"]
                            .iter()
                            .filter_map(|field| update.get(*field))
                            .map(|value| value.to_string().len())
                            .sum();
                        self.count_context(tool_chars);
                        self.set_activity(Some(if title.is_empty() {
                            "Running a tool…".to_string()
                        } else {
                            format!("Running: {title}")
                        }));
                        tracing::debug!(
                            event = "agent",
                            action = "acp_tool",
                            kind,
                            title,
                            status,
                            "ACP tool update"
                        );
                    }
                }
            }
            "session/request_permission" => {
                let response_id = params
                    .get("_response_id")
                    .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|n| n as i64)));
                let options = params
                    .get("options")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let option_id = pick_allow_option_id(&options).unwrap_or("allow-once");
                let tool_title = params
                    .get("toolCall")
                    .and_then(|t| t.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.set_activity(Some(if tool_title.is_empty() {
                    "Requesting permission…".to_string()
                } else {
                    format!("Requesting permission: {tool_title}")
                }));
                let allowed = tool_path_allowed(tool_title, &self.write_roots);
                if allowed {
                    tracing::info!(
                        event = "agent",
                        action = "acp_permission",
                        ?response_id,
                        option_id,
                        tool_title,
                        "auto-approving ACP permission"
                    );
                    if let Some(id) = response_id {
                        respond(
                            &mut self.stdin,
                            id,
                            json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                        )?;
                    } else {
                        tracing::warn!(
                            event = "agent",
                            action = "acp_permission_no_id",
                            "permission request missing response id — cannot approve"
                        );
                    }
                } else {
                    let Some(id) = response_id else {
                        tracing::warn!(
                            event = "agent",
                            action = "acp_permission_no_id",
                            "permission request missing response id — cannot ask the user"
                        );
                        return Ok(());
                    };
                    let chosen = self.ask_user_for_permission(tool_title, &options)?;
                    tracing::info!(
                        event = "agent",
                        action = "acp_permission_answered",
                        response_id = id,
                        option_id = %chosen,
                        tool_title,
                        "user answered ACP permission request"
                    );
                    respond(
                        &mut self.stdin,
                        id,
                        json!({ "outcome": { "outcome": "selected", "optionId": chosen } }),
                    )?;
                }
            }
            "cursor/ask_question" => {
                if let Some(id) = params.get("_response_id").and_then(Value::as_i64) {
                    tracing::debug!(
                        event = "agent",
                        action = "acp_skip_question",
                        id,
                        "skipping cursor/ask_question"
                    );
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
            other => {
                tracing::debug!(
                    event = "agent",
                    action = "acp_notification",
                    method = other,
                    "ACP notification"
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
enum AcpRequest {
    Response { _id: i64, result: Value },
    ResponseError { message: String, error: Value },
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
                _id: id.as_i64().unwrap_or_default(),
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
                error: error.clone(),
            });
            // Also satisfy await_response for request/response pairs.
            let _ = tx.send(AcpRequest::Response {
                _id: id.as_i64().unwrap_or_default(),
                result: json!({}),
            });
            continue;
        }

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let mut params = value.get("params").cloned().unwrap_or(json!({}));
            if let Some(id) = value.get("id") {
                if let Some(obj) = params.as_object_mut() {
                    // Preserve numeric id as i64 when possible (ACP often uses id: 0).
                    let normalized = id
                        .as_i64()
                        .or_else(|| id.as_u64().and_then(|n| i64::try_from(n).ok()))
                        .map(Value::from)
                        .unwrap_or_else(|| id.clone());
                    obj.insert("_response_id".to_string(), normalized);
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

fn acp_write_roots(cwd: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for candidate in extra {
        if let Ok(root) = normalize_absolute(candidate) {
            if !roots.iter().any(|existing| existing == &root) {
                roots.push(root);
            }
        }
    }
    if let Ok(root) = normalize_absolute(cwd) {
        if !roots.iter().any(|existing| existing == &root) {
            roots.push(root);
        }
    }
    roots
}

fn extract_tool_path(tool_title: &str) -> Option<PathBuf> {
    let trimmed = tool_title.trim();
    let rest = trimmed
        .strip_prefix("Write ")
        .or_else(|| trimmed.strip_prefix("Edit "))
        .or_else(|| trimmed.strip_prefix("Delete "))
        .or_else(|| {
            trimmed
                .strip_prefix("Delete `")
                .and_then(|s| s.strip_suffix('`'))
        })
        .or_else(|| {
            trimmed
                .strip_prefix("Edit `")
                .and_then(|s| s.strip_suffix('`'))
        })
        .or_else(|| {
            trimmed
                .strip_prefix("Write `")
                .and_then(|s| s.strip_suffix('`'))
        })?;
    Some(PathBuf::from(rest.trim().trim_matches('`')))
}

fn tool_path_allowed(tool_title: &str, write_roots: &[PathBuf]) -> bool {
    let Some(path) = extract_tool_path(tool_title) else {
        return true;
    };
    write_roots.iter().any(|root| path_is_under(root, &path))
}

fn pick_allow_option_id(options: &[Value]) -> Option<&str> {
    let ids: Vec<&str> = options
        .iter()
        .filter_map(|o| o.get("optionId").and_then(Value::as_str))
        .collect();
    ids.iter()
        .copied()
        .find(|id| *id == "allow-always")
        .or_else(|| ids.iter().copied().find(|id| *id == "allow-once"))
        .or_else(|| {
            ids.iter()
                .copied()
                .find(|id| id.to_ascii_lowercase().contains("allow"))
        })
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

fn pick_effort_option(config_options: Option<&Value>) -> Option<Value> {
    let options = config_options?.as_array()?;
    options
        .iter()
        .find(|o| {
            matches!(
                o.get("category").and_then(Value::as_str),
                Some("effort") | Some("thinking")
            )
        })
        .or_else(|| {
            options.iter().find(|o| {
                o.get("id")
                    .and_then(Value::as_str)
                    .map(|id| {
                        let lower = id.to_ascii_lowercase();
                        lower.contains("effort") || lower.contains("thinking")
                    })
                    .unwrap_or(false)
            })
        })
        .cloned()
}

fn has_config_option_value(option: &Value, value_id: &str) -> bool {
    option
        .get("options")
        .and_then(Value::as_array)
        .is_some_and(|options| {
            options
                .iter()
                .any(|o| o.get("value").and_then(Value::as_str) == Some(value_id))
        })
}

fn apply_session_config_options(
    session: &mut AcpClient,
    session_id: &str,
    config_options: Option<&Value>,
    model: &str,
    effort: &str,
) -> Result<()> {
    if let Some(model_option) = pick_model_option(config_options) {
        let target = if has_config_option_value(&model_option, model) {
            model
        } else {
            model_option
                .get("currentValue")
                .and_then(Value::as_str)
                .unwrap_or(model)
        };
        if model_option.get("currentValue").and_then(Value::as_str) != Some(target) {
            tracing::debug!(
                event = "agent",
                action = "acp_set_model",
                target,
                "ACP session/set_config_option model"
            );
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

    if let Some(effort_value) = effort_for_acp(effort) {
        if let Some(effort_option) = pick_effort_option(config_options) {
            let target = if has_config_option_value(&effort_option, effort_value) {
                effort_value
            } else {
                effort_option
                    .get("currentValue")
                    .and_then(Value::as_str)
                    .unwrap_or(effort_value)
            };
            if effort_option.get("currentValue").and_then(Value::as_str) != Some(target) {
                tracing::debug!(
                    event = "agent",
                    action = "acp_set_effort",
                    target,
                    "ACP session/set_config_option effort"
                );
                session.send_request(
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        "configId": effort_option
                            .get("id")
                            .cloned()
                            .unwrap_or(json!("effort")),
                        "value": target
                    }),
                )?;
                session.await_response(AUTH_TIMEOUT)?;
            }
        }
    }

    Ok(())
}

struct PersistentAcpSession {
    client: AcpClient,
    session_id: String,
    _reader_handle: JoinHandle<()>,
}

impl PersistentAcpSession {
    fn connect(
        host: AcpHost,
        agent_bin: &Path,
        cwd: &Path,
        model: &str,
        effort: &str,
        child_slot: Arc<Mutex<Option<AgentProcess>>>,
        cancelled: Arc<AtomicBool>,
        activity: Arc<Mutex<Option<String>>>,
        pending_permission: PendingPermissionSlot,
        extra_write_roots: &[PathBuf],
        start: &SessionStart,
        traffic_log: Option<SharedAgentTrafficLog>,
        tag: TrafficTag,
        kind: AgentRunKind,
        env: &[(String, String)],
        context_chars: Option<Arc<AtomicU64>>,
        reply_parts: Option<SharedReplyParts>,
    ) -> Result<Self> {
        let mut child = spawn_acp_process(host, agent_bin, env)?;
        let stdin = child.stdin.take().context("agent stdin unavailable")?;
        let stdout = child.stdout.take().context("agent stdout unavailable")?;
        let stderr = child.stderr.take().context("agent stderr unavailable")?;
        spawn_stderr_logger(stderr, traffic_log.clone(), tag.clone(), kind);
        {
            let mut guard = child_slot.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(child);
        }

        let (request_tx, request_rx) = mpsc::channel::<AcpRequest>();
        let reader_handle = thread::spawn(move || read_stdout_lines(stdout, request_tx));

        let mut client = AcpClient {
            stdin,
            next_id: 1,
            request_rx,
            assistant_text: String::new(),
        replay_transcript: Vec::new(),
            cancelled: cancelled.clone(),
            traffic_log,
            tag,
            run_id: RunId::new(),
            kind,
            write_roots: acp_write_roots(cwd, extra_write_roots),
            activity,
            pending_permission,
            context_chars,
            reply_parts,
        };

        let client_name = host.client_name();

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        client.send_request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": { "name": client_name, "version": "0.1.0" }
            }),
        )?;
        let init_result = client.await_response(AUTH_TIMEOUT)?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        run_acp_authenticate(&mut client, host, agent_bin, &init_result)?;

        if cancelled.load(Ordering::SeqCst) {
            bail!("ACP run cancelled");
        }
        let (session_id, session_result) = match start {
            SessionStart::New => {
                client.send_request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))?;
                let result = client.await_response(AUTH_TIMEOUT)?;
                let session_id = result
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .context("session/new missing sessionId")?
                    .to_string();
                (session_id, result)
            }
            SessionStart::Resume(session_id) => {
                let method = resume_method(&init_result).with_context(|| {
                    format!("{} cannot resume session {session_id}", host.label())
                })?;
                tracing::info!(
                    event = "agent",
                    action = "acp_session_resume",
                    method,
                    session_id = %session_id,
                    "resuming ACP session"
                );
                client.send_request(
                    method,
                    json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": [] }),
                )?;
                let result = client.await_response(AUTH_TIMEOUT)?;
                // `session/load` replays the history as message updates; none of
                // it is a reply to anything this process sends.
                client.clear_reply();
                (session_id.clone(), result)
            }
        };

        apply_session_config_options(
            &mut client,
            &session_id,
            session_result.get("configOptions"),
            model,
            effort,
        )?;

        Ok(Self {
            client,
            session_id,
            _reader_handle: reader_handle,
        })
    }

    /// Send one turn made of several text blocks, in order, naming the session
    /// as the turn starts when `name` is given.
    fn prompt_blocks(
        &mut self,
        blocks: &[String],
        run_id: RunId,
        name: Option<(AcpHost, &str)>,
    ) -> Result<String> {
        self.client.clear_reply();
        self.client.run_id = run_id;
        let content: Vec<Value> = blocks
            .iter()
            .map(|text| json!({ "type": "text", "text": text }))
            .collect();
        self.client
            .count_context(blocks.iter().map(String::len).sum());
        self.client.send_request(
            "session/prompt",
            json!({ "sessionId": self.session_id, "prompt": content }),
        )?;
        if let Some((host, title)) = name {
            name_session_in_background(host, self.session_id.clone(), title.to_string());
        }
        self.client.await_response(PROMPT_TIMEOUT)?;
        self.client.count_context(self.client.assistant_text.len());
        Ok(self.client.assistant_text.clone())
    }

    fn shutdown(self, child_slot: Arc<Mutex<Option<AgentProcess>>>) {
        if let Ok(mut guard) = child_slot.lock() {
            if let Some(mut child) = guard.take() {
                child.kill_tree();
            }
        }
        let _ = self._reader_handle.join();
    }
}

/// Fetch a resumable session's full transcript by connecting fresh and
/// issuing `session/resume`/`session/load` — no prompt is sent, and the
/// process is torn down immediately after. Used to populate the one-time
/// cached transcript for a run that doesn't have one yet (e.g. reopening a
/// `Done` chat window, or reconciling on force-exit).
pub(crate) fn fetch_transcript(
    host: AcpHost,
    agent_bin: &Path,
    cwd: &Path,
    session_id: &str,
    traffic_log: Option<SharedAgentTrafficLog>,
) -> Result<String> {
    let child_slot: Arc<Mutex<Option<AgentProcess>>> = Arc::new(Mutex::new(None));
    let cancelled = Arc::new(AtomicBool::new(false));
    let activity = Arc::new(Mutex::new(None));
    let pending_permission: PendingPermissionSlot = Arc::new(Mutex::new(None));
    let session = PersistentAcpSession::connect(
        host,
        agent_bin,
        cwd,
        "",
        "",
        child_slot.clone(),
        cancelled,
        activity,
        pending_permission,
        &[],
        &SessionStart::Resume(session_id.to_string()),
        traffic_log,
        TrafficTag::new(
            format!("transcript-{session_id}"),
            &format!("Transcript fetch · {session_id}"),
            "",
        ),
        AgentRunKind::FleetAgent,
        &[],
        None,
        None,
    )?;
    let text = session.client.replay_transcript_text();
    session.shutdown(child_slot);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn skips_authenticate_for_claude_code_acp_adapter() {
        let init = json!({
            "authMethods": [{ "id": "claude_login", "name": "Claude login" }]
        });
        let adapter = Path::new("/usr/local/bin/claude-code-acp");
        assert!(auth_method_from_initialize(&init, AcpHost::Claude, adapter).is_none());

        let cursor_init = json!({
            "authMethods": [{ "id": "cursor_login", "name": "Cursor login" }]
        });
        assert_eq!(
            auth_method_from_initialize(
                &cursor_init,
                AcpHost::Cursor,
                Path::new("/usr/local/bin/agent")
            )
            .as_deref(),
            Some("cursor_login")
        );
    }

    #[test]
    fn resume_prefers_session_resume_over_load() {
        let claude = json!({
            "agentCapabilities": {
                "loadSession": true,
                "sessionCapabilities": { "resume": {}, "list": {} }
            }
        });
        assert_eq!(resume_method(&claude), Some("session/resume"));
        let cursor = json!({
            "agentCapabilities": { "loadSession": true, "sessionCapabilities": { "list": {} } }
        });
        assert_eq!(resume_method(&cursor), Some("session/load"));
        let neither = json!({ "agentCapabilities": { "loadSession": false } });
        assert_eq!(resume_method(&neither), None);
    }

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn claude_session_title_is_appended_to_the_session_log() {
        let config_dir = scratch_dir("claude-title");
        let project = config_dir.join("projects").join("C--work-repo");
        std::fs::create_dir_all(&project).unwrap();
        let log = project.join("abc-123.jsonl");
        std::fs::write(&log, concat!(r#"{"type":"user"}"#, "\n")).unwrap();

        append_claude_custom_title(&config_dir, "abc-123", "Obligations · Demo", Duration::ZERO)
            .unwrap();

        let records: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[1],
            json!({ "type": "custom-title", "customTitle": "Obligations · Demo", "sessionId": "abc-123" })
        );
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn naming_a_session_without_a_log_fails() {
        let config_dir = scratch_dir("claude-title-missing");
        assert!(append_claude_custom_title(&config_dir, "nope", "Title", Duration::ZERO).is_err());
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    /// Scripted ACP agent: answers each prompt with the text blocks it got, and
    /// records every request next to itself.
    const FAKE_ACP_AGENT: &str = r##"
import json, os, sys

LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "requests.jsonl")

def send(message):
    print(json.dumps(message), flush=True)

while True:
    line = sys.stdin.readline()
    if not line:
        break
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method = message.get("method")
    if method is None or "id" not in message:
        continue
    params = message.get("params", {})
    with open(LOG, "a") as log:
        print(json.dumps({"method": method, "params": params}), file=log)
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}}, "authMethods": []}
    elif method == "session/new":
        result = {"sessionId": "fake-session-1"}
    elif method == "session/prompt":
        texts = [block.get("text", "") for block in params.get("prompt", [])]
        update = {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "reply to: " + " | ".join(texts)}}
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": params.get("sessionId"), "update": update}})
        result = {"stopReason": "end_turn"}
    else:
        result = {}
    send({"jsonrpc": "2.0", "id": message["id"], "result": result})
"##;

    fn python_available() -> bool {
        Command::new("python")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn wait_for_reply(provider: &mut CursorAcpProvider, id: RunId) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            match provider.poll_run(id) {
                Some(AgentRunState::Success(text)) => return text.unwrap_or_default(),
                Some(AgentRunState::Failure(err)) => panic!("turn failed: {err}"),
                _ => thread::sleep(Duration::from_millis(20)),
            }
        }
        panic!("timed out waiting for run {id:?}");
    }

    #[test]
    fn conversation_sends_opening_once_and_resumes_after_close() {
        use crate::agent_launch::AgentLaunchOptions;
        use crate::platform::AgentPlatform;
        use crate::provider::SessionOpening;

        if !python_available() {
            eprintln!("skipping: python is not available to run the fake ACP agent");
            return;
        }
        let dir = scratch_dir("fake-acp");
        // The name marks it as a standalone adapter: no `acp` subcommand, no login.
        let agent_bin = dir.join("fake-claude-code-acp.py");
        std::fs::write(&agent_bin, FAKE_ACP_AGENT).unwrap();
        let traffic = crate::agent_traffic::shared_log();
        let mut provider = CursorAcpProvider::with_agent_bin(AcpHost::Claude, agent_bin)
            .with_traffic_log(traffic.clone());
        let turn =
            |opening: Option<SessionOpening>, resume: Option<String>, message: &str| SessionTurn {
                key: "run-1".into(),
                owner_id: "config".into(),
                // An empty title skips naming, which would touch the real ~/.claude.
                title: String::new(),
                cwd: dir.clone(),
                options: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
                resume_session_id: resume,
                opening,
                message: message.into(),
                purpose: crate::provider::SessionPurpose::Chat,
                env: Vec::new(),
            };
        let opening = SessionOpening {
            context: Some("CONTEXT".into()),
        };

        let first = provider
            .send_session_turn(turn(Some(opening), None, "first"))
            .unwrap();
        assert_eq!(
            wait_for_reply(&mut provider, first.id),
            "reply to: CONTEXT | first"
        );
        let second = provider
            .send_session_turn(turn(None, None, "second"))
            .unwrap();
        assert_eq!(wait_for_reply(&mut provider, second.id), "reply to: second");
        let session_id = provider.session_id("run-1").expect("session id");
        assert_eq!(session_id, "fake-session-1");

        provider.close_session("run-1");
        let third = provider
            .send_session_turn(turn(None, Some(session_id.clone()), "third"))
            .unwrap();
        assert_eq!(wait_for_reply(&mut provider, third.id), "reply to: third");

        let requests: Vec<Value> = std::fs::read_to_string(dir.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let methods: Vec<&str> = requests
            .iter()
            .filter_map(|request| request.get("method").and_then(Value::as_str))
            .collect();
        assert_eq!(
            methods,
            [
                "initialize",
                "session/new",
                "session/prompt",
                "session/prompt",
                "initialize",
                "session/resume",
                "session/prompt",
            ]
        );
        assert_eq!(
            requests[5].pointer("/params/sessionId"),
            Some(&json!(session_id))
        );

        // Every turn, process, and protocol message of the session is one
        // transcript, keyed by the session.
        drop(provider);
        let summaries = traffic.lock().unwrap().agent_summaries();
        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["run-1"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pick_effort_option_matches_category_and_id() {
        let by_category = json!([
            { "id": "model", "category": "model", "currentValue": "auto" },
            {
                "id": "reasoning",
                "category": "effort",
                "currentValue": "medium",
                "options": [{ "value": "low" }, { "value": "high" }]
            }
        ]);
        let effort = pick_effort_option(Some(&by_category)).expect("effort option");
        assert_eq!(
            effort.get("category").and_then(Value::as_str),
            Some("effort")
        );

        let by_id = json!([{ "id": "thinking-level", "currentValue": "low" }]);
        let thinking = pick_effort_option(Some(&by_id)).expect("thinking id");
        assert_eq!(
            thinking.get("id").and_then(Value::as_str),
            Some("thinking-level")
        );

        assert!(pick_effort_option(Some(&json!([]))).is_none());
    }
}
