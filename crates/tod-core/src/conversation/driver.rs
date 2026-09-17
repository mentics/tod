//! Runs one conversation's agent: sends each user message to the
//! conversation's agent session, and records the reply.
//!
//! The first message carries the opening context; later ones carry only the
//! user's corrections since the previous turn (the delta) and the message.
//! The session is resumed by its recorded id. When it outgrows the context
//! budget, or can no longer be resumed, the driver rotates to a fresh session
//! that gets a snapshot of the conversation (D9), and the transcript shows
//! where that happened.
//!
//! Nothing runs on its own: there are no automatic turns, no backoff, and no
//! summaries. A turn starts only from [`ConversationDriver::send`].

use crate::conversation::context::{
    ReportedStale, delta, focus_selection, last_action_at, opening, resume_snapshot,
};
use crate::interview::context::estimate_tokens;
use crate::media::MediaPaths;
use crate::session_name::{CONVERSATION_SURFACE, session_name};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use tod_agent::{
    AgentLaunchOptions, AgentProvider, AgentRunState, PermissionRequest, RunId, SessionOpening,
    SessionPurpose, SessionTurn,
};
use tod_store::conversation::{
    Conversation, ConversationRepo, Focus, ReplyPart, TurnRole, actor_for, reply_answer,
};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_ENV, ACTOR_USER, InterviewCommand};
use tod_store::settings::InterviewContextSettings;
use uuid::Uuid;

/// The body of the transcript entry that marks a fresh agent session.
pub const ROTATION_NOTE: &str = "Started a fresh agent session";

#[derive(Debug, Clone)]
pub struct ConversationConfig {
    pub data_root: PathBuf,
    pub media: MediaPaths,
    pub launch: AgentLaunchOptions,
    /// `context_budget_tokens` decides when the session rotates.
    pub context: InterviewContextSettings,
}

/// What the view shows about the turn in flight.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationStatus {
    /// A turn is in flight.
    pub running: bool,
    /// What the agent is doing right now, when the provider reports it.
    pub activity: Option<String>,
    /// The agent is blocked on a permission decision; answer it through the
    /// provider (`respond_to_permission`) with this request's run.
    pub permission: Option<PermissionRequest>,
    /// The last turn's failure, until the next send.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationEvent {
    /// A turn ended: its agent (or error) turn is in the transcript.
    TurnFinished { error: Option<String> },
    /// The driver started a fresh agent session; a rotation turn is in the
    /// transcript.
    Rotated,
}

/// The turn in flight.
struct Run {
    id: RunId,
    key: String,
    /// The user turn this run answers.
    user_seq: i64,
    /// The session was resumed from a recorded id with no live process, so a
    /// failure may mean it can no longer be resumed.
    cold_resume: bool,
    /// Context characters the provider held before this turn.
    chars_at_start: u64,
}

pub struct ConversationDriver {
    config: ConversationConfig,
    focus: Focus,
    /// `None` until the first send creates the row.
    conversation_id: Option<Uuid>,
    run: Option<Run>,
    /// Estimated tokens in the current agent session; `None` until known.
    session_tokens: Option<i64>,
    reported_stale: ReportedStale,
    activity: Option<String>,
    permission: Option<PermissionRequest>,
    last_error: Option<String>,
}

impl ConversationDriver {
    /// An unsaved, empty conversation about `focus`. Its row is created by
    /// the first [`Self::send`].
    pub fn new(config: ConversationConfig, focus: Focus) -> Self {
        Self {
            config,
            focus,
            conversation_id: None,
            run: None,
            session_tokens: None,
            reported_stale: ReportedStale::new(),
            activity: None,
            permission: None,
            last_error: None,
        }
    }

