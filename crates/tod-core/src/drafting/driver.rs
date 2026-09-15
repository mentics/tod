//! Runs one node's drafter: decides when it takes a turn and which agent
//! session the turn goes to.
//!
//! A turn is due when the user dumped something at the node, resolved a
//! choice, asked for a rewrite of pre-v3 obligations, or when someone else
//! changed what the drafter works from and then paused. Sessions follow the
//! interview's rules: docs and a snapshot once, then only changes; rotate
//! when over budget or cold.
//!
//! Before a turn, the driver has every ancestor summary the drafter's context
//! needs written (`drafting::summary`); the turn waits for them.

use crate::drafting::DraftingMode;
use crate::drafting::context::snapshot;
use crate::drafting::summary;
use crate::interview::context::{ContextScope, delta, estimate_tokens};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tod_agent::{
    AgentLaunchOptions, AgentProvider, AgentRunState, RunId, SessionOpening, SessionPurpose,
    SessionTurn,
};
use tod_store::drafting::*;
use tod_store::fleet::FleetStore;
use tod_store::interview::*;
use tod_store::outline::{EXTRA_CONTENT_SUMMARY, OUTCOME_PENDING, OutlineMutation, ancestor_chain};
use tod_store::settings::InterviewContextSettings;
use uuid::Uuid;

/// Failed turns in a row before the driver waits for a manual retry.
const MAX_FAILURES: u32 = 3;
/// How long others' changes must sit still before the drafter reacts to them.
const QUIET_PERIOD: Duration = Duration::from_secs(3);

pub struct DraftingConfig {
    pub node_id: Uuid,
    pub node_title: String,
    pub mode: DraftingMode,
    pub agent_config_id: String,
    /// Working directory for the drafting loop (the node's repo).
    pub repo_cwd: PathBuf,
    pub data_root: PathBuf,
    pub tod_cli: PathBuf,
    pub launch: AgentLaunchOptions,
    pub context: InterviewContextSettings,
    /// Byte-stable docs every session opens with (`drafting_session_prefix`).
    pub prefix: String,
    /// Start a round unprompted when buildable was never recorded — for a node
    /// that is in `design`, not one merely having obligations rewritten.
    pub kickoff: bool,
}

struct Turn {
    run: RunId,
    agent_session: Uuid,
    key: String,
    chars_at_start: u64,
    dumps: Vec<i64>,
    choices: Vec<i64>,
    rewrite: bool,
}

