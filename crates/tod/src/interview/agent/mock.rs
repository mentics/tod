use super::answer_pool::{AnswerProcessorPoolManager, AnswerProcessorPoolStats, AnswerSubmitAssignment};
use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState,
    DeepDiveContext, RunId,
};
use super::question_maker_pool::{QuestionMakerPoolManager, QuestionMakerSubmitAssignment};
use tod_store::agent_traffic::{
    AgentCategory, InterviewAgentCounts, SharedAgentTrafficLog, TrafficDirection,
};
use crate::interview::config::path_for_storage;
use crate::interview::settings::{AnswerProcessorSettings, QuestionMakerSettings};
use crate::interview::transcript::new_transcript_filename;
use crate::process_bundle::InterviewAgentPrompt;
use anyhow::{Context, Result, bail};
use chrono::Local;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Fast in-process agent backend for UI tests. Writes realistic on-disk scaffolding
/// and queue/status files; never calls Cursor ACP or any external process.
pub struct MockAgentProvider {
    runs: HashMap<RunId, AgentRunState>,
    answer_pool: AnswerProcessorPoolManager,
    answer_run_agent: HashMap<RunId, String>,
    answer_completion_tx: mpsc::Sender<AnswerJobResult>,
    answer_completion_rx: mpsc::Receiver<AnswerJobResult>,
    question_maker_pool: QuestionMakerPoolManager,
    question_maker_run_agent: HashMap<RunId, String>,
    question_maker_completion_tx: mpsc::Sender<QuestionMakerJobResult>,
    question_maker_completion_rx: mpsc::Receiver<QuestionMakerJobResult>,
    fleet_run_agent: HashMap<RunId, String>,
    traffic_log: Option<SharedAgentTrafficLog>,
}

struct AnswerJobResult {
    agent_config_id: String,
    cwd: PathBuf,
    slot_id: u32,
    run_id: RunId,
    result: Result<String, String>,
}

struct QuestionMakerJobResult {
    agent_config_id: String,
    cwd: PathBuf,
    slot_id: u32,
    run_id: RunId,
    result: Result<String, String>,
}

impl MockAgentProvider {
    pub fn new() -> Self {
        let (answer_completion_tx, answer_completion_rx) = mpsc::channel();
        let (question_maker_completion_tx, question_maker_completion_rx) = mpsc::channel();
        Self {
            runs: HashMap::new(),
            answer_pool: AnswerProcessorPoolManager::default(),
            answer_run_agent: HashMap::new(),
            answer_completion_tx,
            answer_completion_rx,
            question_maker_pool: QuestionMakerPoolManager::default(),
            question_maker_run_agent: HashMap::new(),
            question_maker_completion_tx,
            question_maker_completion_rx,
            fleet_run_agent: HashMap::new(),
            traffic_log: None,
        }
    }

    pub fn with_traffic_log(mut self, traffic_log: SharedAgentTrafficLog) -> Self {
        self.traffic_log = Some(traffic_log);
        self
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
        let category = match kind {
            AgentRunKind::QuestionMakerReplenishment => AgentCategory::QuestionMaker,
            AgentRunKind::AnswerProcessor => AgentCategory::AnswerProcessor,
            AgentRunKind::DeepDiveChat => AgentCategory::DeepDive,
            AgentRunKind::FleetAgent => AgentCategory::Fleet,
        };
        let label = match kind {
            AgentRunKind::QuestionMakerReplenishment => "question-maker",
            AgentRunKind::AnswerProcessor => "answer-processor",
            AgentRunKind::DeepDiveChat => "deep-dive",
            AgentRunKind::FleetAgent => "fleet-agent",
        };
        let agent_id = self
            .fleet_run_agent
            .get(&run_id)
            .cloned()
            .unwrap_or_else(|| format!("{run_id:?}"));
        log.lock().expect("traffic log mutex").record(
            category,
            agent_id,
            label,
            direction,
            content,
        );
    }