    /// Continue a stored conversation.
    pub fn open(
        config: ConversationConfig,
        fleet: &FleetStore,
        conversation_id: Uuid,
    ) -> Result<Self> {
        let conversation = fleet
            .read(|conn| ConversationRepo::new(conn).get(conversation_id))?
            .with_context(|| format!("conversation {conversation_id} not found"))?;
        let mut driver = Self::new(config, conversation.focus);
        driver.conversation_id = Some(conversation_id);
        Ok(driver)
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The stored conversation, once the first send created it.
    pub fn conversation_id(&self) -> Option<Uuid> {
        self.conversation_id
    }

    pub fn config(&self) -> &ConversationConfig {
        &self.config
    }

    pub fn status(&self) -> ConversationStatus {
        ConversationStatus {
            running: self.run.is_some(),
            activity: self.activity.clone(),
            permission: self.permission.clone(),
            last_error: self.last_error.clone(),
        }
    }

    /// The provider's session key for a conversation.
    pub fn session_key(conversation_id: Uuid) -> String {
        format!("conversation-{conversation_id}")
    }

    /// Stop the turn in flight. Its user turn stays in the transcript with an
    /// error turn after it.
    pub fn cancel(&mut self, fleet: &FleetStore, agent: &mut dyn AgentProvider) -> Result<()> {
        let Some(run) = self.run.take() else {
            return Ok(());
        };
        let _ = agent.cancel_run(run.id);
        self.activity = None;
        self.permission = None;
        let id = self
            .conversation_id
            .context("a run without a conversation")?;
        self.append(fleet, id, TurnRole::Error, "Stopped")?;
        Ok(())
    }

    /// Append the user's message and start the agent's turn on it. Creates
    /// the conversation row on the first send.
    pub fn send(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        text: &str,
    ) -> Result<()> {
        if self.run.is_some() {
            bail!("the agent is still working on the previous message");
        }
        let text = text.trim();
        if text.is_empty() {
            bail!("nothing to send");
        }
        self.last_error = None;
        let id = self.ensure_row(fleet)?;
        let key = Self::session_key(id);

        // Built before the user turn is appended: the user's corrections
        // since the previous prompt, which was built right after the previous
        // user turn.
        let (conversation, prior_user_at, has_history) = fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            let conversation = repo.get(id)?.context("conversation vanished")?;
            let turns = repo.turns(id)?;
            let prior_user_at = turns
                .iter()
                .rev()
                .find(|t| t.role == TurnRole::User)
                .map(|t| t.created_at);
            let has_history =
                turns.iter().any(|t| t.role == TurnRole::Agent) || !repo.actions(id)?.is_empty();
            Ok((conversation, prior_user_at, has_history))
        })?;
        let changes = match prior_user_at {
            Some(at) => fleet.read(|conn| {
                let since = last_action_at(conn, id, at)?;
                delta(conn, id, since, &mut self.reported_stale)
            })?,
            None => String::new(),
        };
        let user_seq = self.append(fleet, id, TurnRole::User, text)?;

        let live = agent.session_id(&key).is_some();
        let resumable = live || conversation.agent_session_id.is_some();
        let budget = self.config.context.context_budget_tokens as i64;
        if resumable && self.session_tokens.is_none() {
            self.session_tokens = Some(self.estimate_from_transcript(fleet, id)?);
        }
        let over_budget = self.session_tokens.is_some_and(|t| t > budget);

