//! Runs a node's interview agents: decides when the question maker and the
//! answer processor take a turn, and which agent session each turn goes to.
//!
//! A session gets the role docs and a snapshot once; every later turn carries
//! only the changes since its previous turn. Sessions rotate to a fresh
//! snapshot when they grow past the context budget or would resume cold after
//! the prompt cache lapsed.

use crate::interview::context::{ContextScope, delta, estimate_tokens, snapshot};
use crate::interview::phase::phase_for_session_key;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tod_agent::{
    AgentLaunchOptions, AgentProvider, AgentRunState, RunId, SessionOpening, SessionPurpose,
    SessionTurn,
};
use tod_store::fleet::FleetStore;
use tod_store::interview::*;
use tod_store::settings::InterviewContextSettings;
use uuid::Uuid;

/// Answer processor lanes: one normally, a second when the backlog is large.
const MAX_ANSWER_LANES: i64 = 2;
/// Attempts per answer before the driver stops retrying it on its own.
const MAX_ANSWER_ATTEMPTS: u32 = 3;
const MAX_QUESTION_MAKER_FAILURES: u32 = 3;
/// Pause after a question maker turn that added nothing without declaring exhaustion.
const QUESTION_MAKER_IDLE_PAUSE: Duration = Duration::from_secs(60);

pub struct DriverConfig {
    pub node_id: Uuid,
    pub node_title: String,
    pub interview_session_id: Uuid,
    /// Session phase key (`task-requirements-interview`, …).
    pub phase_key: String,
    /// Working directory for design and planning (the node's repo).
    pub repo_cwd: PathBuf,
    pub data_root: PathBuf,
    pub tod_cli: PathBuf,
    pub launch: AgentLaunchOptions,
    pub replenish_threshold: u32,
    pub context: InterviewContextSettings,
    pub question_maker_prefix: String,
    pub answer_processor_prefix: String,
}

struct Turn {
    run: RunId,
    role: Role,
    lane: i64,
    agent_session: Uuid,
    key: String,
    chars_at_start: u64,
    questions: Vec<i64>,
    open_before: usize,
}

#[derive(Default)]
struct Backoff {
    failures: u32,
    next_at: Option<Instant>,
}

impl Backoff {
    fn ready(&self) -> bool {
        self.next_at.is_none_or(|at| Instant::now() >= at)
    }