/// An agent writing one ancestor's summary.
struct SummaryRun {
    run: RunId,
    node_id: Uuid,
    title: String,
    key: String,
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
pub struct DraftingStatus {
    pub running: bool,
    /// Titles of the ancestors whose summaries are being written for the next turn.
    pub summarizing: Vec<String>,
    pub rewrite_pending: bool,
    pub last_error: Option<String>,
    /// Turns failed repeatedly; the driver waits for [`DraftingDriver::retry`].
    pub manual_required: bool,
}

impl DraftingStatus {
    /// A turn or the summaries it waits on are in flight.
    pub fn busy(&self) -> bool {
        self.running || !self.summarizing.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftingEvent {
    TurnFinished { error: Option<String> },
}

/// Why a turn is being started.
struct Due {
    dumps: Vec<DraftingDump>,
    choices: Vec<DraftingChoice>,
    rewrite: bool,
    kickoff: bool,
}

pub struct DraftingDriver {
    config: DraftingConfig,
    turn: Option<Turn>,
    summaries: Vec<SummaryRun>,
    backoff: Backoff,
    rewrite_requested: bool,
    kickoff_checked: bool,
    /// Newest change by someone else, and when it was first seen.
    pending_changes: Option<(i64, Instant)>,
    manual_required: bool,
    last_error: Option<String>,
}

impl DraftingDriver {
    pub fn new(config: DraftingConfig) -> Self {
        Self {
            config,
            turn: None,
            summaries: Vec::new(),
            backoff: Backoff::default(),
            rewrite_requested: false,
            kickoff_checked: false,
            pending_changes: None,
            manual_required: false,
            last_error: None,
        }
    }

    pub fn config(&self) -> &DraftingConfig {
        &self.config
    }

    pub fn status(&self) -> DraftingStatus {
        DraftingStatus {
            running: self.turn.is_some(),
            summarizing: self.summaries.iter().map(|s| s.title.clone()).collect(),
            rewrite_pending: self.rewrite_requested,
            last_error: self.last_error.clone(),
            manual_required: self.manual_required,
        }
    }

    /// Clear failure state so the drafter runs again.
    pub fn retry(&mut self) {
        self.manual_required = false;
        self.backoff.reset();
        self.last_error = None;
    }

    /// Ask for the node's pre-v3 obligations to be rewritten on the next turn.
    pub fn request_rewrite(&mut self) {
        self.rewrite_requested = true;
    }

    /// Stop the turn in flight, and any summaries it waits on.
    pub fn cancel(&mut self, agent: &mut dyn AgentProvider) {
        if let Some(turn) = self.turn.take() {
            let _ = agent.cancel_run(turn.run);
        }
        for run in self.summaries.drain(..) {
            let _ = agent.cancel_run(run.run);
            agent.close_session(&run.key);
            summary::release(run.node_id);
        }
    }

    /// Advance: collect a finished turn, then start one if it is due.
    pub fn tick(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider) -> Vec<DraftingEvent> {
        let mut events = Vec::new();
        self.poll_summaries(fleet, agent, &mut events);
        self.poll(fleet, agent, &mut events);
        if let Err(err) = self.schedule(fleet, agent) {
            self.backoff.fail();
            self.last_error = Some(format!("{err:#}"));
        }
        events
    }

    /// Store each finished summary. A failure counts like a failed turn, since
    /// the turn cannot start without it.
    fn poll_summaries(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        events: &mut Vec<DraftingEvent>,
    ) {
        let mut i = 0;
        while i < self.summaries.len() {
            let outcome = match agent.poll_run(self.summaries[i].run) {
                Some(AgentRunState::InFlight(_)) | Some(AgentRunState::NeedsPermission(_)) => {
                    i += 1;
                    continue;
                }
                Some(AgentRunState::Success(reply)) => reply
                    .as_deref()
                    .and_then(summary::parse)
                    .ok_or_else(|| anyhow::anyhow!("the reply had no summary")),
                Some(AgentRunState::Failure(message)) => Err(anyhow::anyhow!(message)),
                None => Err(anyhow::anyhow!("agent run was lost")),
            };
            let run = self.summaries.remove(i);
            agent.close_session(&run.key);
            let stored = outcome.and_then(|body| {
                fleet.interview(
                    ACTOR_AGENT,
                    InterviewCommand::Outline {
                        mutation: OutlineMutation::SetExtraContent {
                            node_id: run.node_id,
                            content_type: EXTRA_CONTENT_SUMMARY.into(),
                            body,
                        },
                        target: None,
                    },
                )?;
                Ok(())
            });
            summary::release(run.node_id);
            if let Err(err) = stored {
                self.backoff.fail();
                if self.backoff.failures >= MAX_FAILURES {
                    self.manual_required = true;
                }
                let error = format!("Summarizing \"{}\" failed: {err:#}", run.title);
                self.last_error = Some(error.clone());
                events.push(DraftingEvent::TurnFinished { error: Some(error) });
            }
        }
    }

    fn poll(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider, events: &mut Vec<DraftingEvent>) {
        let Some(turn) = &self.turn else {
            return;
        };
        let outcome = match agent.poll_run(turn.run) {
            Some(AgentRunState::InFlight(_)) | Some(AgentRunState::NeedsPermission(_)) => return,
            Some(AgentRunState::Success(reply)) => Ok(reply),
            Some(AgentRunState::Failure(message)) => Err(message),
            None => Err("agent run was lost".to_string()),
        };
        let turn = self.turn.take().expect("checked above");
        let error = match self.finish_turn(fleet, agent, &turn, outcome) {
            Ok(error) => error,
            Err(err) => Some(format!("{err:#}")),
        };
        self.last_error = error.clone();
        events.push(DraftingEvent::TurnFinished { error });
    }

    fn finish_turn(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        turn: &Turn,
        outcome: std::result::Result<Option<String>, String>,
    ) -> Result<Option<String>> {
        let chars = agent
            .session_context_chars(&turn.key)
            .unwrap_or(turn.chars_at_start);
        let added_tokens = (chars.saturating_sub(turn.chars_at_start) / 4) as i64;
        let row = fleet
            .read(|conn| InterviewRepo::new(conn).get_agent_session(turn.agent_session))?
            .context("drafter session vanished")?;
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::RecordAgentTurn {
                id: turn.agent_session,
                agent_session_id: agent.session_id(&turn.key),
                synced_rev: None,
                est_tokens: Some(row.est_tokens + added_tokens),
                turn_completed: outcome.is_ok(),
            },
        )?;
        match outcome {
            Ok(reply) => {
                self.backoff.reset();
                if turn.rewrite {
                    self.rewrite_requested = false;
                }
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::RecordDraftingTurn {
                        node_id: self.config.node_id,
                        summary: reply.as_deref().and_then(change_summary),
                        dump_seqs: turn.dumps.clone(),
                        choice_seqs: turn.choices.clone(),
                    },
                )?;
                Ok(None)
            }
            Err(message) => {
                self.backoff.fail();
                if self.backoff.failures >= MAX_FAILURES {
                    self.manual_required = true;
                }
                // A session whose turns keep failing may be broken; start fresh.
                if self.backoff.failures >= 2 {
                    self.retire(fleet, agent, turn.agent_session)?;
                }
                Ok(Some(message))
            }
        }
    }