    fn finish(
        &mut self,
        kind: AgentRunKind,
        request: Option<&str>,
        state: AgentRunState,
    ) -> AgentRunHandle {
        let id = RunId::new();
        if let Some(req) = request {
            self.log_traffic(kind, id, TrafficDirection::Request, req);
        }
        if let AgentRunState::Success(Some(text)) = &state {
            self.log_traffic(kind, id, TrafficDirection::Response, text);
        } else if let AgentRunState::Failure(message) = &state {
            self.log_traffic(kind, id, TrafficDirection::Response, message);
        }
        self.runs.insert(id, state.clone());
        AgentRunHandle { id }
    }

    fn drain_answer_completions(&mut self) {
        while let Ok(job) = self.answer_completion_rx.try_recv() {
            self.apply_answer_completion(job);
        }
    }

    fn apply_answer_completion(&mut self, job: AnswerJobResult) {
        let response = match &job.result {
            Ok(text) => text.clone(),
            Err(err) => format!("ERROR: {err}"),
        };
        self.log_traffic(
            AgentRunKind::AnswerProcessor,
            job.run_id,
            TrafficDirection::Response,
            &response,
        );
        let outcome = self.answer_pool.complete_run(
            &job.agent_config_id,
            job.slot_id,
            job.run_id,
            job.result,
        );
        if let Some(recycled) = outcome.recycled_slot_id {
            let _ = recycled;
        }
        for (slot_id, run_id, prompt) in outcome.dispatched {
            self.answer_run_agent
                .insert(run_id, job.agent_config_id.clone());
            self.spawn_answer_job(job.agent_config_id.clone(), job.cwd.clone(), slot_id, run_id, prompt);
        }
    }

    fn spawn_answer_job(
        &self,
        agent_config_id: String,
        cwd: PathBuf,
        slot_id: u32,
        run_id: RunId,
        prompt: String,
    ) {
        let tx = self.answer_completion_tx.clone();
        thread::spawn(move || {
            let result = process_answer_from_prompt(&prompt).map_err(|err| err.to_string());
            let _ = tx.send(AnswerJobResult {
                agent_config_id,
                cwd,
                slot_id,
                run_id,
                result,
            });
        });
    }

    fn drain_question_maker_completions(&mut self) {
        while let Ok(job) = self.question_maker_completion_rx.try_recv() {
            self.apply_question_maker_completion(job);
        }
    }

    fn apply_question_maker_completion(&mut self, job: QuestionMakerJobResult) {
        let response = match &job.result {
            Ok(text) => text.clone(),
            Err(err) => format!("ERROR: {err}"),
        };
        self.log_traffic(
            AgentRunKind::QuestionMakerReplenishment,
            job.run_id,
            TrafficDirection::Response,
            &response,
        );
        let outcome = self.question_maker_pool.complete_run(
            &job.agent_config_id,
            job.slot_id,
            job.run_id,
            job.result,
        );
        if let Some(recycled) = outcome.recycled_slot_id {
            let _ = recycled;
        }
        for (slot_id, run_id, prompt) in outcome.dispatched {
            self.question_maker_run_agent
                .insert(run_id, job.agent_config_id.clone());
            self.spawn_question_maker_job(
                job.agent_config_id.clone(),
                job.cwd.clone(),
                slot_id,
                run_id,
                prompt,
            );
        }
    }

    fn spawn_question_maker_job(
        &self,
        agent_config_id: String,
        cwd: PathBuf,
        slot_id: u32,
        run_id: RunId,
        prompt: String,
    ) {
        let tx = self.question_maker_completion_tx.clone();
        thread::spawn(move || {
            let result = process_question_maker_from_prompt(&prompt).map_err(|err| err.to_string());
            let _ = tx.send(QuestionMakerJobResult {
                agent_config_id,
                cwd,
                slot_id,
                run_id,
                result,
            });
        });
    }
}