    fn fail(&mut self) {
        self.failures += 1;
        let secs = 1u64 << self.failures.min(5);
        self.next_at = Some(Instant::now() + Duration::from_secs(secs));
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverStatus {
    pub question_maker_running: bool,
    pub answers_in_flight: usize,
    pub answer_lanes_busy: usize,
    pub last_error: Option<String>,
    /// The question maker failed repeatedly and waits for a manual retry.
    pub manual_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverEvent {
    QuestionMakerFinished { error: Option<String> },
    AnswersFinished { questions: Vec<i64>, error: Option<String> },
}

pub struct InterviewDriver {
    config: DriverConfig,
    phase: &'static str,
    turns: Vec<Turn>,
    question_maker_backoff: Backoff,
    answer_backoff: Backoff,
    question_maker_idle_until: Option<Instant>,
    attempts: HashMap<i64, u32>,
    manual_required: bool,
    last_error: Option<String>,
}

impl InterviewDriver {
    pub fn new(config: DriverConfig) -> Self {
        let phase = phase_for_session_key(&config.phase_key);
        Self {
            config,
            phase,
            turns: Vec::new(),
            question_maker_backoff: Backoff::default(),
            answer_backoff: Backoff::default(),
            question_maker_idle_until: None,
            attempts: HashMap::new(),
            manual_required: false,
            last_error: None,
        }
    }

    pub fn config(&self) -> &DriverConfig {
        &self.config
    }

    pub fn phase(&self) -> &'static str {
        self.phase
    }

    pub fn status(&self) -> DriverStatus {
        let answers: Vec<&Turn> = self
            .turns
            .iter()
            .filter(|t| t.role == Role::AnswerProcessor)
            .collect();
        DriverStatus {
            question_maker_running: self.question_maker_running(),
            answers_in_flight: answers.iter().map(|t| t.questions.len()).sum(),
            answer_lanes_busy: answers.len(),
            last_error: self.last_error.clone(),
            manual_required: self.manual_required,
        }
    }

    /// Clear failure state and let the question maker run again.
    pub fn retry(&mut self) {
        self.manual_required = false;
        self.question_maker_backoff.reset();
        self.answer_backoff.reset();
        self.question_maker_idle_until = None;
        self.attempts.clear();
        self.last_error = None;
    }

    /// Ask for a question maker turn soon (e.g. the user sent a question back).
    pub fn wake_question_maker(&mut self) {
        self.question_maker_idle_until = None;
    }

    fn question_maker_running(&self) -> bool {
        self.turns.iter().any(|t| t.role == Role::QuestionMaker)
    }

    /// Advance: collect finished turns, then start any turn that is due.
    pub fn tick(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider) -> Vec<DriverEvent> {
        let mut events = Vec::new();
        self.poll_turns(fleet, agent, &mut events);
        if let Err(err) = self.schedule(fleet, agent) {
            self.last_error = Some(format!("{err:#}"));
        }
        events
    }

    fn poll_turns(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        events: &mut Vec<DriverEvent>,
    ) {
        let mut finished = Vec::new();
        for (index, turn) in self.turns.iter().enumerate() {
            match agent.poll_run(turn.run) {
                Some(AgentRunState::InFlight(_)) => {}
                // Interview agents don't run with write access that would trigger a
                // permission prompt; treat one as still-running rather than surfacing it.
                Some(AgentRunState::NeedsPermission(_)) => {}
                Some(AgentRunState::Success(_)) => finished.push((index, None)),
                Some(AgentRunState::Failure(message)) => finished.push((index, Some(message))),
                None => finished.push((index, Some("agent run was lost".to_string()))),
            }
        }
        for (index, error) in finished.into_iter().rev() {
            let turn = self.turns.remove(index);
            let error = match self.finish_turn(fleet, agent, &turn, error) {
                Ok(error) => error,
                Err(err) => Some(format!("{err:#}")),
            };
            if let Some(message) = &error {
                self.last_error = Some(message.clone());
            }
            events.push(match turn.role {
                Role::QuestionMaker | Role::Drafter => DriverEvent::QuestionMakerFinished { error },
                Role::AnswerProcessor => DriverEvent::AnswersFinished {
                    questions: turn.questions.clone(),
                    error,
                },
            });
        }
    }

    /// Record the turn and judge its outcome; returns the error to surface.
    fn finish_turn(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        turn: &Turn,
        run_error: Option<String>,
    ) -> Result<Option<String>> {
        let chars = agent
            .session_context_chars(&turn.key)
            .unwrap_or(turn.chars_at_start);
        let added_tokens = (chars.saturating_sub(turn.chars_at_start) / 4) as i64;
        let row = fleet
            .read(|conn| InterviewRepo::new(conn).get_agent_session(turn.agent_session))?
            .context("interview agent session vanished")?;
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::RecordAgentTurn {
                id: turn.agent_session,
                agent_session_id: agent.session_id(&turn.key),
                synced_rev: None,
                est_tokens: Some(row.est_tokens + added_tokens),
                turn_completed: run_error.is_none(),
            },
        )?;

        let (open_now, state, unprocessed) = fleet.read(|conn| {
            let repo = InterviewRepo::new(conn);
            Ok((
                repo.list_questions(self.config.node_id, &[STATUS_OPEN])?.len(),
                repo.question_maker_state(self.config.interview_session_id)?
                    .map(|s| s.0)
                    .unwrap_or_default(),
                repo.unprocessed_answers(self.config.node_id)?
                    .into_iter()
                    .map(|q| q.seq)
                    .collect::<HashSet<i64>>(),
            ))
        })?;

        let failed = match turn.role {
            Role::QuestionMaker | Role::Drafter => {
                if let Some(message) = run_error {
                    self.question_maker_backoff.fail();
                    if self.question_maker_backoff.failures >= MAX_QUESTION_MAKER_FAILURES {
                        self.manual_required = true;
                    }
                    Some(message)
                } else {
                    self.question_maker_backoff.reset();
                    if open_now <= turn.open_before && state != QUESTION_MAKER_EXHAUSTED {
                        self.question_maker_idle_until = Some(Instant::now() + QUESTION_MAKER_IDLE_PAUSE);
                    }
                    None
                }
            }
            Role::AnswerProcessor => {
                let left: Vec<i64> = turn
                    .questions
                    .iter()
                    .copied()
                    .filter(|seq| unprocessed.contains(seq))
                    .collect();
                for seq in &turn.questions {
                    if left.contains(seq) {
                        *self.attempts.entry(*seq).or_default() += 1;
                    } else {
                        self.attempts.remove(seq);
                    }
                }
                match (run_error, left.is_empty()) {
                    (Some(message), _) => {
                        self.answer_backoff.fail();
                        Some(message)
                    }
                    (None, false) => {
                        self.answer_backoff.fail();
                        let labels: Vec<String> = left.iter().map(|s| format!("q-{s}")).collect();
                        Some(format!(
                            "The answer processor left {} unprocessed",
                            labels.join(", ")
                        ))
                    }
                    (None, true) => {
                        self.answer_backoff.reset();
                        None
                    }
                }
            }
        };

        // A session whose turns keep failing may be broken; start the next one fresh.
        let backoff = match turn.role {
            Role::QuestionMaker | Role::Drafter => &self.question_maker_backoff,
            Role::AnswerProcessor => &self.answer_backoff,
        };
        if failed.is_some() && backoff.failures >= 2 {
            self.retire(fleet, agent, turn.agent_session)?;
        }
        Ok(failed)
    }

    /// Prefix tagging a handoff note as one derived from a failing gate row,
    /// so it can be found again later to dedupe or close it. Kept in sync
    /// with the app-side equivalent used when a human waives a row directly.
    const GATE_HANDOFF_PREFIX: &'static str = "Gate check failed: ";

    /// The lifecycle name a failing gate row's `from_state` uses for this
    /// driver's interview phase — distinct from the phase key itself,
    /// since the `requirements` phase gates on the `proposed` → `design`
    /// transition, not a `requirements` lifecycle state.
    fn gate_from_state(&self) -> &'static str {
        match self.phase {
            PHASE_DESIGN => "design",
            PHASE_PLANNING => "planning",
            _ => "proposed",
        }
    }