        if resumable && !over_budget {
            let message = join(&changes, text);
            return self.start(
                fleet,
                agent,
                &conversation,
                user_seq,
                None,
                message,
                conversation.agent_session_id.clone().filter(|_| !live),
            );
        }
        if has_history {
            return self.rotate_and_start(fleet, agent, &conversation, user_seq, &changes, text);
        }
        // The first turn: the opening context, then the message.
        let context =
            fleet.read(|conn| opening(conn, &self.config.media, &self.config.data_root, id))?;
        self.session_tokens = Some(0);
        self.start(
            fleet,
            agent,
            &conversation,
            user_seq,
            Some(context),
            join(&changes, text),
            None,
        )
    }

    /// Collect a finished turn. Call on every poll.
    pub fn tick(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
    ) -> Vec<ConversationEvent> {
        let mut events = Vec::new();
        if let Err(err) = self.poll(fleet, agent, &mut events) {
            let error = format!("{err:#}");
            self.last_error = Some(error.clone());
            events.push(ConversationEvent::TurnFinished { error: Some(error) });
        }
        events
    }

    fn poll(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        events: &mut Vec<ConversationEvent>,
    ) -> Result<()> {
        let Some(run) = &self.run else {
            return Ok(());
        };
        let outcome = match agent.poll_run(run.id) {
            Some(AgentRunState::InFlight(activity)) => {
                self.activity = activity;
                self.permission = None;
                return Ok(());
            }
            Some(AgentRunState::NeedsPermission(request)) => {
                self.permission = Some(request);
                return Ok(());
            }
            Some(AgentRunState::Success(reply)) => Ok(reply.unwrap_or_default()),
            Some(AgentRunState::Failure(message)) => Err(message),
            None => Err("agent run was lost".to_string()),
        };
        let run = self.run.take().expect("checked above");
        self.activity = None;
        self.permission = None;
        let id = self
            .conversation_id
            .context("a run without a conversation")?;
        let chars = agent
            .session_context_chars(&run.key)
            .unwrap_or(run.chars_at_start);
        match outcome {
            Ok(reply) => {
                let parts = agent.session_reply_parts(&run.key).unwrap_or_default();
                let reply = reply.trim();
                let added = (chars.saturating_sub(run.chars_at_start) / 4) as i64;
                self.session_tokens =
                    Some(self.session_tokens.unwrap_or(0) + added.max(estimate_tokens(reply)));
                let first = fleet
                    .read(|conn| ConversationRepo::new(conn).get(id))?
                    .is_some_and(|c| c.session_name.is_none());
                let name = first.then(|| self.session_title(fleet)).transpose()?;
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::SetConversationSession {
                        conversation_id: id,
                        agent_session_id: agent.session_id(&run.key),
                        session_name: name,
                    },
                )?;
                // With parts, the turn's body is the answer alone: the
                // narration around the work stays in the parts.
                let body = if parts.is_empty() {
                    reply.to_string()
                } else {
                    reply_answer(&parts)
                };
                self.append_with_parts(fleet, id, TurnRole::Agent, &body, parts)?;
                events.push(ConversationEvent::TurnFinished { error: None });
            }
            Err(message) if run.cold_resume => {
                // The recorded session could not be resumed: start fresh and
                // send the same message again.
                let (conversation, text) = fleet.read(|conn| {
                    let repo = ConversationRepo::new(conn);
                    let conversation = repo.get(id)?.context("conversation vanished")?;
                    let text = repo
                        .turns(id)?
                        .into_iter()
                        .find(|t| t.seq == run.user_seq)
                        .map(|t| t.body)
                        .unwrap_or_default();
                    Ok((conversation, text))
                })?;
                tracing::warn!(conversation = %id, "resume failed, rotating: {message}");
                // The corrections were already in the failed prompt; a fresh
                // session gets the current state in its snapshot.
                self.rotate_and_start(fleet, agent, &conversation, run.user_seq, "", &text)?;
                events.push(ConversationEvent::Rotated);
            }
            Err(message) => {
                self.append(fleet, id, TurnRole::Error, &message)?;
                self.last_error = Some(message.clone());
                events.push(ConversationEvent::TurnFinished {
                    error: Some(message),
                });
            }
        }
        Ok(())
    }

    fn ensure_row(&mut self, fleet: &FleetStore) -> Result<Uuid> {
        if let Some(id) = self.conversation_id {
            return Ok(id);
        }
        let id = Uuid::new_v4();
        let launch = &self.config.launch;
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::CreateConversation {
                id,
                focus: self.focus,
                platform: Some(launch.platform.label().to_lowercase()),
                model: Some(launch.model.clone()).filter(|m| !m.is_empty()),
                effort: Some(launch.effort.clone()).filter(|e| !e.is_empty()),
            },
        )?;
        self.conversation_id = Some(id);
        Ok(id)
    }

    fn append(&self, fleet: &FleetStore, id: Uuid, role: TurnRole, body: &str) -> Result<i64> {
        self.append_with_parts(fleet, id, role, body, Vec::new())
    }

    fn append_with_parts(
        &self,
        fleet: &FleetStore,
        id: Uuid,
        role: TurnRole,
        body: &str,
        parts: Vec<ReplyPart>,
    ) -> Result<i64> {
        let value = fleet.interview(
            ACTOR_USER,
            InterviewCommand::AppendConversationTurn {
                conversation_id: id,
                role,
                body: body.to_string(),
                parts,
            },
        )?;
        value["seq"].as_i64().context("turn seq missing")
    }

    /// Mark the rotation, forget the old session, and send `text` to a fresh
    /// one that gets the conversation's snapshot.
    fn rotate_and_start(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        conversation: &Conversation,
        user_seq: i64,
        changes: &str,
        text: &str,
    ) -> Result<()> {
        let id = conversation.id;
        agent.close_session(&Self::session_key(id));
        self.append(fleet, id, TurnRole::Rotation, ROTATION_NOTE)?;
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::SetConversationSession {
                conversation_id: id,
                agent_session_id: None,
                session_name: None,
            },
        )?;
        let budget = self.config.context.context_budget_tokens as i64;
        let context = fleet.read(|conn| {
            resume_snapshot(
                conn,
                &self.config.media,
                &self.config.data_root,
                id,
                budget,
                Some(user_seq),
            )
        })?;
        self.session_tokens = Some(0);
        self.start(
            fleet,
            agent,
            conversation,
            user_seq,
            Some(context),
            join(changes, text),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        fleet: &FleetStore,
        agent: &mut dyn AgentProvider,
        conversation: &Conversation,
        user_seq: i64,
        context: Option<String>,
        message: String,
        resume: Option<String>,
    ) -> Result<()> {
        let id = conversation.id;
        let key = Self::session_key(id);
        let opening = match &context {
            Some(context) => Some(SessionOpening {
                title: self.session_title(fleet)?,
                context: Some(context.clone()),
            }),
            None => None,
        };
        let sent = context.as_deref().map_or(0, estimate_tokens) + estimate_tokens(&message);
        self.session_tokens = Some(self.session_tokens.unwrap_or(0) + sent);
        let chars_at_start = agent.session_context_chars(&key).unwrap_or(0);
        let cold_resume = resume.is_some();
        let handle = agent.send_session_turn(SessionTurn {
            key: key.clone(),
            owner_id: id.to_string(),
            cwd: self.scratch_dir()?,
            options: self.config.launch.clone(),
            resume_session_id: resume,
            opening,
            message,
            purpose: SessionPurpose::Conversation,
            env: vec![(ACTOR_ENV.to_string(), actor_for(id))],
        });
        let handle = match handle {
            Ok(handle) => handle,
            Err(err) => {
                let message = format!("{err:#}");
                self.append(fleet, id, TurnRole::Error, &message)?;
                self.last_error = Some(message);
                return Err(err);
            }
        };
        // Once sent, the chars the provider reports include this message.
        let chars_at_start = chars_at_start.max(agent.session_context_chars(&key).unwrap_or(0));
        self.run = Some(Run {
            id: handle.id,
            key,
            user_seq,
            cold_resume,
            chars_at_start,
        });
        Ok(())
    }

    /// The agent-side session name: surface, focus title, start time.
    fn session_title(&self, fleet: &FleetStore) -> Result<String> {
        let focus = fleet.read(|conn| focus_selection(conn, self.focus))?;
        Ok(session_name(
            Some(CONVERSATION_SURFACE),
            &focus.title,
            chrono::Local::now(),
        ))
    }

    /// The session's size when this driver did not see it grow: the opening
    /// (or the latest snapshot) plus every turn since the latest rotation.
    fn estimate_from_transcript(&self, fleet: &FleetStore, id: Uuid) -> Result<i64> {
        fleet.read(|conn| {
            let turns = ConversationRepo::new(conn).turns(id)?;
            let since = turns
                .iter()
                .rposition(|t| t.role == TurnRole::Rotation)
                .map_or(0, |i| i + 1);
            let base = opening(conn, &self.config.media, &self.config.data_root, id)?;
            Ok(estimate_tokens(&base)
                + turns[since..]
                    .iter()
                    .map(|t| estimate_tokens(&t.body))
                    .sum::<i64>())
        })
    }

    /// An empty directory: the agent works on the project through `tod-cli`
    /// and needs nothing from a repository.
    fn scratch_dir(&self) -> Result<PathBuf> {
        let dir = self.config.data_root.join("agent").join("conversation");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(dir)
    }
}

/// The delta (if any), then the user's message.
fn join(changes: &str, text: &str) -> String {
    if changes.is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n# Message\n\n{text}", changes.trim_end())
    }
}