impl Default for MockAgentProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for MockAgentProvider {
    fn start_question_maker_replenishment(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: InterviewAgentPrompt,
        pool: &QuestionMakerSettings,
    ) -> Result<AgentRunHandle> {
        let (assignment, run_id) = self
            .question_maker_pool
            .submit(agent_config_id.to_string(), pool.clone(), prompt.clone())
            .map_err(|e| anyhow::anyhow!(e))?;
        self.log_traffic(
            AgentRunKind::QuestionMakerReplenishment,
            run_id,
            TrafficDirection::Request,
            &prompt.full(),
        );
        self.question_maker_run_agent
            .insert(run_id, agent_config_id.to_string());
        match assignment {
            QuestionMakerSubmitAssignment::Dispatch { slot_id, prompt } => {
                self.spawn_question_maker_job(
                    agent_config_id.to_string(),
                    cwd.clone(),
                    slot_id,
                    run_id,
                    prompt,
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
    ) -> Result<AgentRunHandle> {
        let (assignment, run_id) = self
            .answer_pool
            .submit(agent_config_id.to_string(), pool.clone(), prompt.clone())
            .map_err(|e| anyhow::anyhow!(e))?;
        self.log_traffic(
            AgentRunKind::AnswerProcessor,
            run_id,
            TrafficDirection::Request,
            &prompt.full(),
        );
        self.answer_run_agent
            .insert(run_id, agent_config_id.to_string());
        match assignment {
            AnswerSubmitAssignment::Dispatch { slot_id, prompt } => {
                self.spawn_answer_job(
                    agent_config_id.to_string(),
                    cwd.clone(),
                    slot_id,
                    run_id,
                    prompt,
                );
            }
            AnswerSubmitAssignment::Queued { .. } => {}
        }
        Ok(AgentRunHandle { id: run_id })
    }

    fn start_deep_dive_chat(
        &mut self,
        _agent_config_id: &str,
        _cwd: PathBuf,
        context: DeepDiveContext,
        initial_message: Option<String>,
    ) -> Result<AgentRunHandle> {
        let body = initial_message
            .clone()
            .unwrap_or_else(|| context.question_body.clone());
        let request = initial_message
            .as_deref()
            .unwrap_or(context.question_body.as_str())
            .to_string();
        let reply = format!(
            "Mock deep-dive reply for {}:\n\nConsider: {}\n\n(Use this text is available.)",
            context.question_id,
            body.chars().take(240).collect::<String>()
        );
        Ok(self.finish(
            AgentRunKind::DeepDiveChat,
            Some(&request),
            AgentRunState::Success(Some(reply)),
        ))
    }

    fn start_fleet_agent(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: String,
    ) -> Result<AgentRunHandle> {
        let preview: String = prompt.chars().take(200).collect();
        let reply = format!(
            "Fleet agent run complete (mock).\n\n\
             Config: {agent_config_id}\n\
             Cwd: {}\n\n\
             Prompt preview:\n{preview}…",
            cwd.display()
        );
        let handle = self.finish(
            AgentRunKind::FleetAgent,
            Some(&prompt),
            AgentRunState::Success(Some(reply)),
        );
        self.fleet_run_agent
            .insert(handle.id, agent_config_id.to_string());
        Ok(handle)
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.drain_question_maker_completions();
        self.drain_answer_completions();
        if let Some(agent_id) = self.question_maker_run_agent.get(&id).cloned() {
            if let Some(state) = self.question_maker_pool.poll_run(&agent_id, id) {
                if !matches!(state, AgentRunState::InFlight) {
                    self.question_maker_run_agent.remove(&id);
                }
                return Some(state);
            }
        }
        if let Some(agent_id) = self.answer_run_agent.get(&id).cloned() {
            if let Some(state) = self.answer_pool.poll_run(&agent_id, id) {
                if !matches!(state, AgentRunState::InFlight) {
                    self.answer_run_agent.remove(&id);
                }
                return Some(state);
            }
        }
        self.runs.get(&id).cloned()
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        if let Some(agent_id) = self.question_maker_run_agent.remove(&id) {
            self.question_maker_pool.cancel_run(&agent_id, id);
            return Ok(());
        }
        if let Some(agent_id) = self.answer_run_agent.remove(&id) {
            self.answer_pool.cancel_run(&agent_id, id);
            return Ok(());
        }
        self.runs.remove(&id);
        self.fleet_run_agent.remove(&id);
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        let pool_stats = self.answer_pool.global_stats();
        InterviewAgentCounts {
            question_maker_in_flight: self.question_maker_pool.in_flight_count(),
            answer_active: pool_stats.active,
            answer_pool: pool_stats.in_pool,
            answer_max: pool_stats.max,
            ..Default::default()
        }
    }
}

fn process_question_maker_from_prompt(prompt: &str) -> Result<String> {
    if prompt.contains("Interview UI kickoff") {
        bootstrap_from_prompt(prompt)
    } else if prompt.contains("Action payload") || prompt.contains("Process question maker action") {
        action_from_prompt(prompt)
    } else {
        replenish_from_prompt(prompt)
    }
}

fn bootstrap_from_prompt(prompt: &str) -> Result<String> {
    let node_id = prompt_field(prompt, "Node id").context("mock bootstrap: missing Node id")?;
    let phase = prompt_field(prompt, "Phase").unwrap_or_else(|| "project-defining".into());
    let phase = phase.split('(').next().unwrap_or(&phase).trim().to_string();
    let scratch = prompt_field(prompt, "Session scratchpad")
        .or_else(|| {
            prompt_field(prompt, "Create interview-config at").and_then(|p| {
                std::path::PathBuf::from(&p)
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
            })
        })
        .context("mock bootstrap: missing Session scratchpad")?;
    let config_path = prompt_field(prompt, "Create interview-config at")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&scratch).join("interview-config.md"));
    let node_uuid = uuid::Uuid::parse_str(&node_id).unwrap_or_else(|_| uuid::Uuid::new_v4());
    let session_stem = prompt_field(prompt, "Session id").unwrap_or_else(|| {
        new_transcript_filename(
            &format!("{}-interview", phase.replace(' ', "-")),
            Local::now(),
        )
        .trim_end_matches(".md")
        .to_string()
    });

    let session_dir = PathBuf::from(scratch.trim());
    fs::create_dir_all(&session_dir)?;
    let queue = session_dir.join("queue");
    fs::create_dir_all(&queue)?;

    let transcript = session_dir.join("transcript.md");
    fs::write(
        &transcript,
        format!(
            "# Mock interview — {session_stem}\n\n## Session\n\n**Node:** {node_id}\n**Phase:** {phase}\n\n",
        ),
    )?;

    let question_maker_status = session_dir.join("question-maker-status.md");
    let answer_processor_status = session_dir.join("answer-processor-status.md");
    let scope_dir = session_dir.join("scope");
    fs::create_dir_all(&scope_dir)?;
    let obligations = scope_dir.join("obligations.md");
    fs::write(&obligations, "# Mock obligations\n")?;

    let config = format!(
        "# Interview config\n\n\
session_id: {session_stem}\n\
node_id: {node_uuid}\n\
phase: {phase}\n\
scratchpad: {}\n\
queue: {}\n\
queue_target: 8\n\
scope:\n\
  - {}\n",
        path_for_storage(&session_dir),
        path_for_storage(&queue),
        path_for_storage(&obligations),
    );
    fs::write(&config_path, config)?;
    write_status(&question_maker_status, "idle", "mock bootstrap complete")?;
    write_status(&answer_processor_status, "idle", "")?;
    write_queue_questions(&queue, 8, "mock-bootstrap")?;

    Ok(queue.display().to_string())
}

fn replenish_from_prompt(prompt: &str) -> Result<String> {
    let config_path =
        prompt_field(prompt, "Config path").context("mock replenish: missing Config path")?;
    let target: u32 = prompt
        .lines()
        .find_map(|l| l.strip_prefix("Target open question count: "))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(8);

    let config_text =
        fs::read_to_string(&config_path).with_context(|| format!("read config {config_path}"))?;
    let queue = config_value(&config_text, "queue").context("config missing queue")?;
    let status = config_value(&config_text, "question_maker_status");
    let queue_dir = PathBuf::from(queue.trim_end_matches(['/', '\\']));

    let existing = count_queue_files(&queue_dir);
    // Empty queue on replenish = no further questions (interview-complete signal).
    if existing == 0 {
        if let Some(status_path) = &status {
            write_status(
                &PathBuf::from(status_path),
                "complete",
                "no further questions",
            )?;
        }
        return Ok(queue_dir.display().to_string());
    }

    let need = target.saturating_sub(existing);
    if need > 0 {
        let start = existing + 1;
        for i in 0..need {
            let n = start + i;
            write_one_question(&queue_dir, n, "mock-replenish")?;
        }
    }

    if let Some(status_path) = status {
        let open = count_queue_files(&queue_dir);
        if open == 0 {
            write_status(
                &PathBuf::from(status_path),
                "complete",
                "no further questions",
            )?;
        } else {
            write_status(
                &PathBuf::from(status_path),
                "idle",
                "mock replenish complete",
            )?;
        }
    }

    Ok(queue_dir.display().to_string())
}

fn action_from_prompt(prompt: &str) -> Result<String> {
    let config_path =
        prompt_field(prompt, "Config path").context("mock action: missing Config path")?;
    let actions = extract_actions(prompt);
    if actions.is_empty() {
        bail!("mock action: no action/id in payload");
    }

    let config_text =
        fs::read_to_string(&config_path).with_context(|| format!("read config {config_path}"))?;
    let queue = config_value(&config_text, "queue").context("config missing queue")?;
    let status = config_value(&config_text, "question_maker_status");
    let queue_dir = PathBuf::from(queue.trim_end_matches(['/', '\\']));

    let mut handled = Vec::new();
    for (action, id) in &actions {
        match action.as_str() {
            "defer" => {
                if delete_queue_question(&queue_dir, id)? {
                    handled.push(format!("defer:{id}"));
                }
            }
            "reconsider" => {
                if rewrite_queue_question(&queue_dir, id, |body| {
                    format!(
                        "{}\n\n*(Mock reconsider — please re-check this question.)*\n",
                        body.trim_end()
                    )
                })? {
                    handled.push(format!("reconsider:{id}"));
                }
            }
            "more-options" => {
                if add_more_options(&queue_dir, id)? {
                    handled.push(format!("more-options:{id}"));
                }
            }
            other => bail!("mock action: unsupported action {other}"),
        }
    }

    if let Some(status_path) = status {
        write_status(
            &PathBuf::from(status_path),
            "idle",
            &format!("actions: {}", handled.join(",")),
        )?;
    }

    Ok(format!("actions: {}", handled.join(",")))
}

fn process_answer_from_prompt(prompt: &str) -> Result<String> {
    let config_path = prompt_field(prompt, "Config path")
        .context("mock answer-processor: missing Config path")?;
    let ids = extract_answer_ids(prompt);
    if ids.is_empty() {
        bail!("mock answer-processor: no answer id in payload");
    }

    let config_text =
        fs::read_to_string(&config_path).with_context(|| format!("read config {config_path}"))?;
    let queue = config_value(&config_text, "queue").context("config missing queue")?;
    let status = config_value(&config_text, "answer_processor_status");
    let queue_dir = PathBuf::from(queue.trim_end_matches(['/', '\\']));

    let mut resolved = Vec::new();
    for id in &ids {
        if delete_queue_question(&queue_dir, id)? {
            resolved.push(id.clone());
        }
    }

    if let Some(status_path) = status {
        write_status(
            &PathBuf::from(status_path),
            "idle",
            &format!("resolved: {}", resolved.join(",")),
        )?;
    }

    // Last question cleared → signal interview complete for UI (no further questions).
    if count_queue_files(&queue_dir) == 0 {
        if let Some(rs) = config_value(&config_text, "question_maker_status") {
            write_status(&PathBuf::from(rs), "complete", "no further questions")?;
        }
    }

    Ok(format!("resolved: {}\nmodified:", resolved.join(",")))
}

fn prompt_field(prompt: &str, label: &str) -> Option<String> {
    let prefix = format!("{label}: ");
    prompt.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

fn config_value(config: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    config.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

fn write_status(path: &Path, status: &str, message: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = if message.is_empty() {
        format!("status: {status}\n")
    } else {
        format!("status: {status}\nmessage: {message}\n")
    };
    fs::write(path, body)?;
    Ok(())
}

fn count_queue_files(queue: &Path) -> u32 {
    fs::read_dir(queue)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| x.eq_ignore_ascii_case("md"))
                })
                .count() as u32
        })
        .unwrap_or(0)
}

