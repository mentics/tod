use super::acp_host::{AcpHost, is_standalone_acp_server, spawn_acp_process};
use super::answer_pool::{AnswerProcessorPoolManager, AnswerSubmitAssignment};
use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState, DeepDiveContext, RunId,
};
use super::question_maker_pool::{QuestionMakerPoolManager, QuestionMakerSubmitAssignment};
use crate::interview::paths::TodPaths;
use crate::interview::settings::{AnswerProcessorSettings, QuestionMakerSettings};
use crate::process_bundle::InterviewAgentPrompt;
use crate::process_bundle::{TodInstallPaths, build_deep_dive_prompt, load_deep_dive_role_doc};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tod_store::agent_traffic::{
    AgentCategory, InterviewAgentCounts, SharedAgentTrafficLog, TrafficDirection,
};
use tod_store::fleet::paths::normalize_absolute;
use tod_store::path_is_under;
use tod_store::{AgentLaunchOptions, effort_for_acp};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

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

#[derive(Debug)]
enum SlotCommand {
    Prompt { run_id: RunId, prompt: String },
    Shutdown,
}

#[derive(Debug)]
struct SlotCompletion {
    agent_config_id: String,
    cwd: PathBuf,
    slot_id: u32,
    run_id: RunId,
    result: Result<String, String>,
}

#[derive(Clone)]
struct PoolRunContext {
    agent_config_id: String,
    _cwd: PathBuf,
    model: String,
    effort: String,
}

struct AcpLiveSlot {
    cmd_tx: Sender<SlotCommand>,
    worker: JoinHandle<()>,
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
}

/// ACP agent backend (Cursor, Claude, …).
pub struct CursorAcpProvider {
    host: AcpHost,
    agent_bin: PathBuf,
    deep_dive_role: String,
    runs: HashMap<RunId, ActiveRun>,
    answer_pool: AnswerProcessorPoolManager,
    answer_run_context: HashMap<RunId, PoolRunContext>,
    answer_slot_workers: HashMap<String, HashMap<u32, AcpLiveSlot>>,
    answer_slot_completions: Receiver<SlotCompletion>,
    answer_slot_completion_tx: Sender<SlotCompletion>,
    question_maker_pool: QuestionMakerPoolManager,
    question_maker_run_context: HashMap<RunId, PoolRunContext>,
    question_maker_slot_workers: HashMap<String, HashMap<u32, AcpLiveSlot>>,
    question_maker_slot_completions: Receiver<SlotCompletion>,
    question_maker_slot_completion_tx: Sender<SlotCompletion>,
    fleet_run_context: HashMap<RunId, String>,
    traffic_log: Option<SharedAgentTrafficLog>,
}

impl CursorAcpProvider {
    fn load_deep_dive_role() -> String {
        TodInstallPaths::discover()
            .ok()
            .and_then(|install| load_deep_dive_role_doc(&install).ok())
            .unwrap_or_else(|| "# Deep dive\n\nFree-form chat.".into())
    }