    /// Mirror any currently-failing gate criterion that names this phase's
    /// interview as its resolution into an open handoff note, and close any
    /// such note whose criterion has since stopped failing — purely from
    /// persisted `node_gate_evaluations` state, so it doesn't matter which
    /// entry point (or none) led the user to open this interview.
    fn sync_gate_failures(&self, fleet: &FleetStore) -> Result<bool> {
        use tod_store::outline::repos::gate::{ACTION_INTERVIEW, GateRepo};

        let node = self.config.node_id;
        let from_state = self.gate_from_state();
        let (failing, open_gate_handoffs) = fleet.read(|conn| {
            let failing = GateRepo::new(conn).list_open_interview_failures(node, from_state)?;
            let open_gate_handoffs = InterviewRepo::new(conn)
                .list_memory(node, Some(MEMORY_HANDOFF), Some(MEMORY_OPEN))?
                .into_iter()
                .filter(|m| m.body.starts_with(Self::GATE_HANDOFF_PREFIX))
                .collect::<Vec<_>>();
            Ok((failing, open_gate_handoffs))
        })?;

        let mut wrote_new = false;
        for (criterion, eval) in &failing {
            debug_assert_eq!(eval.action, ACTION_INTERVIEW);
            let tag = format!("{}{}", Self::GATE_HANDOFF_PREFIX, criterion.label);
            if open_gate_handoffs.iter().any(|m| m.body.starts_with(&tag)) {
                continue;
            }
            let body = match eval.detail.as_deref() {
                Some(detail) if !detail.trim().is_empty() => format!("{tag} — {detail}"),
                _ => tag,
            };
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::AddMemory {
                    node_id: node,
                    kind: MEMORY_HANDOFF.into(),
                    phase: Some(self.phase.to_string()),
                    body,
                    question_seq: None,
                },
            )?;
            wrote_new = true;
        }