fn write_queue_questions(queue: &Path, count: u32, tag: &str) -> Result<()> {
    for n in 1..=count {
        write_one_question(queue, n, tag)?;
    }
    Ok(())
}

fn write_one_question(queue: &Path, n: u32, tag: &str) -> Result<()> {
    let id = format!("q-{n:03}");
    let path = queue.join(format!("{id}-{tag}.md"));
    let body = if n % 2 == 1 {
        format!(
            "---\nid: {id}\ncreated: 2026-08-24T12:00:00Z\nlayer: task\nkind: decision\ncontext: |\n  Mock context for question {n} ({tag}).\nquestion: Which mock option do you want?\nrecommend: 1 — default mock pick\noptions:\n  - key: \"1\"\n    label: Option One\n  - key: \"2\"\n    label: Option Two\n---\n"
        )
    } else {
        format!(
            "---\nid: {id}\ncreated: 2026-08-24T12:00:00Z\nlayer: task\nkind: wording\nquestion: Approve this mock statement for question {n}?\nproposed_text: |\n  Mock durable statement {n} ({tag}).\noptions:\n  - key: \"1\"\n    label: Accept\n  - key: \"2\"\n    label: Modify — describe changes\n  - key: \"3\"\n    label: Reject\n---\n"
        )
    };
    fs::write(path, body)?;
    Ok(())
}