    pub fn for_host(host: AcpHost) -> Result<Self> {
        let (answer_slot_completion_tx, answer_slot_completions) = mpsc::channel();
        let (question_maker_slot_completion_tx, question_maker_slot_completions) = mpsc::channel();
        Ok(Self {
            host,
            agent_bin: host.resolve_bin()?,
            deep_dive_role: Self::load_deep_dive_role(),
            runs: HashMap::new(),
            answer_pool: AnswerProcessorPoolManager::default(),
            answer_run_context: HashMap::new(),
            answer_slot_workers: HashMap::new(),
            answer_slot_completions,
            answer_slot_completion_tx,
            question_maker_pool: QuestionMakerPoolManager::default(),
            question_maker_run_context: HashMap::new(),
            question_maker_slot_workers: HashMap::new(),
            question_maker_slot_completions,
            question_maker_slot_completion_tx,
            fleet_run_context: HashMap::new(),
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

    fn kind_category(kind: AgentRunKind) -> AgentCategory {
        match kind {
            AgentRunKind::QuestionMakerReplenishment => AgentCategory::QuestionMaker,
            AgentRunKind::AnswerProcessor => AgentCategory::AnswerProcessor,
            AgentRunKind::DeepDiveChat => AgentCategory::DeepDive,
            AgentRunKind::FleetAgent => AgentCategory::Fleet,
        }
    }

    fn kind_label(kind: AgentRunKind) -> &'static str {
        match kind {
            AgentRunKind::QuestionMakerReplenishment => "question-maker",
            AgentRunKind::AnswerProcessor => "answer-processor",
            AgentRunKind::DeepDiveChat => "deep-dive",
            AgentRunKind::FleetAgent => "fleet-agent",
        }
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
        let agent_id = self
            .fleet_run_context
            .get(&run_id)
            .cloned()
            .unwrap_or_else(|| Self::run_id_string(run_id));
        log.lock().expect("traffic log mutex").record(
            Self::kind_category(kind),
            agent_id,
            Self::kind_label(kind),
            direction,
            content,
        );
    }

    pub fn with_agent_bin(host: AcpHost, agent_bin: PathBuf) -> Self {
        let (answer_slot_completion_tx, answer_slot_completions) = mpsc::channel();
        let (question_maker_slot_completion_tx, question_maker_slot_completions) = mpsc::channel();
        Self {
            host,
            agent_bin,
            deep_dive_role: Self::load_deep_dive_role(),
            runs: HashMap::new(),
            answer_pool: AnswerProcessorPoolManager::default(),
            answer_run_context: HashMap::new(),
            answer_slot_workers: HashMap::new(),
            answer_slot_completions,
            answer_slot_completion_tx,
            question_maker_pool: QuestionMakerPoolManager::default(),
            question_maker_run_context: HashMap::new(),
            question_maker_slot_workers: HashMap::new(),
            question_maker_slot_completions,
            question_maker_slot_completion_tx,
            fleet_run_context: HashMap::new(),
            traffic_log: None,
        }
    }

    fn process_answer_slot_completions(&mut self) {
        while let Ok(completion) = self.answer_slot_completions.try_recv() {
            let SlotCompletion {
                agent_config_id,
                cwd,
                slot_id,
                run_id,
                result,
            } = completion;
            let outcome = self
                .answer_pool
                .complete_run(&agent_config_id, slot_id, run_id, result);
            if let Some(recycled) = outcome.recycled_slot_id {
                self.shutdown_answer_slot(&agent_config_id, recycled);
            }
            for (sid, rid, prompt) in outcome.dispatched {
                let pool_opts = self
                    .answer_run_context
                    .get(&run_id)
                    .map(|ctx| (ctx.model.clone(), ctx.effort.clone()))
                    .unwrap_or_else(|| ("auto".into(), "auto".into()));
                self.answer_run_context.insert(
                    rid,
                    PoolRunContext {
                        agent_config_id: agent_config_id.clone(),
                        _cwd: cwd.clone(),
                        model: pool_opts.0.clone(),
                        effort: pool_opts.1.clone(),
                    },
                );
                self.dispatch_answer_prompt(
                    &agent_config_id,
                    &cwd,
                    sid,
                    rid,
                    prompt,
                    &pool_opts.0,
                    &pool_opts.1,
                );
            }
        }
    }

    fn process_question_maker_slot_completions(&mut self) {
        while let Ok(completion) = self.question_maker_slot_completions.try_recv() {
            let SlotCompletion {
                agent_config_id,
                cwd,
                slot_id,
                run_id,
                result,
            } = completion;
            let logged = match &result {
                Ok(text) => text.clone(),
                Err(err) => format!("ERROR: {err}"),
            };
            self.log_traffic(
                AgentRunKind::QuestionMakerReplenishment,
                run_id,
                TrafficDirection::Response,
                &logged,
            );
            let outcome =
                self.question_maker_pool
                    .complete_run(&agent_config_id, slot_id, run_id, result);
            if let Some(recycled) = outcome.recycled_slot_id {
                self.shutdown_question_maker_slot(&agent_config_id, recycled);
            }
            for (sid, rid, prompt) in outcome.dispatched {
                let pool_opts = self
                    .question_maker_run_context
                    .get(&run_id)
                    .map(|ctx| (ctx.model.clone(), ctx.effort.clone()))
                    .unwrap_or_else(|| ("auto".into(), "auto".into()));
                self.question_maker_run_context.insert(
                    rid,
                    PoolRunContext {
                        agent_config_id: agent_config_id.clone(),
                        _cwd: cwd.clone(),
                        model: pool_opts.0.clone(),
                        effort: pool_opts.1.clone(),
                    },
                );
                self.dispatch_question_maker_prompt(
                    &agent_config_id,
                    &cwd,
                    sid,
                    rid,
                    prompt,
                    &pool_opts.0,
                    &pool_opts.1,
                );
            }
        }
    }

    fn shutdown_answer_slot(&mut self, agent_config_id: &str, slot_id: u32) {
        Self::shutdown_slot_in_map(&mut self.answer_slot_workers, agent_config_id, slot_id);
    }

    fn shutdown_question_maker_slot(&mut self, agent_config_id: &str, slot_id: u32) {
        Self::shutdown_slot_in_map(
            &mut self.question_maker_slot_workers,
            agent_config_id,
            slot_id,
        );
    }

    fn shutdown_slot_in_map(
        slot_workers: &mut HashMap<String, HashMap<u32, AcpLiveSlot>>,
        agent_config_id: &str,
        slot_id: u32,
    ) {
        let Some(slots) = slot_workers.get_mut(agent_config_id) else {
            return;
        };
        if let Some(slot) = slots.remove(&slot_id) {
            slot.cancelled.store(true, Ordering::SeqCst);
            let _ = slot.cmd_tx.send(SlotCommand::Shutdown);
            if let Ok(mut guard) = slot.child.lock() {
                if let Some(mut child) = guard.take() {
                    kill_child_tree(&mut child);
                }
            }
            let _ = slot.worker.join();
        }
        if slots.is_empty() {
            slot_workers.remove(agent_config_id);
        }
    }

    fn ensure_answer_slot_worker(
        &mut self,
        agent_config_id: &str,
        cwd: &Path,
        slot_id: u32,
        model: &str,
        effort: &str,
    ) -> Result<()> {
        Self::ensure_slot_worker(
            agent_config_id,
            cwd,
            slot_id,
            self.host,
            &self.agent_bin,
            model,
            effort,
            &mut self.answer_slot_workers,
            self.answer_slot_completion_tx.clone(),
        )
    }

    fn ensure_question_maker_slot_worker(
        &mut self,
        agent_config_id: &str,
        cwd: &Path,
        slot_id: u32,
        model: &str,
        effort: &str,
    ) -> Result<()> {
        Self::ensure_slot_worker(
            agent_config_id,
            cwd,
            slot_id,
            self.host,
            &self.agent_bin,
            model,
            effort,
            &mut self.question_maker_slot_workers,
            self.question_maker_slot_completion_tx.clone(),
        )
    }

    fn ensure_slot_worker(
        agent_config_id: &str,
        cwd: &Path,
        slot_id: u32,
        host: AcpHost,
        agent_bin: &Path,
        model: &str,
        effort: &str,
        slot_workers: &mut HashMap<String, HashMap<u32, AcpLiveSlot>>,
        done_tx: Sender<SlotCompletion>,
    ) -> Result<()> {
        if slot_workers
            .get(agent_config_id)
            .is_some_and(|m| m.contains_key(&slot_id))
        {
            return Ok(());
        }

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let agent_bin = agent_bin.to_path_buf();
        let model = model.to_string();
        let effort = effort.to_string();
        let cwd_buf = cwd.to_path_buf();
        let agent_id = agent_config_id.to_string();
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let child_for_worker = child_slot.clone();
        let cancelled_for_worker = cancelled.clone();

        let worker = thread::spawn(move || {
            run_acp_pool_slot(
                host,
                &agent_bin,
                &agent_id,
                &cwd_buf,
                &model,
                &effort,
                slot_id,
                cmd_rx,
                done_tx,
                child_for_worker,
                cancelled_for_worker,
            );
        });

        slot_workers
            .entry(agent_config_id.to_string())
            .or_default()
            .insert(
                slot_id,
                AcpLiveSlot {
                    cmd_tx,
                    worker,
                    child: child_slot,
                    cancelled,
                },
            );
        Ok(())
    }

    fn dispatch_answer_prompt(
        &mut self,
        agent_config_id: &str,
        cwd: &Path,
        slot_id: u32,
        run_id: RunId,
        prompt: String,
        model: &str,
        effort: &str,
    ) {
        if self
            .ensure_answer_slot_worker(agent_config_id, cwd, slot_id, model, effort)
            .is_err()
        {
            let outcome = self.answer_pool.complete_run(
                agent_config_id,
                slot_id,
                run_id,
                Err("failed to start ACP pool slot".into()),
            );
            if let Some(recycled) = outcome.recycled_slot_id {
                self.shutdown_answer_slot(agent_config_id, recycled);
            }
            for (sid, rid, p) in outcome.dispatched {
                self.answer_run_context.insert(
                    rid,
                    PoolRunContext {
                        agent_config_id: agent_config_id.to_string(),
                        _cwd: cwd.to_path_buf(),
                        model: model.to_string(),
                        effort: effort.to_string(),
                    },
                );
                self.dispatch_answer_prompt(agent_config_id, cwd, sid, rid, p, model, effort);
            }
            return;
        }
        if let Some(slot) = self
            .answer_slot_workers
            .get(agent_config_id)
            .and_then(|m| m.get(&slot_id))
        {
            let _ = slot.cmd_tx.send(SlotCommand::Prompt { run_id, prompt });
        }
    }

    fn dispatch_question_maker_prompt(
        &mut self,
        agent_config_id: &str,
        cwd: &Path,
        slot_id: u32,
        run_id: RunId,
        prompt: String,
        model: &str,
        effort: &str,
    ) {
        self.log_traffic(
            AgentRunKind::QuestionMakerReplenishment,
            run_id,
            TrafficDirection::Request,
            &prompt,
        );
        if self
            .ensure_question_maker_slot_worker(agent_config_id, cwd, slot_id, model, effort)
            .is_err()
        {
            let outcome = self.question_maker_pool.complete_run(
                agent_config_id,
                slot_id,
                run_id,
                Err("failed to start ACP pool slot".into()),
            );
            if let Some(recycled) = outcome.recycled_slot_id {
                self.shutdown_question_maker_slot(agent_config_id, recycled);
            }
            for (sid, rid, p) in outcome.dispatched {
                self.question_maker_run_context.insert(
                    rid,
                    PoolRunContext {
                        agent_config_id: agent_config_id.to_string(),
                        _cwd: cwd.to_path_buf(),
                        model: model.to_string(),
                        effort: effort.to_string(),
                    },
                );
                self.dispatch_question_maker_prompt(
                    agent_config_id,
                    cwd,
                    sid,
                    rid,
                    p,
                    model,
                    effort,
                );
            }
            return;
        }
        if let Some(slot) = self
            .question_maker_slot_workers
            .get(agent_config_id)
            .and_then(|m| m.get(&slot_id))
        {
            let _ = slot.cmd_tx.send(SlotCommand::Prompt { run_id, prompt });
        }
    }

    fn spawn_run(
        &mut self,
        kind: AgentRunKind,
        cwd: PathBuf,
        prompt: String,
        model: String,
        effort: String,
    ) -> Result<AgentRunHandle> {
        let id = RunId::new();
        let (tx, rx) = mpsc::channel();
        let agent_bin = self.agent_bin.clone();
        let host = self.host;
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let child_for_worker = child_slot.clone();
        let cancelled_for_worker = cancelled.clone();

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

        self.log_traffic(kind, id, TrafficDirection::Request, &prompt);

        let traffic_log = self.traffic_log.clone();
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
                traffic_log,
                id,
                kind,
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
                state: AgentRunState::InFlight,
                child: child_slot,
                cancelled,
                worker: Some(worker),
                receiver: rx,
            },
        );

        Ok(AgentRunHandle { id })
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
    fn start_question_maker_replenishment(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: InterviewAgentPrompt,
        pool: &QuestionMakerSettings,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        let (assignment, run_id) = self
            .question_maker_pool
            .submit(agent_config_id.to_string(), pool.clone(), prompt)
            .map_err(|e| anyhow::anyhow!(e))?;
        self.question_maker_run_context.insert(
            run_id,
            PoolRunContext {
                agent_config_id: agent_config_id.to_string(),
                _cwd: cwd.clone(),
                model: options.model.clone(),
                effort: options.effort.clone(),
            },
        );
        match assignment {
            QuestionMakerSubmitAssignment::Dispatch { slot_id, prompt } => {
                self.dispatch_question_maker_prompt(
                    agent_config_id,
                    &cwd,
                    slot_id,
                    run_id,
                    prompt,
                    &options.model,
                    &options.effort,
                );
            }
            QuestionMakerSubmitAssignment::Queued { .. } => {}
        }
        Ok(AgentRunHandle { id: run_id })
    }

    fn start_answer_processor(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: InterviewAgentPrompt,
        pool: &AnswerProcessorSettings,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        let (assignment, run_id) = self
            .answer_pool
            .submit(agent_config_id.to_string(), pool.clone(), prompt.clone())
            .map_err(|e| anyhow::anyhow!(e))?;
        self.answer_run_context.insert(
            run_id,
            PoolRunContext {
                agent_config_id: agent_config_id.to_string(),
                _cwd: cwd.clone(),
                model: options.model.clone(),
                effort: options.effort.clone(),
            },
        );
        match assignment {
            AnswerSubmitAssignment::Dispatch { slot_id, prompt } => {
                self.dispatch_answer_prompt(
                    agent_config_id,
                    &cwd,
                    slot_id,
                    run_id,
                    prompt,
                    &options.model,
                    &options.effort,
                );
            }
            AnswerSubmitAssignment::Queued { .. } => {}
        }
        Ok(AgentRunHandle { id: run_id })
    }

    fn start_deep_dive_chat(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        context: DeepDiveContext,
        initial_message: Option<String>,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        let prompt =
            build_deep_dive_prompt(&self.deep_dive_role, &context, initial_message.as_deref());
        let _ = agent_config_id;
        self.spawn_run(
            AgentRunKind::DeepDiveChat,
            cwd,
            prompt,
            options.model,
            options.effort,
        )
    }

    fn start_fleet_agent(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        let handle = self.spawn_run(
            AgentRunKind::FleetAgent,
            cwd,
            prompt,
            options.model,
            options.effort,
        )?;
        self.fleet_run_context
            .insert(handle.id, agent_config_id.to_string());
        Ok(handle)
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.process_answer_slot_completions();
        self.process_question_maker_slot_completions();

        if let Some(ctx) = self.question_maker_run_context.get(&id).cloned() {
            if let Some(state) = self.question_maker_pool.poll_run(&ctx.agent_config_id, id) {
                if !matches!(state, AgentRunState::InFlight) {
                    self.question_maker_run_context.remove(&id);
                }
                return Some(state);
            }
        }

        if let Some(ctx) = self.answer_run_context.get(&id).cloned() {
            if let Some(state) = self.answer_pool.poll_run(&ctx.agent_config_id, id) {
                if !matches!(state, AgentRunState::InFlight) {
                    self.answer_run_context.remove(&id);
                }
                return Some(state);
            }
        }

        let mut completed: Option<(AgentRunKind, String)> = None;
        if let Some(run) = self.runs.get_mut(&id) {
            if matches!(run.state, AgentRunState::InFlight) {
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
            let state = run.state.clone();
            if let Some((kind, logged)) = completed {
                self.log_traffic(kind, id, TrafficDirection::Response, &logged);
                if !matches!(state, AgentRunState::InFlight) {
                    self.fleet_run_context.remove(&id);
                }
            }
            return Some(state);
        }
        None
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        if let Some(ctx) = self.question_maker_run_context.remove(&id) {
            self.question_maker_pool
                .cancel_run(&ctx.agent_config_id, id);
            return Ok(());
        }
        if let Some(ctx) = self.answer_run_context.remove(&id) {
            self.answer_pool.cancel_run(&ctx.agent_config_id, id);
            return Ok(());
        }
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
            self.fleet_run_context.remove(&id);
        }
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        let mut counts = InterviewAgentCounts::default();
        for run in self.runs.values() {
            if !matches!(run.state, AgentRunState::InFlight) {
                continue;
            }
            match run.kind {
                AgentRunKind::QuestionMakerReplenishment => {}
                AgentRunKind::AnswerProcessor => {}
                AgentRunKind::DeepDiveChat => counts.deep_dive_in_flight += 1,
                AgentRunKind::FleetAgent => {}
            }
        }
        counts.question_maker_in_flight = self.question_maker_pool.in_flight_count();
        let pool_stats = self.answer_pool.global_stats();
        counts.answer_active = pool_stats.active;
        counts.answer_pool = pool_stats.in_pool;
        counts.answer_max = pool_stats.max;
        counts
    }
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

fn run_acp_session(
    host: AcpHost,
    agent_bin: &Path,
    cwd: &Path,
    model: &str,
    effort: &str,
    prompt: &str,
    child_slot: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
    traffic_log: Option<SharedAgentTrafficLog>,
    run_id: RunId,
    kind: AgentRunKind,
) -> Result<String> {
    let mut child = spawn_acp_process(host, agent_bin)?;
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
        traffic_log,
        run_id,
        kind,
        write_roots: acp_write_roots(cwd),
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
    traffic_log: Option<SharedAgentTrafficLog>,
    run_id: RunId,
    kind: AgentRunKind,
    write_roots: Vec<PathBuf>,
}

impl AcpClient {
    fn log_raw(&self, direction: TrafficDirection, content: &str) {
        let Some(log) = &self.traffic_log else {
            return;
        };
        log.lock().expect("traffic log mutex").record(
            CursorAcpProvider::kind_category(self.kind),
            CursorAcpProvider::run_id_string(self.run_id),
            format!("{} · acp", CursorAcpProvider::kind_label(self.kind)),
            direction,
            content,
        );
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
                        }
                    } else if kind == "tool_call" || kind == "tool_call_update" {
                        let title = update.get("title").and_then(Value::as_str).unwrap_or("");
                        let status = update.get("status").and_then(Value::as_str).unwrap_or("");
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
                    let deny_id = pick_deny_option_id(&options).unwrap_or("deny-once");
                    tracing::warn!(
                        event = "agent",
                        action = "acp_permission_denied",
                        ?response_id,
                        deny_id,
                        tool_title,
                        "denying ACP permission outside allowed write roots"
                    );
                    if let Some(id) = response_id {
                        respond(
                            &mut self.stdin,
                            id,
                            json!({ "outcome": { "outcome": "selected", "optionId": deny_id } }),
                        )?;
                    }
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

fn acp_write_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(paths) = TodPaths::discover() {
        if let Ok(root) = normalize_absolute(paths.data_root()) {
            roots.push(root);
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

fn pick_deny_option_id(options: &[Value]) -> Option<&str> {
    let ids: Vec<&str> = options
        .iter()
        .filter_map(|o| o.get("optionId").and_then(Value::as_str))
        .collect();
    ids.iter()
        .copied()
        .find(|id| *id == "deny-once")
        .or_else(|| {
            ids.iter()
                .copied()
                .find(|id| id.to_ascii_lowercase().contains("deny"))
        })
        .or_else(|| {
            ids.iter()
                .copied()
                .find(|id| id.to_ascii_lowercase().contains("reject"))
        })
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
        child_slot: Arc<Mutex<Option<Child>>>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let mut child = spawn_acp_process(host, agent_bin)?;
        let stdin = child.stdin.take().context("agent stdin unavailable")?;
        let stdout = child.stdout.take().context("agent stdout unavailable")?;
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
            cancelled: cancelled.clone(),
            traffic_log: None,
            run_id: RunId::new(),
            kind: AgentRunKind::AnswerProcessor,
            write_roots: acp_write_roots(cwd),
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
        client.send_request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))?;
        let session_result = client.await_response(AUTH_TIMEOUT)?;
        let session_id = session_result
            .get("sessionId")
            .and_then(Value::as_str)
            .context("session/new missing sessionId")?
            .to_string();

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

    fn prompt(&mut self, prompt: &str) -> Result<String> {
        self.client.assistant_text.clear();
        self.client.send_request(
            "session/prompt",
            json!({
                "sessionId": self.session_id,
                "prompt": [{ "type": "text", "text": prompt }]
            }),
        )?;
        self.client.await_response(PROMPT_TIMEOUT)?;
        Ok(self.client.assistant_text.clone())
    }

    fn shutdown(self, child_slot: Arc<Mutex<Option<Child>>>) {
        if let Ok(mut guard) = child_slot.lock() {
            if let Some(mut child) = guard.take() {
                kill_child_tree(&mut child);
            }
        }
        let _ = self._reader_handle.join();
    }
}

fn drain_slot_prompts_on_connect_failure(
    cmd_rx: &Receiver<SlotCommand>,
    done_tx: &Sender<SlotCompletion>,
    agent_config_id: &str,
    cwd: &Path,
    slot_id: u32,
    err_msg: &str,
) {
    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(SlotCommand::Prompt { run_id, .. }) => {
                let _ = done_tx.send(SlotCompletion {
                    agent_config_id: agent_config_id.to_string(),
                    cwd: cwd.to_path_buf(),
                    slot_id,
                    run_id,
                    result: Err(err_msg.to_string()),
                });
            }
            Ok(SlotCommand::Shutdown) | Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn run_acp_pool_slot(
    host: AcpHost,
    agent_bin: &Path,
    agent_config_id: &str,
    cwd: &Path,
    model: &str,
    effort: &str,
    slot_id: u32,
    cmd_rx: Receiver<SlotCommand>,
    done_tx: Sender<SlotCompletion>,
    child_slot: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
) {
    let cwd_buf = cwd.to_path_buf();
    let agent_config_id = agent_config_id.to_string();
    let mut session = match PersistentAcpSession::connect(
        host,
        agent_bin,
        cwd,
        model,
        effort,
        child_slot.clone(),
        cancelled.clone(),
    ) {
        Ok(session) => session,
        Err(err) => {
            let err_msg = err.to_string();
            tracing::error!(
                event = "agent",
                action = "acp_pool_connect_failed",
                slot_id,
                error = %err_msg,
                "failed to open ACP pool slot"
            );
            // Prompts may have been sent while connect was in flight; fail them so runs
            // do not stay InFlight forever.
            drain_slot_prompts_on_connect_failure(
                &cmd_rx,
                &done_tx,
                &agent_config_id,
                &cwd_buf,
                slot_id,
                &err_msg,
            );
            return;
        }
    };

    while let Ok(cmd) = cmd_rx.recv() {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        match cmd {
            SlotCommand::Prompt { run_id, prompt } => {
                let result = session.prompt(&prompt).map_err(|err| err.to_string());
                let _ = done_tx.send(SlotCompletion {
                    agent_config_id: agent_config_id.clone(),
                    cwd: cwd_buf.clone(),
                    slot_id,
                    run_id,
                    result,
                });
            }
            SlotCommand::Shutdown => break,
        }
    }

    session.shutdown(child_slot);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn deep_dive_prompt_includes_context() {
        let prompt = build_deep_dive_prompt(
            "# Deep dive\n\nRole text.",
            &DeepDiveContext {
                project: "tod".into(),
                task: "interview".into(),
                lifecycle_state: "active".into(),
                interview_purpose: "implementation".into(),
                interview_phase: "design".into(),
                question_id: "q-001".into(),
                question_body: "How should settings persist?".into(),
            },
            Some("User: Let's explore trade-offs"),
        );
        assert!(prompt.contains("Role text."));
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
                &format!("(Get-CimInstance Win32_Process -Filter '{filter}').ProcessId"),
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