    fn schedule(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider) -> Result<()> {
        if self.turn.is_some()
            || !self.summaries.is_empty()
            || self.manual_required
            || !self.backoff.ready()
        {
            return Ok(());
        }
        let node = self.config.node_id;
        let phase = self.config.mode.phase();
        let (dumps, choices, session, buildable, changed_rev) = fleet.read(|conn| {
            let drafting = DraftingRepo::new(conn);
            let interview = InterviewRepo::new(conn);
            let session = interview
                .live_agent_sessions(node, phase, Role::Drafter)?
                .into_iter()
                .next();
            let changed_rev = match &session {
                Some(s) => interview
                    .changes_since(&ancestor_chain(conn, node)?, s.synced_rev, &s.id.to_string())?
                    .into_iter()
                    .filter(|c| c.entity == ENTITY_OBLIGATION || c.entity == ENTITY_CONTENT)
                    .filter(|c| c.fields != ["provenance"])
                    .map(|c| c.rev)
                    .max(),
                None => None,
            };
            Ok((
                drafting.unrouted_dumps(node)?,
                drafting.unprocessed_choices(node)?,
                session,
                drafting.buildable(node)?,
                changed_rev,
            ))
        })?;

        let quiet_changes = match (changed_rev, self.pending_changes) {
            (None, _) => {
                self.pending_changes = None;
                false
            }
            (Some(rev), Some((seen, since))) if seen == rev => since.elapsed() >= QUIET_PERIOD,
            (Some(rev), _) => {
                self.pending_changes = Some((rev, Instant::now()));
                false
            }
        };
        // Entering the drafting loop starts a round of its own, once per driver.
        let kickoff = !self.kickoff_checked
            && self.config.kickoff
            && match &buildable {
                None => true,
                Some(eval) => session.is_none() && eval.outcome == OUTCOME_PENDING,
            };

        let due = Due {
            dumps,
            choices,
            rewrite: self.rewrite_requested,
            kickoff,
        };
        if due.dumps.is_empty() && due.choices.is_empty() && !due.rewrite && !due.kickoff && !quiet_changes {
            self.kickoff_checked = true;
            return Ok(());
        }
        // Ancestors' requirements reach the drafter only as their summaries, so
        // the turn waits until every one it needs is written. Nothing is taken
        // off the due list meanwhile: the next tick finds the same work.
        let missing = fleet.read(|conn| summary::missing(conn, node, Some(phase)))?;
        if !missing.is_empty() {
            return self.start_summaries(fleet, agent, missing);
        }
        self.kickoff_checked = true;
        self.pending_changes = None;
        self.start_turn(fleet, agent, due)
    }