        for handoff in &open_gate_handoffs {
            let still_failing = failing.iter().any(|(criterion, _)| {
                handoff
                    .body
                    .starts_with(&format!("{}{}", Self::GATE_HANDOFF_PREFIX, criterion.label))
            });
            if !still_failing {
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::UpdateMemory {
                        node_id: node,
                        seq: handoff.seq,
                        body: None,
                        status: Some(MEMORY_DONE.to_string()),
                    },
                )?;
            }
        }

        Ok(wrote_new)
    }

    fn schedule(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider) -> Result<()> {
        let node = self.config.node_id;
        let gate_handoff = self.sync_gate_failures(fleet).unwrap_or(false);
        let (mut open, unprocessed, state, new_handoff, handoffs_open, stale) = fleet.read(|conn| {
            let repo = InterviewRepo::new(conn);
            let handoffs = repo.list_memory(node, Some(MEMORY_HANDOFF), Some(MEMORY_OPEN))?;
            // A handoff is new to the question maker when it was written after that
            // session's context was built — including during its own turn, which a
            // timestamp taken when the turn finished would hide.
            let new_handoff = match repo
                .live_agent_sessions(node, self.phase, Role::QuestionMaker)?
                .first()
            {
                _ if handoffs.is_empty() => false,
                None => true,
                Some(session) => repo
                    .changes_since(&[node], session.synced_rev, &session.id.to_string())?
                    .iter()
                    .any(|c| {
                        c.entity == ENTITY_MEMORY
                            && c.op == "insert"
                            && handoffs.iter().any(|h| h.id == c.entity_id)
                    }),
            };
            Ok((
                repo.list_questions(node, &[STATUS_OPEN])?.len(),
                repo.unprocessed_answers(node)?
                    .into_iter()
                    .map(|q| q.seq)
                    .collect::<Vec<_>>(),
                repo.question_maker_state(self.config.interview_session_id)?
                    .map(|s| s.0)
                    .unwrap_or_default(),
                new_handoff,
                !handoffs.is_empty(),
                repo.stale_proposal_questions(node)?,
            ))
        })?;

        // A question whose proposal targets a removed obligation can't be
        // accepted; the answer processor should have withdrawn it, so the app
        // does it as a backstop and asks for a replacement.
        //
        // `state == exhausted && handoffs_open` on its own (not just `new_handoff`,
        // which only fires the tick a handoff first appears) re-triggers a turn on
        // every subsequent poll too — needed because `SetExhausted` now refuses to
        // apply while handoffs are open, but a turn can still end exhausted in the
        // store if it declared exhaustion before that guard existed, or before this
        // fix landed, leaving a stale state on disk with nothing else to wake it.
        let mut wake_question_maker =
            new_handoff || gate_handoff || (state == QUESTION_MAKER_EXHAUSTED && handoffs_open);
        if !stale.is_empty() {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::WithdrawStaleProposals { node_id: node },
            )?;
            open = fleet.read(|conn| {
                Ok(InterviewRepo::new(conn)
                    .list_questions(node, &[STATUS_OPEN])?
                    .len())
            })?;
            wake_question_maker = true;
        }

        if !self.question_maker_running()
            && !self.manual_required
            && self.question_maker_backoff.ready()
        {
            let exhausted = state == QUESTION_MAKER_EXHAUSTED;
            let idle = self
                .question_maker_idle_until
                .is_some_and(|until| Instant::now() < until);
            let below_target = open < self.config.replenish_threshold as usize;
            if wake_question_maker || (!exhausted && below_target && !idle) {
                if exhausted {
                    fleet.interview(
                        ACTOR_USER,
                        InterviewCommand::SetExhausted {
                            session_id: self.config.interview_session_id,
                            reason: None,
                        },
                    )?;
                }
                let instruction = format!(
                    "Target open questions: {}.",
                    self.config.replenish_threshold
                );
                if let Err(err) =
                    self.start_turn(fleet, agent, Role::QuestionMaker, 0, Vec::new(), open, instruction)
                {
                    self.question_maker_backoff.fail();
                    return Err(err);
                }
            }
        }

        let in_flight: HashSet<i64> = self
            .turns
            .iter()
            .flat_map(|t| t.questions.iter().copied())
            .collect();
        let pending: Vec<i64> = unprocessed
            .iter()
            .copied()
            .filter(|seq| !in_flight.contains(seq))
            .filter(|seq| self.attempts.get(seq).copied().unwrap_or(0) < MAX_ANSWER_ATTEMPTS)
            .collect();
        if pending.is_empty() || !self.answer_backoff.ready() {
            return Ok(());
        }
        let busy: HashSet<i64> = self
            .turns
            .iter()
            .filter(|t| t.role == Role::AnswerProcessor)
            .map(|t| t.lane)
            .collect();
        let half = (self.config.replenish_threshold as usize / 2).max(1);
        let fan_out = unprocessed.len() > half;
        let free: Vec<i64> = (0..MAX_ANSWER_LANES)
            .filter(|lane| !busy.contains(lane) && (*lane == 0 || fan_out))
            .collect();
        let batches: Vec<(i64, Vec<i64>)> = match free.as_slice() {
            [] => Vec::new(),
            [a, b, ..] if fan_out && pending.len() > 1 => {
                let split = pending.len().div_ceil(2);
                vec![(*a, pending[..split].to_vec()), (*b, pending[split..].to_vec())]
            }
            [a, ..] => vec![(*a, pending)],
        };
        for (lane, questions) in batches {
            let labels: Vec<String> = questions.iter().map(|s| format!("q-{s}")).collect();
            let instruction = format!("Process: {}.", labels.join(", "));
            if let Err(err) = self.start_turn(
                fleet,
                agent,
                Role::AnswerProcessor,
                lane,
                questions,
                open,
                instruction,
            ) {
                self.answer_backoff.fail();
                return Err(err);
            }
        }
        Ok(())
    }

    fn retire(&self, fleet: &FleetStore, agent: &mut dyn AgentProvider, id: Uuid) -> Result<()> {
        agent.close_session(&session_key(id));
        fleet.interview(ACTOR_USER, InterviewCommand::RetireAgentSession { id })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn start_turn(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        role: Role,
        lane: i64,
        questions: Vec<i64>,
        open_before: usize,
        instruction: String,
    ) -> Result<()> {
        let now_ms = tod_store::outline::now_ms();
        let existing = fleet
            .read(|conn| {
                InterviewRepo::new(conn).live_agent_sessions(self.config.node_id, self.phase, role)
            })?
            .into_iter()
            .find(|s| s.lane == lane);
        let budget = self.config.context.context_budget_tokens as i64;
        let idle_ms = self.config.context.prompt_cache_idle_minutes as i64 * 60_000;
        let reusable = existing.as_ref().is_some_and(|s| {
            // Without a live process or a recorded session id the context is gone.
            let reachable =
                agent.session_id(&session_key(s.id)).is_some() || s.agent_session_id.is_some();
            let over_budget = s.est_tokens > budget;
            let cold = s.last_turn_at.is_some_and(|t| now_ms - t > idle_ms)
                && s.est_tokens > 2 * s.snapshot_tokens;
            reachable && !over_budget && !cold
        });
        if let Some(stale) = existing.as_ref().filter(|_| !reusable) {
            self.retire(fleet, agent, stale.id)?;
        }

        let scope_role = role;
        let data_root = self.config.data_root.clone();
        let tod_cli = self.config.tod_cli.clone();
        let scope = ContextScope {
            node_id: self.config.node_id,
            phase: self.phase,
            role: scope_role,
            interview_session_id: Some(self.config.interview_session_id),
            data_root: &data_root,
            tod_cli: &tod_cli,
            answered_cap: self.config.context.answered_history_cap as usize,
        };

        let (session_id, opening, resume, message) = match existing.filter(|_| reusable) {
            Some(row) => {
                let (rev, changes) = fleet.read(|conn| {
                    // Compute the delta before the watermark so a write landing in
                    // between is reflected in `rev` too, not left to duplicate into
                    // the next delta.
                    let changes = delta(conn, &scope, row.synced_rev, &row.id.to_string())?;
                    let rev = InterviewRepo::new(conn).head_rev()?;
                    Ok((rev, changes))
                })?;
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::RecordAgentTurn {
                        id: row.id,
                        agent_session_id: None,
                        synced_rev: Some(rev),
                        est_tokens: None,
                        turn_completed: false,
                    },
                )?;
                let message = if changes.is_empty() {
                    format!("# Turn\n\n{instruction}")
                } else {
                    format!("{changes}\n# Turn\n\n{instruction}")
                };
                (row.id, None, row.agent_session_id.clone(), message)
            }
            None => {
                let id = Uuid::new_v4();
                let (rev, state) = fleet.read(|conn| {
                    // Snapshot before the watermark, for the same reason as the
                    // resumed-session branch above.
                    let state = snapshot(conn, &scope)?;
                    let rev = InterviewRepo::new(conn).head_rev()?;
                    Ok((rev, state))
                })?;
                let prefix = match role {
                    Role::QuestionMaker | Role::Drafter => &self.config.question_maker_prefix,
                    Role::AnswerProcessor => &self.config.answer_processor_prefix,
                };
                // Byte-stable docs first so the provider can cache them across sessions.
                let context = format!("{}\n\n{state}", prefix.trim_end());
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::CreateAgentSession {
                        id,
                        node_id: self.config.node_id,
                        interview_session_id: Some(self.config.interview_session_id),
                        phase: self.phase.to_string(),
                        role,
                        lane,
                        synced_rev: rev,
                        snapshot_tokens: estimate_tokens(&context),
                    },
                )?;
                let title = format!(
                    "{} · {} · {}",
                    self.config.node_title,
                    match role {
                        Role::QuestionMaker => "question maker",
                        Role::AnswerProcessor => "answer processor",
                        Role::Drafter => "drafter",
                    },
                    self.phase
                );
                (
                    id,
                    Some(SessionOpening {
                        title,
                        context: Some(context),
                    }),
                    None,
                    format!("# Turn\n\n{instruction}"),
                )
            }
        };

        let key = session_key(session_id);
        let chars_at_start = agent.session_context_chars(&key).unwrap_or(0);
        let handle = agent.send_session_turn(SessionTurn {
            key: key.clone(),
            owner_id: self.config.node_id.to_string(),
            cwd: self.cwd()?,
            options: self.config.launch.clone(),
            resume_session_id: resume,
            opening,
            message,
            purpose: match role {
                Role::QuestionMaker => SessionPurpose::QuestionMaker,
                Role::AnswerProcessor => SessionPurpose::AnswerProcessor,
                Role::Drafter => SessionPurpose::Drafter,
            },
            env: vec![(ACTOR_ENV.to_string(), session_id.to_string())],
        })?;
        self.turns.push(Turn {
            run: handle.id,
            role,
            lane,
            agent_session: session_id,
            key,
            chars_at_start,
            questions,
            open_before,
        });
        Ok(())
    }

    /// Requirements work runs in an empty directory so the agent does not load
    /// a repository's instructions it has no use for; design and planning run
    /// in the node's repository.
    fn cwd(&self) -> Result<PathBuf> {
        if self.phase == PHASE_REQUIREMENTS {
            let dir = self.config.data_root.join("agent").join("interview");
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create {}", dir.display()))?;
            return Ok(dir);
        }
        Ok(self.config.repo_cwd.clone())
    }
}