fn find_queue_question_path(queue: &Path, id: &str) -> Option<PathBuf> {
    let Ok(rd) = fs::read_dir(queue) else {
        return None;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l.trim() == format!("id: {id}"))
            || path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{id}-")) || n == &format!("{id}.md"))
        {
            return Some(path);
        }
    }
    None
}

fn rewrite_queue_question(
    queue: &Path,
    id: &str,
    mutate_body: impl FnOnce(&str) -> String,
) -> Result<bool> {
    let Some(path) = find_queue_question_path(queue, id) else {
        return Ok(false);
    };
    let text = fs::read_to_string(&path)?;
    let (front, body) = split_front_matter(&text);
    let new_body = mutate_body(body);
    fs::write(path, format!("{front}{new_body}"))?;
    Ok(true)
}

fn add_more_options(queue: &Path, id: &str) -> Result<bool> {
    let Some(path) = find_queue_question_path(queue, id) else {
        return Ok(false);
    };
    let text = fs::read_to_string(&path)?;
    let (front, body) = split_front_matter(&text);
    let next_key = next_option_key(front);
    let meta = front
        .trim_end()
        .trim_end_matches("---")
        .trim_end()
        .to_string();
    let mut rebuilt = meta;
    if !rebuilt.contains("options:") {
        rebuilt.push_str("\noptions:");
    }
    rebuilt.push_str(&format!(
        "\n  - key: \"{next_key}\"\n    label: Mock extra option {next_key}\n---\n"
    ));
    fs::write(path, format!("{rebuilt}{body}"))?;
    Ok(true)
}