    /// Ask an agent for each missing summary no other driver is already writing.
    fn start_summaries(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        missing: Vec<Uuid>,
    ) -> Result<()> {
        let cwd = self.scratch_dir()?;
        for node_id in missing {
            if !summary::claim(node_id) {
                continue;
            }
            let started = fleet
                .read(|conn| summary::request(conn, node_id))
                .and_then(|(title, message)| {
                    let key = format!("summary-{node_id}-{}", Uuid::new_v4());
                    let handle = agent.send_session_turn(SessionTurn {
                        key: key.clone(),
                        agent_config_id: self.config.agent_config_id.clone(),
                        cwd: cwd.clone(),
                        options: self.config.launch.clone(),
                        resume_session_id: None,
                        opening: Some(SessionOpening {
                            title: format!("{title} · summary"),
                            context: None,
                        }),
                        message,
                        purpose: SessionPurpose::Summarizer,
                        env: Vec::new(),
                    })?;
                    Ok(SummaryRun {
                        run: handle.id,
                        node_id,
                        title,
                        key,
                    })
                });
            match started {
                Ok(run) => self.summaries.push(run),
                Err(err) => {
                    summary::release(node_id);
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    fn instruction(&self, due: &Due) -> Result<String> {
        let mut out = String::new();
        if !due.dumps.is_empty() {
            out.push_str("## New dumps\n\nWhat the user said, oldest first. Route every piece to where it belongs.\n");
            for dump in &due.dumps {
                write!(out, "\n### {}\n\n{}\n", dump.label(), dump.body.trim())?;
            }
        }
        if !due.choices.is_empty() {
            out.push_str("\n## Choices resolved\n\n");
            for c in &due.choices {
                match (c.status.as_str(), c.answer) {
                    (CHOICE_ANSWERED, Some(n)) => {
                        let label = c
                            .options
                            .get((n - 1).max(0) as usize)
                            .map(|o| o.label.as_str())
                            .unwrap_or("");
                        writeln!(
                            out,
                            "- {} \"{}\": the user picked {n}. {label}. Its obligations are already written as the user's.",
                            c.label(),
                            c.question
                        )?;
                    }
                    _ => {
                        let options: Vec<String> = c
                            .options
                            .iter()
                            .enumerate()
                            .map(|(i, o)| format!("{}. {}", i + 1, o.label))
                            .collect();
                        writeln!(
                            out,
                            "- {} \"{}\": the user said You pick. Write your best call as agent obligations ({}).",
                            c.label(),
                            c.question,
                            options.join(" | ")
                        )?;
                    }
                }
            }
        }
        if due.rewrite {
            write!(
                out,
                "\n## Rewrite pre-v3 obligations\n\n\
                 Rewrite this node's obligations marked \"{PRE_V3_ATTENTION_WHY}\" into the smallest \
                 set of powerful obligations: merge, reword, move, or delete what is already covered. \
                 Score attention on every agent obligation you keep, so none keeps that reason.\n\n\
                 Never lose content: write each replacement first, check `obligations list` shows \
                 its full text, and only then delete the obligations it covers. Never delete an \
                 obligation you have not read.\n"
            )?;
        }
        if due.kickoff {
            out.push_str(
                "\n## Start\n\nThe node is in the drafting loop. Research, draft what is missing, and record buildable.\n",
            );
        }
        if out.is_empty() {
            out.push_str(
                "## Changes by others\n\nTake the changes above into account: generalize the user's corrections, \
                 repair what they made wrong, and record buildable again.\n",
            );
        }
        write!(out, "\nEnd the turn with the change summary inside {SUMMARY_OPEN} tags.")?;
        Ok(out.trim_start().to_string())
    }

    fn retire(&self, fleet: &FleetStore, agent: &mut dyn AgentProvider, id: Uuid) -> Result<()> {
        agent.close_session(&session_key(id));
        fleet.interview(ACTOR_USER, InterviewCommand::RetireAgentSession { id })?;
        Ok(())
    }

    fn start_turn(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider, due: Due) -> Result<()> {
        let node = self.config.node_id;
        let phase = self.config.mode.phase();
        let now_ms = tod_store::outline::now_ms();
        let existing = fleet
            .read(|conn| InterviewRepo::new(conn).live_agent_sessions(node, phase, Role::Drafter))?
            .into_iter()
            .next();
        let budget = self.config.context.context_budget_tokens as i64;
        let idle_ms = self.config.context.prompt_cache_idle_minutes as i64 * 60_000;
        let reusable = existing.as_ref().is_some_and(|s| {
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

        let data_root = self.config.data_root.clone();
        let tod_cli = self.config.tod_cli.clone();
        let scope = ContextScope {
            node_id: node,
            phase,
            role: Role::Drafter,
            interview_session_id: None,
            data_root: &data_root,
            tod_cli: &tod_cli,
            answered_cap: self.config.context.answered_history_cap as usize,
        };
        let only_changes =
            due.dumps.is_empty() && due.choices.is_empty() && !due.rewrite && !due.kickoff;
        let instruction = self.instruction(&due)?;

        let (session_id, opening, resume, message) = match existing.filter(|_| reusable) {
            Some(row) => {
                let (rev, changes) = fleet.read(|conn| {
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
                // Nothing the drafter works from actually changed (e.g. only an
                // ancestor's requirement, which its summary already stands in for).
                if only_changes && changes.is_empty() {
                    return Ok(());
                }
                let message = if changes.is_empty() {
                    format!("# Turn\n\n{instruction}")
                } else {
                    format!("{changes}\n# Turn\n\n{instruction}")
                };
                (row.id, None, row.agent_session_id.clone(), message)
            }
            None => {
                let id = Uuid::new_v4();
                let mode = self.config.mode;
                let (rev, state) = fleet.read(|conn| {
                    let state = snapshot(conn, &scope, mode)?;
                    let rev = InterviewRepo::new(conn).head_rev()?;
                    Ok((rev, state))
                })?;
                let context = format!("{}\n\n{state}", self.config.prefix.trim_end());
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::CreateAgentSession {
                        id,
                        node_id: node,
                        interview_session_id: None,
                        phase: phase.to_string(),
                        role: Role::Drafter,
                        lane: 0,
                        synced_rev: rev,
                        snapshot_tokens: estimate_tokens(&context),
                    },
                )?;
                let title = format!(
                    "{} · drafter · {}",
                    self.config.node_title,
                    self.config.mode.label().to_lowercase()
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
            agent_config_id: self.config.agent_config_id.clone(),
            cwd: self.cwd()?,
            options: self.config.launch.clone(),
            resume_session_id: resume,
            opening,
            message,
            purpose: SessionPurpose::Drafter,
            env: vec![(ACTOR_ENV.to_string(), session_id.to_string())],
        })?;
        self.turn = Some(Turn {
            run: handle.id,
            agent_session: session_id,
            key,
            chars_at_start,
            dumps: due.dumps.iter().map(|d| d.seq).collect(),
            choices: due.choices.iter().map(|c| c.seq).collect(),
            rewrite: due.rewrite,
        });
        Ok(())
    }

    /// Capture runs in an empty directory so the agent does not load a
    /// repository's instructions it has no use for; drafting runs in the repo.
    fn cwd(&self) -> Result<PathBuf> {
        if self.config.mode == DraftingMode::Capture {
            return self.scratch_dir();
        }
        Ok(self.config.repo_cwd.clone())
    }

    /// An empty directory for agents that need nothing from a repository.
    fn scratch_dir(&self) -> Result<PathBuf> {
        let dir = self.config.data_root.join("agent").join("drafting");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(dir)
    }
}

fn session_key(id: Uuid) -> String {
    format!("drafting-{id}")
}

/// Delimits the change summary in a drafter's reply (see `drafting/base.md`).
pub(crate) const SUMMARY_OPEN: &str = "<change-summary>";
pub(crate) const SUMMARY_CLOSE: &str = "</change-summary>";

/// The change summary out of a turn's whole reply.
fn change_summary(reply: &str) -> Option<String> {
    tagged_block(reply, SUMMARY_OPEN, SUMMARY_CLOSE)
}

/// The text between `open` and `close` in an agent's whole reply. A provider
/// hands back every message of the turn run together, narration between tool
/// calls included, so only the last delimited block counts. An unclosed block
/// runs to the end; a reply with no block falls back to its last paragraph.
pub(crate) fn tagged_block(reply: &str, open: &str, close: &str) -> Option<String> {
    let body = match reply.rfind(open) {
        Some(start) => {
            let rest = &reply[start + open.len()..];
            rest.find(close).map_or(rest, |end| &rest[..end])
        }
        None => reply.trim().rsplit("\n\n").next().unwrap_or_default(),
    };
    let mut body = body.trim();
    // The docs show the summary fenced; a drafter may fence it inside the tags too.
    if let Some(fenced) = body.strip_prefix("```") {
        body = fenced.split_once('\n').map_or("", |(_, rest)| rest);
        body = body.trim_end().strip_suffix("```").unwrap_or(body).trim();
    }
    (!body.is_empty()).then(|| body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafting::mock::Direct;
    use crate::interview::test_support::{Fixture, fixture};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tod_agent::agent_traffic::InterviewAgentCounts;
    use tod_agent::{AgentPlatform, AgentRunHandle};
    use tod_store::outline::{KIND_REQUIREMENT, OutlineMutation};

    /// Plays the mock drafter synchronously against the fixture's open store;
    /// every run finishes at once.
    struct MockDrafter {
        fleet: Arc<FleetStore>,
        turns: Vec<SessionTurn>,
        runs: HashMap<RunId, AgentRunState>,
        sessions: HashMap<String, String>,
    }

    impl AgentProvider for MockDrafter {
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
            if turn.purpose == SessionPurpose::Summarizer {
                let reply = summary::mock_summarizer(&turn.message)?;
                self.runs.insert(id, AgentRunState::Success(Some(reply)));
                self.turns.push(turn);
                return Ok(AgentRunHandle { id });
            }
            let actor = turn
                .env
                .iter()
                .find(|(k, _)| k == ACTOR_ENV)
                .map(|(_, v)| v.clone())
                .unwrap();
            let row = self
                .fleet
                .read(|conn| InterviewRepo::new(conn).get_agent_session(Uuid::parse_str(&actor)?))
                .unwrap()
                .unwrap();
            let client = Direct {
                fleet: &self.fleet,
                actor,
            };
            let text = turn.prompt_blocks().join("\n\n");
            let state = match crate::drafting::mock::drafter(&client, &row, &text) {
                Ok(reply) => AgentRunState::Success(Some(reply)),
                Err(err) => AgentRunState::Failure(format!("{err:#}")),
            };
            self.sessions
                .entry(turn.key.clone())
                .or_insert_with(|| format!("agent-side-{}", turn.key));
            self.runs.insert(id, state);
            self.turns.push(turn);
            Ok(AgentRunHandle { id })
        }

        fn session_id(&self, key: &str) -> Option<String> {
            self.sessions.get(key).cloned()
        }

        fn session_context_chars(&self, _: &str) -> Option<u64> {
            Some(0)
        }

        fn close_session(&mut self, key: &str) {
            self.sessions.remove(key);
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

    fn driver(fx: &Fixture, mode: DraftingMode) -> (DraftingDriver, MockDrafter) {
        let driver = DraftingDriver::new(DraftingConfig {
            node_id: fx.node,
            node_title: "Drafting node".into(),
            mode,
            agent_config_id: "config".into(),
            repo_cwd: fx.root.clone(),
            data_root: fx.root.clone(),
            tod_cli: fx.root.join("tod-cli"),
            launch: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
            context: InterviewContextSettings::default(),
            prefix: "DRAFTER DOCS".into(),
            kickoff: mode == DraftingMode::Drafting,
        });
        let agent = MockDrafter {
            fleet: fx.fleet.clone(),
            turns: Vec::new(),
            runs: HashMap::new(),
            sessions: HashMap::new(),
        };
        (driver, agent)
    }

    fn marked(fx: &Fixture) -> Vec<MarkedObligation> {
        fx.fleet
            .read(|conn| DraftingRepo::new(conn).marked_obligations(fx.node))
            .unwrap()
    }

    #[test]
    fn a_dump_becomes_agent_obligations_and_a_change_summary() {
        let fx = fixture();
        let (mut driver, mut agent) = driver(&fx, DraftingMode::Capture);
        driver.tick(&fx.fleet, &mut agent);
        assert!(agent.turns.is_empty(), "capture waits for a dump");

        fx.user(InterviewCommand::AddDump {
            node_id: Some(fx.node),
            body: "Notes have a Markdown preview. Notes sync.".into(),
        });
        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(agent.turns.len(), 1);
        let turn = &agent.turns[0];
        assert_eq!(turn.purpose, SessionPurpose::Drafter);
        let context = turn.opening.as_ref().unwrap().context.as_deref().unwrap();
        assert!(context.starts_with("DRAFTER DOCS"), "{context}");
        assert!(context.contains("# Drafting state"), "{context}");
        assert!(turn.message.contains("### d-1"), "{}", turn.message);

        // Finish the turn: the dump is taken in and the summary stored.
        let events = driver.tick(&fx.fleet, &mut agent);
        assert_eq!(events, [DraftingEvent::TurnFinished { error: None }]);
        let rows = marked(&fx);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|m| m.mark.is_agent() && m.mark.attention.is_some()));
        let (unrouted, summaries) = fx
            .fleet
            .read(|conn| {
                let repo = DraftingRepo::new(conn);
                Ok((repo.unrouted_dumps(fx.node)?, repo.recent_summaries(fx.node, 5)?))
            })
            .unwrap();
        assert!(unrouted.is_empty());
        assert!(summaries[0].body.contains("Not mentioned yet"), "{}", summaries[0].body);
        assert!(!summaries[0].body.contains("Mock:"), "narration is dropped: {}", summaries[0].body);
        assert!(!summaries[0].body.contains(SUMMARY_OPEN), "{}", summaries[0].body);
        assert_eq!(agent.turns.len(), 1, "nothing else is due");
    }

    #[test]
    fn only_the_change_summary_is_kept_from_a_narrated_reply() {
        let reply = "Let me verify the current state of obligations:Excellent - both are in.\
                     Let me check the drafting state more directly:\
                     <change-summary>\nSettings panel   + 2 requirements\n1 choice waiting on Settings panel\n</change-summary>";
        assert_eq!(
            change_summary(reply).as_deref(),
            Some("Settings panel   + 2 requirements\n1 choice waiting on Settings panel")
        );
        // An earlier block (e.g. a draft) loses to the last one; text after it is dropped.
        assert_eq!(
            change_summary("<change-summary>draft</change-summary> more work <change-summary>No changes.</change-summary> done").as_deref(),
            Some("No changes.")
        );
        assert_eq!(
            change_summary("Checking:<change-summary>\n```text\nApp  + constraint\n```\n</change-summary>").as_deref(),
            Some("App  + constraint"),
            "a fence inside the tags is unwrapped"
        );
        assert_eq!(change_summary("Checking:<change-summary>\nApp  + constraint").as_deref(), Some("App  + constraint"));
        assert_eq!(
            change_summary("Let me look around.\n\nSettings panel  + 1 requirement\n").as_deref(),
            Some("Settings panel  + 1 requirement"),
            "without tags, the last paragraph"
        );
        assert_eq!(change_summary("Narration:<change-summary> </change-summary>"), None);
        assert_eq!(change_summary("  \n"), None);
    }

    #[test]
    fn user_edits_and_confirmations_set_provenance_and_reset_buildable() {
        let fx = fixture();
        let (mut driver, mut agent) = driver(&fx, DraftingMode::Drafting);
        fx.user(InterviewCommand::AddDump {
            node_id: Some(fx.node),
            body: "The panel supports keyboard navigation.".into(),
        });
        driver.tick(&fx.fleet, &mut agent);
        driver.tick(&fx.fleet, &mut agent);
        let buildable = |fx: &Fixture| {
            fx.fleet
                .read(|conn| DraftingRepo::new(conn).buildable(fx.node))
                .unwrap()
                .map(|e| e.outcome)
        };
        assert_eq!(buildable(&fx).as_deref(), Some("pass"));
        let drafted = marked(&fx)[0].obligation.id;

        fx.user(InterviewCommand::ConfirmObligation {
            obligation_id: drafted,
        });
        assert_eq!(marked(&fx)[0].mark.provenance, PROVENANCE_USER);
        assert_eq!(buildable(&fx).as_deref(), Some("pass"), "confirming changes no meaning");

        fx.outline(tod_store::outline::OutlineMutation::UpdateObligationBody {
            obligation_id: drafted,
            body: "Every panel supports keyboard navigation.".into(),
        });
        assert_eq!(buildable(&fx).as_deref(), Some("pending"), "an edit resets buildable");

        // Agents cannot confirm.
        let err = fx
            .act(ACTOR_AGENT, InterviewCommand::ConfirmObligation { obligation_id: drafted })
            .unwrap_err();
        assert!(err.to_string().contains("only the user"), "{err}");
    }

    #[test]
    fn a_question_in_a_dump_becomes_a_choice_and_picking_applies_it_as_the_user() {
        let fx = fixture();
        let (mut driver, mut agent) = driver(&fx, DraftingMode::Drafting);
        fx.user(InterviewCommand::AddDump {
            node_id: Some(fx.node),
            body: "Should settings sync across machines?".into(),
        });
        driver.tick(&fx.fleet, &mut agent);
        driver.tick(&fx.fleet, &mut agent);
        let open = fx
            .fleet
            .read(|conn| DraftingRepo::new(conn).list_choices(fx.node, &[CHOICE_OPEN]))
            .unwrap();
        assert_eq!(open.len(), 1);
        let err = fx
            .act(
                ACTOR_AGENT,
                InterviewCommand::SetBuildable {
                    node_id: fx.node,
                    outcome: "pass".into(),
                    detail: None,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("choices are open"), "{err}");

        fx.user(InterviewCommand::AnswerChoice {
            node_id: fx.node,
            seq: open[0].seq,
            option: Some(1),
        });
        let rows = marked(&fx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mark.provenance, PROVENANCE_USER);

        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(agent.turns.len(), 2, "the resolved choice goes back to the drafter");
        assert!(agent.turns[1].message.contains("the user picked 1"), "{}", agent.turns[1].message);
    }

    #[test]
    fn missing_ancestor_summaries_are_written_before_the_turn() {
        let fx = fixture();
        fx.obligation("Ancestor requirement nobody below should see in full.");
        let child = Uuid::new_v4();
        fx.outline(OutlineMutation::CreateNode {
            node_id: Some(child),
            list_id: fx.fleet.list_outline_lists().unwrap()[0].id,
            parent_id: Some(fx.node),
            anchor_id: None,
            position: tod_store::outline::CreatePosition::Child,
            title: "Child".into(),
        });
        let inherited = |fx: &Fixture| {
            fx.fleet
                .read(|conn| {
                    crate::interview::context::render_inherited_context(
                        conn,
                        &tod_store::outline::repos::NodeRepo::new(conn),
                        child,
                        None,
                    )
                })
                .unwrap()
        };
        let before = inherited(&fx);
        assert!(!before.contains("nobody below"), "requirements are never listed: {before}");
        assert!(before.contains("No summary yet"), "{before}");

        let (mut driver, mut agent) = driver(&fx, DraftingMode::Capture);
        driver.config.node_id = child;
        fx.user(InterviewCommand::AddDump {
            node_id: Some(child),
            body: "Children list their parent.".into(),
        });
        driver.tick(&fx.fleet, &mut agent);
        assert_eq!(agent.turns.len(), 1);
        assert_eq!(agent.turns[0].purpose, SessionPurpose::Summarizer);
        assert!(agent.turns[0].message.contains("nobody below"), "{}", agent.turns[0].message);
        assert_eq!(driver.status().summarizing, ["Interview node"]);

        // The summary lands, then the turn it waited on starts.
        assert!(driver.tick(&fx.fleet, &mut agent).is_empty());
        assert_eq!(agent.turns.len(), 2);
        assert_eq!(agent.turns[1].purpose, SessionPurpose::Drafter);
        let context = agent.turns[1].opening.as_ref().unwrap().context.as_deref().unwrap();
        assert!(context.contains("Mock summary of Interview node"), "{context}");
        assert!(!context.contains("nobody below"), "{context}");
        assert!(!context.contains("No summary yet"), "{context}");
        assert!(driver.status().summarizing.is_empty());
    }

    #[test]
    fn a_rewrite_reworks_pre_v3_obligations() {
        let fx = fixture();
        // Stand in for the migration: an agent obligation marked pre-v3.
        let id = Uuid::new_v4();
        fx.act(
            ACTOR_AGENT,
            InterviewCommand::Outline {
                mutation: OutlineMutation::CreateObligation {
                    obligation_id: Some(id),
                    node_id: fx.node,
                    kind: KIND_REQUIREMENT.into(),
                    after_id: None,
                    before: false,
                    section: None,
                    body: "old   style obligation".into(),
                    phase: PHASE_REQUIREMENTS.into(),
                },
                target: None,
            },
        )
        .unwrap();
        fx.act(
            ACTOR_AGENT,
            InterviewCommand::SetAttention {
                obligation_id: id,
                attention: ATTENTION_MEDIUM.into(),
                why: Some(PRE_V3_ATTENTION_WHY.into()),
            },
        )
        .unwrap();
        let (mut driver, mut agent) = driver(&fx, DraftingMode::Capture);
        driver.request_rewrite();
        driver.tick(&fx.fleet, &mut agent);
        driver.tick(&fx.fleet, &mut agent);
        assert!(!driver.status().rewrite_pending);
        let rows = marked(&fx);
        assert_eq!(rows[0].obligation.id, id);
        assert!(!rows[0].mark.is_pre_v3(), "{:?}", rows[0].mark);
        assert!(rows[0].mark.is_agent());
    }
}