fn session_key(id: Uuid) -> String {
    format!("interview-{id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, draft, fixture};
    use tod_agent::agent_traffic::InterviewAgentCounts;
    use tod_agent::{AgentPlatform, AgentRunHandle};
    use tod_store::outline::OutlineMutation;

    /// Records every turn; each run finishes immediately.
    #[derive(Default)]
    struct FakeAgent {
        turns: Vec<SessionTurn>,
        runs: HashMap<RunId, AgentRunState>,
        chars: HashMap<String, u64>,
        sessions: HashMap<String, String>,
        closed: Vec<String>,
    }

    impl AgentProvider for FakeAgent {
        fn start_fleet_agent(
            &mut self,
            _: &str,
            _: PathBuf,
            _: String,
            _: AgentLaunchOptions,
            _: String,
        ) -> anyhow::Result<AgentRunHandle> {
            anyhow::bail!("not used")
        }

        fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle> {
            let id = RunId::new();
            let sent: u64 = turn.prompt_blocks().iter().map(|b| b.len() as u64).sum();
            *self.chars.entry(turn.key.clone()).or_default() += sent;
            self.sessions
                .entry(turn.key.clone())
                .or_insert_with(|| format!("agent-side-{}", turn.key));
            self.runs.insert(id, AgentRunState::Success(Some("ok".into())));
            self.turns.push(turn);
            Ok(AgentRunHandle { id })
        }

        fn session_id(&self, key: &str) -> Option<String> {
            self.sessions.get(key).cloned()
        }

        fn fleet_run_session_id(&self, _id: RunId) -> Option<String> {
            None
        }

        fn fetch_full_transcript(
            &self,
            _platform: tod_agent::AgentPlatform,
            _cwd: &std::path::Path,
            _agent_session_id: &str,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        fn session_context_chars(&self, key: &str) -> Option<u64> {
            self.chars.get(key).copied()
        }

        fn close_session(&mut self, key: &str) {
            self.sessions.remove(key);
            self.closed.push(key.to_string());
        }

        fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
            self.runs.get(&id).cloned()
        }

        fn respond_to_permission(&mut self, _: RunId, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }

        fn cancel_run(&mut self, _: RunId) -> anyhow::Result<()> {
            Ok(())
        }

        fn interview_status_counts(&self) -> InterviewAgentCounts {
            InterviewAgentCounts::default()
        }
    }

    fn driver(fx: &Fixture, budget_tokens: u64) -> InterviewDriver {
        InterviewDriver::new(DriverConfig {
            node_id: fx.node,
            node_title: "Interview node".into(),
            interview_session_id: fx.session,
            phase_key: "task-requirements-interview".into(),
            repo_cwd: fx.root.clone(),
            data_root: fx.root.clone(),
            tod_cli: fx.root.join("tod-cli"),
            launch: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
            replenish_threshold: 4,
            context: InterviewContextSettings {
                context_budget_tokens: budget_tokens,
                ..Default::default()
            },
            question_maker_prefix: "QUESTION MAKER DOCS".into(),
            answer_processor_prefix: "ANSWER PROCESSOR DOCS".into(),
        })
    }

    fn actor(turn: &SessionTurn) -> String {
        turn.env
            .iter()
            .find(|(key, _)| key == ACTOR_ENV)
            .map(|(_, value)| value.clone())
            .expect("interview turns carry their actor")
    }

    fn live_sessions(fx: &Fixture, role: Role) -> Vec<AgentSessionRow> {
        fx.fleet
            .read(|conn| {
                InterviewRepo::new(conn).live_agent_sessions(fx.node, PHASE_REQUIREMENTS, role)
            })
            .unwrap()
    }

    #[test]
    fn a_session_gets_docs_and_snapshot_once_then_only_changes() {
        let fx = fixture();
        let mut agent = FakeAgent::default();
        let mut driver = driver(&fx, 100_000);

        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(agent.turns.len(), 1);
        let first = &agent.turns[0];
        assert_eq!(first.purpose, SessionPurpose::QuestionMaker);
        let opening = first.opening.as_ref().expect("first turn opens the session");
        let context = opening.context.as_deref().unwrap();
        assert!(context.starts_with("QUESTION MAKER DOCS"), "{context}");
        assert!(context.contains("# Interview state"), "{context}");
        assert_eq!(first.message, "# Turn\n\nTarget open questions: 4.");

        // The question maker writes two questions; the user answers one of them.
        let question_maker = actor(first);
        fx.question(&question_maker, draft("First question?"));
        fx.question(&question_maker, draft("Second question?"));
        fx.answer(1, Some(1), None);

        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(
            agent.turns.len(),
            3,
            "question maker continues, answer processor starts"
        );

        let second = &agent.turns[1];
        assert_eq!(second.purpose, SessionPurpose::QuestionMaker);
        assert_eq!(actor(second), question_maker, "same session reused");
        assert!(second.opening.is_none(), "docs and snapshot are not re-sent");
        assert!(second.resume_session_id.is_some());
        assert!(second.message.contains("q-1 answered: 1 Yes"), "{}", second.message);
        assert!(!second.message.contains("# Interview state"), "{}", second.message);
        assert!(
            !second.message.contains("question?"),
            "own questions are not echoed: {}",
            second.message
        );

        let processor = &agent.turns[2];
        assert_eq!(processor.purpose, SessionPurpose::AnswerProcessor);
        let context = processor.opening.as_ref().unwrap().context.as_deref().unwrap();
        assert!(context.starts_with("ANSWER PROCESSOR DOCS"), "{context}");
        assert!(context.contains("## Answers awaiting processing"), "{context}");
        assert_eq!(processor.message, "# Turn\n\nProcess: q-1.");
        assert_eq!(live_sessions(&fx, Role::QuestionMaker).len(), 1);
    }

    #[test]
    fn a_second_answer_processor_starts_when_the_backlog_exceeds_half_the_target() {
        let fx = fixture();
        fx.exhaust();
        for n in 1..=3 {
            let seq = fx.question(ACTOR_USER, draft(&format!("Question {n}?")));
            fx.answer(seq, None, Some("an answer"));
        }
        let mut agent = FakeAgent::default();
        driver(&fx, 100_000).tick(&fx.fleet, &mut agent);

        let messages: Vec<&str> = agent.turns.iter().map(|t| t.message.as_str()).collect();
        assert_eq!(
            messages,
            ["# Turn\n\nProcess: q-1, q-2.", "# Turn\n\nProcess: q-3."],
            "backlog 3 > half of target 4: split across two lanes"
        );
        assert!(
            agent
                .turns
                .iter()
                .all(|t| t.purpose == SessionPurpose::AnswerProcessor)
        );
        assert_ne!(actor(&agent.turns[0]), actor(&agent.turns[1]));
    }

    #[test]
    fn a_small_backlog_stays_on_one_answer_processor() {
        let fx = fixture();
        fx.exhaust();
        for n in 1..=2 {
            let seq = fx.question(ACTOR_USER, draft(&format!("Question {n}?")));
            fx.answer(seq, None, Some("an answer"));
        }
        let mut agent = FakeAgent::default();
        driver(&fx, 100_000).tick(&fx.fleet, &mut agent);
        let messages: Vec<&str> = agent.turns.iter().map(|t| t.message.as_str()).collect();
        assert_eq!(messages, ["# Turn\n\nProcess: q-1, q-2."]);
    }

    #[test]
    fn a_session_over_budget_rotates_to_a_fresh_snapshot() {
        let fx = fixture();
        let mut agent = FakeAgent::default();
        // Any opening snapshot exceeds a 10-token budget.
        let mut driver = driver(&fx, 10);

        driver.tick(&fx.fleet, &mut agent);
        let first = actor(&agent.turns[0]);
        fx.question(&first, draft("A question?"));

        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(agent.turns.len(), 2);
        let second = &agent.turns[1];
        assert_ne!(actor(second), first, "a new session");
        assert!(second.opening.is_some(), "the new session gets a fresh snapshot");
        assert_eq!(agent.closed, [format!("interview-{first}")]);
        let live = live_sessions(&fx, Role::QuestionMaker);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id.to_string(), actor(second));
    }

    #[test]
    fn stale_proposals_are_withdrawn_and_the_question_maker_asked_for_more() {
        let fx = fixture();
        let target = fx.obligation("Old wording.");
        let mut question = draft("Reword it?");
        question.proposal = Some(Proposal {
            op: ProposalOp::Update,
            kind: None,
            section: None,
            node: None,
            id: Some(target.to_string()),
            content_type: None,
            text: Some("New wording.".into()),
            append: false,
            replaces: Vec::new(),
        });
        fx.question(ACTOR_USER, question);
        fx.exhaust();
        fx.outline(OutlineMutation::DeleteObligation {
            obligation_id: target,
        });

        let mut agent = FakeAgent::default();
        driver(&fx, 100_000).tick(&fx.fleet, &mut agent);

        let q = fx.get_question(1);
        assert_eq!(q.status, STATUS_WITHDRAWN);
        assert_eq!(q.withdrawn_by, None);
        assert_eq!(q.withdrawn_reason.as_deref(), Some(STALE_PROPOSAL_REASON));
        assert_eq!(agent.turns.len(), 1);
        assert_eq!(agent.turns[0].purpose, SessionPurpose::QuestionMaker);
        let state = fx
            .fleet
            .read(|conn| InterviewRepo::new(conn).question_maker_state(fx.session))
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(state, QUESTION_MAKER_IDLE, "exhaustion cleared for the replacement");
    }

    #[test]
    fn answers_left_unprocessed_are_reported_and_not_retried_at_once() {
        let fx = fixture();
        fx.exhaust();
        let seq = fx.question(ACTOR_USER, draft("A question?"));
        fx.answer(seq, None, Some("an answer"));
        let mut agent = FakeAgent::default();
        let mut driver = driver(&fx, 100_000);

        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(agent.turns.len(), 1);
        // The turn ends without the agent marking q-1 processed.
        let events = driver.tick(&fx.fleet, &mut agent);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DriverEvent::AnswersFinished { error: Some(err), .. }
                    if err.contains("left q-1 unprocessed")
            )),
            "{events:?}"
        );
        assert_eq!(agent.turns.len(), 1, "backoff holds the retry");
        assert!(driver.status().last_error.is_some());
    }
}