fn split_front_matter(text: &str) -> (&str, &str) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some((meta, body)) = rest.split_once("\n---\n") {
            let front_end = "---\n".len() + meta.len() + "\n---\n".len();
            return (&text[..front_end], body);
        }
    }
    (text, "")
}

fn next_option_key(front: &str) -> u32 {
    let mut max = 0u32;
    for line in front.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- key:") {
            let key = rest.trim().trim_matches('"');
            if let Ok(n) = key.parse::<u32>() {
                max = max.max(n);
            }
        }
    }
    max + 1
}

fn delete_queue_question(queue: &Path, id: &str) -> Result<bool> {
    let Some(path) = find_queue_question_path(queue, id) else {
        return Ok(false);
    };
    fs::remove_file(&path)?;
    Ok(true)
}

fn extract_answer_ids(prompt: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut in_payload = false;
    for line in prompt.lines() {
        if line.contains("Answer payload") || line.contains("Action payload") {
            in_payload = true;
            continue;
        }
        if !in_payload {
            // Also accept bare `id:` anywhere in the prompt.
            if let Some(rest) = line.trim().strip_prefix("id:") {
                let id = rest.trim().trim_matches('"').to_string();
                if !id.is_empty() && !ids.contains(&id) {
                    ids.push(id);
                }
            }
            continue;
        }
        if let Some(rest) = line.trim().strip_prefix("- id:") {
            let id = rest.trim().trim_matches('"').to_string();
            if !id.is_empty() {
                ids.push(id);
            }
        } else if let Some(rest) = line.trim().strip_prefix("id:") {
            let id = rest.trim().trim_matches('"').to_string();
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn extract_actions(prompt: &str) -> Vec<(String, String)> {
    let mut actions = Vec::new();
    let mut pending_action: Option<String> = None;
    let mut in_payload = false;
    for line in prompt.lines() {
        if line.contains("Action payload") {
            in_payload = true;
            continue;
        }
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("action:") {
            pending_action = Some(rest.trim().trim_matches('"').to_string());
            continue;
        }
        if let Some(rest) = t.strip_prefix("id:") {
            let id = rest.trim().trim_matches('"').to_string();
            if let Some(action) = pending_action.take() {
                if !id.is_empty() {
                    actions.push((action, id));
                }
            } else if in_payload && !id.is_empty() {
                // id without preceding action in this unit — skip
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::agent::answer_pool::AnswerProcessorPoolStats;
    use crate::interview::config::agent_scratchpad_for_node;
    use crate::interview::settings::{AnswerProcessorSettings, QuestionMakerSettings};
    use std::time::Duration;

    fn poll_run(mock: &mut MockAgentProvider, id: RunId) -> AgentRunState {
        for _ in 0..200 {
            if let Some(state) = mock.poll_run(id) {
                if !matches!(state, AgentRunState::InFlight) {
                    return state;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for run {id:?}");
    }

    fn question_maker_prompt(turn: &str) -> InterviewAgentPrompt {
        InterviewAgentPrompt {
            session_prefix: String::new(),
            turn: turn.into(),
        }
    }

    #[test]
    fn mock_bootstrap_writes_config_and_queue() {
        let root = std::env::temp_dir().join(format!("tod-mock-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let node_id = uuid::Uuid::new_v4();
        let scratch_base = agent_scratchpad_for_node(&root, node_id);
        fs::create_dir_all(&scratch_base).unwrap();
        let session_dir = scratch_base.join("mock-session");
        fs::create_dir_all(&session_dir).unwrap();

        let mut mock = MockAgentProvider::new();
        let pool = QuestionMakerSettings::default();
        let prompt = question_maker_prompt(&format!(
            "Interview UI kickoff — bootstrap this interview session.\n\
             Session id: mock-session\n\
             Node id: {node_id}\n\
             Phase: project-defining\n\
             Session scratchpad: {}\n\
             Create interview-config at: {}\n",
            session_dir.display(),
            session_dir.join("interview-config.md").display(),
        ));
        let handle = mock
            .start_question_maker_replenishment("mock-agent", root.clone(), prompt, &pool)
            .unwrap();
        assert!(matches!(
            poll_run(&mut mock, handle.id),
            AgentRunState::Success(_)
        ));

        assert!(session_dir.join("interview-config.md").is_file());
        assert_eq!(count_queue_files(&session_dir.join("queue")), 8);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mock_action_defer_deletes_question() {
        let root = std::env::temp_dir().join(format!("tod-mock-act-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let node_id = uuid::Uuid::new_v4();
        let scratch_base = agent_scratchpad_for_node(&root, node_id);
        let session_dir = scratch_base.join("mock-session");
        fs::create_dir_all(&session_dir).unwrap();

        let mut mock = MockAgentProvider::new();
        let pool = QuestionMakerSettings::default();
        let prompt = question_maker_prompt(&format!(
            "Interview UI kickoff — bootstrap this interview session.\n\
             Session id: mock-session\n\
             Node id: {node_id}\n\
             Phase: project-defining\n\
             Session scratchpad: {}\n\
             Create interview-config at: {}\n",
            session_dir.display(),
            session_dir.join("interview-config.md").display(),
        ));
        let bootstrap_handle = mock
            .start_question_maker_replenishment("mock-agent", root.clone(), prompt, &pool)
            .unwrap();
        assert!(matches!(
            poll_run(&mut mock, bootstrap_handle.id),
            AgentRunState::Success(_)
        ));

        let config_path = session_dir.join("interview-config.md");
        let queue = session_dir.join("queue");
        assert!(find_queue_question_path(&queue, "q-001").is_some());

        let action_prompt = question_maker_prompt(&format!(
            "Process question maker action submission.\n\
             Config path: {}\n\
             Action payload (YAML multi-record):\n\
             ---\naction: defer\nid: q-001\n---\n",
            config_path.display()
        ));
        let handle = mock
            .start_question_maker_replenishment("mock-agent", root.clone(), action_prompt, &pool)
            .unwrap();
        assert!(matches!(
            poll_run(&mut mock, handle.id),
            AgentRunState::Success(_)
        ));
        assert!(find_queue_question_path(&queue, "q-001").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mock_answer_pool_allows_parallel_submits() {
        let mut mock = MockAgentProvider::new();
        let cwd = PathBuf::from("/tmp/mock-pool");
        let pool = AnswerProcessorSettings::default();
        let prompt = InterviewAgentPrompt {
            session_prefix: String::new(),
            turn: "Config path: /x\nAnswer payload:\n---\nid: q-001\n---\n".into(),
        };

        let h0 = mock
            .start_answer_processor("mock-agent", cwd.clone(), prompt.clone(), &pool)
            .unwrap();
        let h1 = mock
            .start_answer_processor("mock-agent", cwd.clone(), prompt, &pool)
            .unwrap();
        assert_eq!(mock.pool_stats("mock-agent", &pool).in_pool, 2);
        assert_eq!(mock.pool_stats("mock-agent", &pool).active, 2);
        assert!(matches!(
            poll_run(&mut mock, h0.id),
            AgentRunState::Failure(_)
        ));
        assert!(matches!(
            poll_run(&mut mock, h1.id),
            AgentRunState::Failure(_)
        ));
    }
}

#[cfg(test)]
impl MockAgentProvider {
    fn pool_stats(
        &self,
        agent_config_id: &str,
        pool: &AnswerProcessorSettings,
    ) -> AnswerProcessorPoolStats {
        self.answer_pool.stats(agent_config_id, pool)
    }
}
