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

use crate::conversation::context::{ReportedStale, focus_selection, last_action_at};
use crate::conversation::protocol::{
    Next, Protocol, ProtocolEnv, RunNotice, TurnContext, protocol_for,
};
use crate::interview::context::estimate_tokens;
use crate::media::MediaPaths;
use crate::session_name::session_name;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use tod_agent::{
    AgentLaunchOptions, AgentProvider, AgentRunState, PermissionRequest, PromptImage, RunId,
    SessionOpening, SessionTurn, SharedAgent, TokenUsage,
};
use tod_journey::{Actor, Decision, Event, JourneyKey, TurnPhase};
use tod_store::conversation::{
    Conversation, ConversationRepo, Focus, ProtocolKind, ReplyPart, TurnAttachment, TurnRole,
    reply_answer,
};
use tod_store::fleet::FleetStore;
use tod_store::fleet::Workdir;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::settings::InterviewContextSettings;
use uuid::Uuid;

/// How the driver reaches the agent: for one provider call at a time.
///
/// Starting and finishing a turn runs git, Docker, and `tod-cli`, which can
/// take seconds, so callers run the driver off the UI thread. A provider
/// shared with the UI ([`SharedAgentAccess`]) is locked only for each call,
/// never across that work, so the UI never waits on it.
pub trait AgentAccess {
    fn with<R>(&mut self, f: impl FnOnce(&mut dyn AgentProvider) -> R) -> R;
}

impl<T: AgentProvider> AgentAccess for T {
    fn with<R>(&mut self, f: impl FnOnce(&mut dyn AgentProvider) -> R) -> R {
        f(self)
    }
}

impl AgentAccess for dyn AgentProvider + '_ {
    fn with<R>(&mut self, f: impl FnOnce(&mut dyn AgentProvider) -> R) -> R {
        f(self)
    }
}

impl AgentAccess for dyn AgentProvider + Send + '_ {
    fn with<R>(&mut self, f: impl FnOnce(&mut dyn AgentProvider) -> R) -> R {
        f(self)
    }
}

/// The app's shared provider, locked for each call and released between.
pub struct SharedAgentAccess<'a>(pub &'a SharedAgent);

impl AgentAccess for SharedAgentAccess<'_> {
    fn with<R>(&mut self, f: impl FnOnce(&mut dyn AgentProvider) -> R) -> R {
        let mut agent = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(agent.as_mut())
    }
}

/// The body of the transcript entry that marks a fresh agent session.
pub const ROTATION_NOTE: &str = "Started a fresh agent session";

#[derive(Debug, Clone)]
pub struct ConversationConfig {
    pub data_root: PathBuf,
    pub media: MediaPaths,
    /// What the agent launches with when there is no `settings_path`; the
    /// focus node's Agent capability still sets what it sets over it.
    pub launch: AgentLaunchOptions,
    /// The settings file each turn's launch is read from
    /// ([`crate::conversation::launch`]), so a change there applies to the
    /// next turn. `None` launches with `launch`.
    pub settings_path: Option<PathBuf>,
    /// `context_budget_tokens` decides when the session rotates.
    pub context: InterviewContextSettings,
}

/// What the view shows about the turn in flight.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ConversationStatus {
    /// A turn is in flight.
    pub running: bool,
    /// What the agent is doing right now, when the provider reports it.
    pub activity: Option<String>,
    /// The turn in flight so far, part by part (text, thoughts, tool calls),
    /// when the provider streams them. Empty when no turn is in flight.
    /// Not sent to the app by the daemon: too chatty for a feed.
    #[serde(skip)]
    pub parts: Vec<ReplyPart>,
    /// The agent is blocked on a permission decision; answer it through the
    /// provider (`respond_to_permission`) with this request's run.
    pub permission: Option<PermissionRequest>,
    /// The last turn's failure, until the next send.
    pub last_error: Option<String>,
    /// The tokens the agent reported spending while this app held its
    /// session, when the provider reports any. The platform's record says
    /// more (`run_transcript::usage_for_key`).
    pub live_usage: Option<TokenUsage>,
    /// What the latest turn this driver started launched with: the platform,
    /// model, and effort asked for. `None` until it starts one.
    #[serde(skip)]
    pub launch: Option<AgentLaunchOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationEvent {
    /// A turn ended: its agent (or error) turn is in the transcript.
    TurnFinished { error: Option<String> },
    /// The driver started a fresh agent session; a rotation turn is in the
    /// transcript.
    Rotated,
    /// The protocol's loop sent another turn without the user; a continuation
    /// turn is in the transcript and a run is still in flight.
    Continued,
    /// The protocol has something to tell the user as the run ended (a failed
    /// commit, a branch that does not match): a toast.
    Notice(RunNotice),
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
    /// The session's name, as sent with the turn.
    title: String,
    /// The session's id is stored on the conversation.
    session_saved: bool,
    /// The cloud sandbox the agent runs in, whose session logs are kept
    /// outside it (`tod_store::fleet::session_log`).
    sandbox: Option<String>,
    /// Where the agent runs, so its session log can be kept for a move.
    workdir: Workdir,
}

pub struct ConversationDriver {
    config: ConversationConfig,
    focus: Focus,
    /// What kind of conversation this is: context, cwd, what "done" means,
    /// and whether the app loops it without the user.
    protocol: &'static dyn Protocol,
    /// Turns sent without the user since the last user message.
    continuations: u32,
    /// The protocol's progress fingerprint before the turn in flight started.
    progress_before: Option<String>,
    /// `None` until the first send creates the row.
    conversation_id: Option<Uuid>,
    run: Option<Run>,
    /// Estimated tokens in the current agent session; `None` until known.
    session_tokens: Option<i64>,
    reported_stale: ReportedStale,
    activity: Option<String>,
    parts: Vec<ReplyPart>,
    permission: Option<PermissionRequest>,
    last_error: Option<String>,
    live_usage: Option<TokenUsage>,
    launched: Option<AgentLaunchOptions>,
    /// Where this driver last started a turn (this machine, a dev container,
    /// a sandbox). A live agent session stays where it started, so a turn
    /// that now belongs elsewhere closes it and resumes it there.
    session_place: Option<Workdir>,
    /// Things the user must be told, delivered by the next [`Self::tick`].
    notices: Vec<RunNotice>,
}

impl ConversationDriver {
    /// An unsaved, empty conversation about `focus`. Its row is created by
    /// the first [`Self::send`].
    pub fn new(config: ConversationConfig, focus: Focus, protocol: ProtocolKind) -> Self {
        Self {
            config,
            focus,
            protocol: protocol_for(protocol),
            continuations: 0,
            progress_before: None,
            conversation_id: None,
            run: None,
            session_tokens: None,
            reported_stale: ReportedStale::new(),
            activity: None,
            parts: Vec::new(),
            permission: None,
            last_error: None,
            live_usage: None,
            launched: None,
            session_place: None,
            notices: Vec::new(),
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
        let mut driver = Self::new(config, conversation.focus, conversation.protocol);
        driver.conversation_id = Some(conversation_id);
        Ok(driver)
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Which protocol runs this conversation.
    pub fn protocol(&self) -> &'static dyn Protocol {
        self.protocol
    }

    /// Continuations sent for the user message being answered.
    pub fn continuations(&self) -> u32 {
        self.continuations
    }

    /// What the protocol is given about this conversation. Opens its own
    /// reads, so never call a protocol method from inside `fleet.read`.
    fn env<'a>(&'a self, fleet: &'a FleetStore, conversation_id: Uuid) -> ProtocolEnv<'a> {
        ProtocolEnv {
            fleet,
            media: &self.config.media,
            data_root: &self.config.data_root,
            conversation_id,
            focus: self.focus,
        }
    }

    /// A conversation resumes by its session id, which only works where the
    /// session's log is: when the node has moved (this machine, a cloud
    /// sandbox), copy it here from where it is, under this working
    /// directory's project name (`doc/agentd.md`, "Moving a node"). A failure
    /// is logged: the resume then fails and the driver's recovery runs.
    fn bring_session(&mut self, fleet: &FleetStore, conversation: Uuid, cwd: &Workdir, session: &str, moved: bool) {
        use tod_store::fleet::session_log::{self as log, ContainerRemote, HostRemote, MirrorRemote, SandboxRemote};
        let result = (|| -> anyhow::Result<bool> {
            let host = HostRemote::new()?;
            let mirror = match self.focus {
                Focus::Node(node) | Focus::Obligation { node, .. } | Focus::PlanStep { node, .. } => {
                    Some(MirrorRemote::new(fleet.paths().root(), &node.to_string()))
                }
                Focus::Project => None,
            };
            match cwd {
                Workdir::Host(path) => {
                    let sources: Vec<&dyn log::Remote> = mirror.iter().map(|m| m as &dyn log::Remote).collect();
                    log::ensure_session(&host, &sources, session, &log::project_dir_name(&path.to_string_lossy()))
                }
                Workdir::Sandbox { sandbox, path } => {
                    let target = SandboxRemote::new(sandbox);
                    let mut sources: Vec<&dyn log::Remote> = vec![&host];
                    sources.extend(mirror.iter().map(|m| m as &dyn log::Remote));
                    log::ensure_session(&target, &sources, session, &log::project_dir_name(path))
                }
                Workdir::Container { container, path } => {
                    let target = ContainerRemote::new(container)?;
                    let mut sources: Vec<&dyn log::Remote> = vec![&host];
                    sources.extend(mirror.iter().map(|m| m as &dyn log::Remote));
                    log::ensure_session(&target, &sources, session, &log::project_dir_name(path))
                }
            }
        })();
        // Shown to the user: a move that quietly lost the session would
        // otherwise look like an agent that forgot.
        match result {
            Ok(true) if moved => self.notices.push(RunNotice::Warning(format!(
                "This node moved: the agent's session continues in {cwd}."
            ))),
            Ok(true) => {}
            Ok(false) => self.fail_visibly(fleet, conversation, format!(
                "There is no copy of the agent's session log to bring to {cwd}, so it cannot continue there; a fresh session will be started."
            )),
            Err(err) => self.fail_visibly(fleet, conversation, format!(
                "Could not bring the agent's session log to {cwd}, so a fresh session will be started: {err:#}"
            )),
        }
    }

    /// Tell the user: a notice, and an error turn in the transcript (which an
    /// unattended run's transcript shows too).
    fn fail_visibly(&mut self, fleet: &FleetStore, conversation: Uuid, message: String) {
        if let Err(err) = self.append(fleet, conversation, TurnRole::Error, &message) {
            tracing::warn!("could not record an error in the transcript: {err:#}");
        }
        self.notices.push(RunNotice::Error(message));
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
            parts: self.parts.clone(),
            permission: self.permission.clone(),
            last_error: self.last_error.clone(),
            live_usage: self.live_usage.clone(),
            launch: self.launched.clone(),
        }
    }

    /// [`Self::launch_options`] for a launch: an unreadable setting is an
    /// error, never a quiet change of what the agent runs with.
    fn try_launch_options(&self, fleet: &FleetStore) -> Result<AgentLaunchOptions> {
        let kind = self.protocol.kind();
        match &self.config.settings_path {
            Some(path) => crate::conversation::launch::try_resolve_from(fleet, path, self.focus, kind),
            None => crate::conversation::launch::try_for_focus(fleet, self.focus, self.config.launch.clone()),
        }
    }

    /// What the next turn launches with: the settings for this kind of
    /// conversation, and the focus node's Agent capability over them. Reads
    /// the settings file and the store.
    pub fn launch_options(&self, fleet: &FleetStore) -> AgentLaunchOptions {
        let kind = self.protocol.kind();
        match &self.config.settings_path {
            Some(path) => crate::conversation::launch::resolve_from(fleet, path, self.focus, kind),
            None => crate::conversation::launch::for_focus(
                fleet,
                self.focus,
                self.config.launch.clone(),
            ),
        }
    }

    /// The provider's session key for a conversation.
    pub fn session_key(conversation_id: Uuid) -> String {
        format!("conversation-{conversation_id}")
    }

    /// Which journey this conversation's events are recorded to: the focus
    /// node's, or the project journey when it has none.
    fn journey_key(&self) -> JourneyKey {
        self.focus
            .node_id()
            .map(JourneyKey::Node)
            .unwrap_or(JourneyKey::Project)
    }

    /// Stop the turn in flight. Its user turn stays in the transcript with an
    /// error turn after it.
    pub fn cancel<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Result<()> {
        let Some(run) = self.run.take() else {
            return Ok(());
        };
        let _ = agent.with(|a| a.cancel_run(run.id));
        self.activity = None;
        self.parts.clear();
        self.permission = None;
        let id = self
            .conversation_id
            .context("a run without a conversation")?;
        self.append(fleet, id, TurnRole::Error, "Stopped")?;
        crate::journey::record(
            self.journey_key(),
            Actor::Agent { conversation: id },
            Event::AgentTurn {
                conversation: id,
                phase: TurnPhase::Stopped,
            },
        );
        Ok(())
    }

    /// Append the user's message and start the agent's turn on it. Creates
    /// the conversation row on the first send.
    /// Sends `text` as the user's turn. Returns the turn's seq — recorded by
    /// the caller as the journey's `send` `UserAction` (spec §3.1), along
    /// with the text's length rather than its content, which is already in
    /// the transcript.
    pub fn send<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        text: &str,
    ) -> Result<i64> {
        self.send_with_images(fleet, agent, text, Vec::new())
    }

    /// [`Self::send`], with images the user attached to the message. They
    /// are kept under the data root with the turn, and go to the agent with
    /// the message; a message may be images alone.
    pub fn send_with_images<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        text: &str,
        images: Vec<PromptImage>,
    ) -> Result<i64> {
        if self.run.is_some() {
            bail!("the agent is still working on the previous message");
        }
        let text = text.trim();
        if text.is_empty() && images.is_empty() {
            bail!("nothing to send");
        }
        self.last_error = None;
        let id = self.ensure_row(fleet)?;
        let key = Self::session_key(id);
        let attachments = images
            .iter()
            .map(|image| {
                TurnAttachment::save(fleet.paths().root(), id, &image.mime_type, &image.data)
            })
            .collect::<Result<Vec<_>>>()?;

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
            Some(at) => {
                let since = fleet.read(|conn| last_action_at(conn, id, at))?;
                self.protocol_delta(fleet, id, since)?
            }
            None => String::new(),
        };
        let sent_context = (!changes.is_empty()).then(|| changes.clone());
        let user_seq = self.append_with_parts_and_context(
            fleet,
            id,
            TurnRole::User,
            text,
            Vec::new(),
            sent_context,
            attachments,
        )?;
        // A user message ends whatever loop the previous one started.
        self.continuations = 0;

        let live = agent.with(|a| a.session_id(&key).is_some());
        let resumable = live || conversation.agent_session_id.is_some();
        let budget = self.config.context.context_budget_tokens as i64;
        if resumable && self.session_tokens.is_none() {
            self.session_tokens = Some(self.estimate_from_transcript(fleet, id)?);
        }
        let over_budget = self.session_tokens.is_some_and(|t| t > budget);

        let mut images = images;
        let mut note = String::new();
        // The retry below rotates again; the marker is written once.
        let mut rotated = false;
        let started = loop {
            let text_now = format!("{text}{note}");
            let text = text_now.as_str();
            let attempt = if resumable && !over_budget {
                let message = join(&changes, text);
                self.start(
                    fleet,
                    agent,
                    &conversation,
                    user_seq,
                    None,
                    message,
                    images.clone(),
                    conversation.agent_session_id.clone().filter(|_| !live),
                )
            } else if has_history {
                let reason = if !resumable {
                    "not resumable"
                } else {
                    "over budget"
                };
                let announce = !rotated;
                rotated = true;
                self.rotate_and_start(
                    fleet,
                    agent,
                    &conversation,
                    user_seq,
                    &changes,
                    text,
                    images.clone(),
                    reason,
                    announce,
                )
            } else {
                // The first turn: the opening context, then the message.
                let context = self.protocol.opening(&self.env(fleet, id))?;
                // Kept so the user can read what the agent was given.
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::SetConversationOpeningContext {
                        conversation_id: id,
                        context: context.clone(),
                    },
                )?;
                self.session_tokens = Some(0);
                self.start(
                    fleet,
                    agent,
                    &conversation,
                    user_seq,
                    Some(context),
                    join(&changes, text),
                    images.clone(),
                    None,
                )
            };
            // An agent that takes no images still gets the message: the same
            // user turn is kept (it is already recorded), never added twice.
            if let Err(e) = &attempt {
                if !images.is_empty() && e.to_string().contains(IMAGES_REFUSED) {
                    images.clear();
                    note = "

(A screenshot was taken but this agent does not accept images.)"
                        .into();
                    continue;
                }
            }
            break attempt;
        };
        started.map(|()| user_seq)
    }

    /// Collect a finished turn. Call on every poll.
    pub fn tick<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Vec<ConversationEvent> {
        let mut events = Vec::new();
        // Background session-log copies that failed since the last tick: put
        // in the transcript, so an unattended run shows them too.
        for problem in crate::conversation::problems::record_pending(fleet) {
            self.notices.push(RunNotice::Error(problem));
        }
        for notice in std::mem::take(&mut self.notices) {
            events.push(ConversationEvent::Notice(notice));
        }
        let key = self.run.as_ref().map(|run| run.key.clone());
        if let Err(err) = self.poll(fleet, agent, &mut events) {
            let error = format!("{err:#}");
            self.last_error = Some(error.clone());
            events.push(ConversationEvent::TurnFinished { error: Some(error) });
        }
        // Read after the poll: a turn that just ended has reported its
        // totals by the time it reads as ended.
        if let Some(usage) = key.and_then(|key| agent.with(|a| a.session_token_usage(&key))) {
            self.live_usage = Some(usage);
        }
        events
    }

    fn poll<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        events: &mut Vec<ConversationEvent>,
    ) -> Result<()> {
        let Some(run) = &self.run else {
            return Ok(());
        };
        let outcome = match agent.with(|a| a.poll_run(run.id)) {
            Some(AgentRunState::InFlight(activity)) => {
                self.parts = agent
                    .with(|a| a.session_reply_parts(&run.key))
                    .unwrap_or_default();
                self.activity = activity;
                self.permission = None;
                return self.save_session_id(fleet, agent);
            }
            Some(AgentRunState::NeedsPermission(request)) => {
                self.parts = agent
                    .with(|a| a.session_reply_parts(&run.key))
                    .unwrap_or_default();
                // Otherwise the view keeps showing the tool that asked, as
                // if it were still running.
                self.activity = Some(format!(
                    "Waiting for your permission: {}",
                    tod_agent::util::one_line_summary(&request.title, 120)
                ));
                self.permission = Some(request);
                return self.save_session_id(fleet, agent);
            }
            Some(AgentRunState::Success(reply)) => Ok(reply.unwrap_or_default()),
            Some(AgentRunState::Failure(message)) => Err(message),
            None => Err("agent run was lost".to_string()),
        };
        let run = self.run.take().expect("checked above");
        self.activity = None;
        self.parts.clear();
        self.permission = None;
        let id = self
            .conversation_id
            .context("a run without a conversation")?;
        let chars = agent
            .with(|a| a.session_context_chars(&run.key))
            .unwrap_or(run.chars_at_start);
        match outcome {
            Ok(reply) => {
                let parts = agent
                    .with(|a| a.session_reply_parts(&run.key))
                    .unwrap_or_default();
                let reply = reply.trim();
                let added = (chars.saturating_sub(run.chars_at_start) / 4) as i64;
                self.session_tokens =
                    Some(self.session_tokens.unwrap_or(0) + added.max(estimate_tokens(reply)));
                let first = fleet
                    .read(|conn| ConversationRepo::new(conn).get(id))?
                    .is_some_and(|c| c.session_name.is_none());
                let name = first.then(|| run.title.clone());
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::SetConversationSession {
                        conversation_id: id,
                        agent_session_id: agent.with(|a| a.session_id(&run.key)),
                        session_name: name,
                    },
                )?;
                // With parts, the turn's body is the answer alone: the
                // narration around the work stays in the parts.
                // The agent's session log leaves the sandbox with each turn,
                // so a recreated sandbox can resume this session.
                if let (Some(sandbox), Some(node)) = (run.sandbox.clone(), self.focus.node_id()) {
                    tod_store::fleet::session_log::pull_node_in_background(
                        fleet.paths().root().to_path_buf(),
                        node.to_string(),
                        sandbox,
                    );
                } else if let (Some(node), Some(session)) =
                    (self.focus.node_id(), agent.with(|a| a.session_id(&run.key)))
                {
                    // On this machine or in a dev container the log is kept
                    // too, so a move to another environment has it to copy.
                    tod_store::fleet::session_log::keep_session_in_background(
                        fleet.paths().root().to_path_buf(),
                        node.to_string(),
                        session,
                        run.workdir.clone(),
                    );
                }
                let body = if parts.is_empty() {
                    reply.to_string()
                } else {
                    reply_answer(&parts)
                };
                let seq = self.land_reply(fleet, agent, id, &body, parts, run.user_seq, events)?;
                crate::journey::record(
                    self.journey_key(),
                    Actor::Agent { conversation: id },
                    Event::AgentTurn {
                        conversation: id,
                        phase: TurnPhase::Replied { seq: seq as u64 },
                    },
                );
            }
            Err(message) if run.cold_resume => {
                // The recorded session could not be resumed: start fresh and
                // send the same message again, with its images.
                let (conversation, text, attachments) = fleet.read(|conn| {
                    let repo = ConversationRepo::new(conn);
                    let conversation = repo.get(id)?.context("conversation vanished")?;
                    let (text, attachments) = repo
                        .turns(id)?
                        .into_iter()
                        .find(|t| t.seq == run.user_seq)
                        .map(|t| (t.body, t.attachments))
                        .unwrap_or_default();
                    Ok((conversation, text, attachments))
                })?;
                let images = attachments
                    .iter()
                    .map(|attachment| {
                        Ok(PromptImage {
                            mime_type: attachment.mime_type.clone(),
                            data: attachment.read(fleet.paths().root())?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                tracing::warn!(conversation = %id, "resume failed, rotating: {message}");
                // The corrections were already in the failed prompt; a fresh
                // session gets the current state in its snapshot.
                self.rotate_and_start(
                    fleet,
                    agent,
                    &conversation,
                    run.user_seq,
                    "",
                    &text,
                    images,
                    "cold resume failed",
                    true,
                )?;
                events.push(ConversationEvent::Rotated);
            }
            Err(message) => {
                let seq = self.append(fleet, id, TurnRole::Error, &message)?;
                crate::journey::record(
                    self.journey_key(),
                    Actor::Agent { conversation: id },
                    Event::AgentTurn {
                        conversation: id,
                        phase: TurnPhase::Failed {
                            seq: seq as u64,
                            error: message.clone(),
                        },
                    },
                );
                self.last_error = Some(message.clone());
                events.push(ConversationEvent::TurnFinished {
                    error: Some(message),
                });
            }
        }
        Ok(())
    }

    /// Record a finished reply, and decide from what the agent recorded
    /// during the turn whether the exchange is over or another turn goes out
    /// without the user. Returns the agent turn's own seq.
    #[allow(clippy::too_many_arguments)]
    fn land_reply<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        id: Uuid,
        body: &str,
        parts: Vec<ReplyPart>,
        turn_seq: i64,
        events: &mut Vec<ConversationEvent>,
    ) -> Result<i64> {
        let reply_seq = self.append_with_parts(fleet, id, TurnRole::Agent, body, parts)?;
        for notice in self.protocol.on_reply(&self.env(fleet, id), body) {
            events.push(ConversationEvent::Notice(notice));
        }
        let report = fleet.read(|conn| ConversationRepo::new(conn).report_since(id, turn_seq))?;
        let next = {
            let env = self.env(fleet, id);
            let after = self.protocol.progress(&env).ok().flatten();
            let progressed = match (&self.progress_before, &after) {
                (Some(before), Some(after)) => before != after,
                // Nothing to compare: never stop the loop for it.
                _ => true,
            };
            self.protocol.next(&TurnContext {
                env: &env,
                report: report.as_ref(),
                continuations: self.continuations,
                progressed,
            })?
        };
        let decision = match &next {
            Next::Done(stop) => Decision::Stop {
                stop: match stop {
                    super::protocol::Stop::Complete => "complete".to_string(),
                    super::protocol::Stop::HandBack(_) => "hand_back".to_string(),
                    super::protocol::Stop::ContinuationCap => "continuation_cap".to_string(),
                    super::protocol::Stop::NoProgress => "no_progress".to_string(),
                },
                reason: match stop {
                    super::protocol::Stop::HandBack(reason) => reason.clone(),
                    _ => String::new(),
                },
            },
            Next::Continue { reason, .. } => Decision::Continue {
                reason: reason.clone(),
            },
        };
        crate::journey::record(
            self.journey_key(),
            Actor::App,
            Event::ProtocolDecision {
                conversation: id,
                protocol: self.protocol.kind().as_str().to_string(),
                decision,
            },
        );
        match next {
            Next::Done(_) => {
                self.continuations = 0;
                for notice in self.protocol.finish(&self.env(fleet, id)) {
                    events.push(ConversationEvent::Notice(notice));
                }
                events.push(ConversationEvent::TurnFinished { error: None });
            }
            Next::Continue { message, .. } => {
                self.continuations += 1;
                self.append(fleet, id, TurnRole::Continuation, &message)?;
                self.resend(fleet, agent, id, message)?;
                events.push(ConversationEvent::Continued);
            }
        }
        Ok(reply_seq)
    }

    /// Send another turn on the conversation's own session, without a user
    /// message behind it.
    fn resend<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        id: Uuid,
        message: String,
    ) -> Result<()> {
        let (conversation, last_seq) = fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            let conversation = repo.get(id)?.context("conversation vanished")?;
            let last_seq = repo.turns(id)?.last().map_or(0, |t| t.seq);
            Ok((conversation, last_seq))
        })?;
        let live = agent.with(|a| a.session_id(&Self::session_key(id)).is_some());
        let resume = conversation.agent_session_id.clone().filter(|_| !live);
        self.start(
            fleet,
            agent,
            &conversation,
            last_seq,
            None,
            message,
            Vec::new(),
            resume,
        )
    }

    fn ensure_row(&mut self, fleet: &FleetStore) -> Result<Uuid> {
        if let Some(id) = self.conversation_id {
            return Ok(id);
        }
        let id = Uuid::new_v4();
        let launch = &self.launch_options(fleet);
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::CreateConversation {
                id,
                focus: self.focus,
                protocol: self.protocol.kind(),
                platform: Some(launch.platform.label().to_lowercase()),
                model: Some(launch.model.clone()).filter(|m| !m.is_empty()),
                effort: Some(launch.effort.clone()).filter(|e| !e.is_empty()),
            },
        )?;
        // A gate check or on-entry run records which transition it is about,
        // as of now: the node may move on while the conversation stays.
        if let Some((from_state, to_state)) = self.protocol.transition(fleet, self.focus) {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::SetConversationTransition {
                    conversation_id: id,
                    from_state,
                    to_state,
                },
            )?;
        }
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
        self.append_with_parts_and_context(fleet, id, role, body, parts, None, Vec::new())
    }

    #[allow(clippy::too_many_arguments)]
    fn append_with_parts_and_context(
        &self,
        fleet: &FleetStore,
        id: Uuid,
        role: TurnRole,
        body: &str,
        parts: Vec<ReplyPart>,
        sent_context: Option<String>,
        attachments: Vec<TurnAttachment>,
    ) -> Result<i64> {
        let value = fleet.interview(
            ACTOR_USER,
            InterviewCommand::AppendConversationTurn {
                conversation_id: id,
                role,
                body: body.to_string(),
                parts,
                sent_context,
                attachments,
            },
        )?;
        value["seq"].as_i64().context("turn seq missing")
    }

    /// Mark the rotation, forget the old session, and send `text` to a fresh
    /// one that gets the conversation's snapshot. `reason` is recorded to the
    /// journey ("over budget" | "not resumable" | "cold resume failed").
    #[allow(clippy::too_many_arguments)]
    fn rotate_and_start<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        conversation: &Conversation,
        user_seq: i64,
        changes: &str,
        text: &str,
        images: Vec<PromptImage>,
        reason: &str,
        announce: bool,
    ) -> Result<()> {
        let id = conversation.id;
        agent.with(|a| a.close_session(&Self::session_key(id)));
        if announce {
            self.append(fleet, id, TurnRole::Rotation, ROTATION_NOTE)?;
            crate::journey::record(
                self.journey_key(),
                Actor::App,
                Event::SessionRotated {
                    conversation: id,
                    reason: reason.to_string(),
                },
            );
        }
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::SetConversationSession {
                conversation_id: id,
                agent_session_id: None,
                session_name: None,
            },
        )?;
        let budget = self.config.context.context_budget_tokens as i64;
        let context =
            self.protocol
                .resume_snapshot(&self.env(fleet, id), budget, Some(user_seq))?;
        self.session_tokens = Some(0);
        self.start(
            fleet,
            agent,
            conversation,
            user_seq,
            Some(context),
            join(changes, text),
            images,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        conversation: &Conversation,
        user_seq: i64,
        context: Option<String>,
        message: String,
        images: Vec<PromptImage>,
        resume: Option<String>,
    ) -> Result<()> {
        let id = conversation.id;
        let key = Self::session_key(id);
        // Named once, when first sent; every later turn reuses that name.
        let title = match &conversation.session_name {
            Some(name) => name.clone(),
            None => self.session_title(fleet)?,
        };
        check_tod_cli()?;
        let (cwd, turn_env, progress) = {
            let env = self.env(fleet, id);
            let cwd = match self.protocol.cwd(&env) {
                Ok(cwd) => cwd,
                Err(err) => {
                    // Shown in the transcript and the view: the turn is not sent.
                    let message = format!("{err:#}");
                    self.append(fleet, id, TurnRole::Error, &message)?;
                    self.last_error = Some(message);
                    return Err(err);
                }
            };
            if let Err(err) = self.protocol.prepare(&env) {
                let err = err.context("preparing the working directory");
                let message = format!("{err:#}");
                self.append(fleet, id, TurnRole::Error, &message)?;
                self.last_error = Some(message);
                return Err(err);
            }
            let mut turn_env = self.protocol.turn_env(&env);
            turn_env.extend(tod_cli_path_env());
            (
                cwd,
                turn_env,
                self.protocol.progress(&env).ok().flatten(),
            )
        };
        let environment = launch_environment(fleet, self.focus, &cwd)?;
        // The node moved while its session was live: the agent process is
        // still where it began, so end it and resume the same session here
        // (`doc/agentd.md`, "Moving a node").
        let mut resume = resume;
        let mut moved = false;
        if resume.is_none()
            && context.is_none()
            && self.session_place.as_ref().is_some_and(|place| *place != cwd)
        {
            moved = true;
            resume = agent.with(|a| {
                let session = a.session_id(&key);
                a.close_session(&key);
                session
            });
        }
        self.session_place = Some(cwd.clone());
        if let Some(session) = &resume {
            self.bring_session(fleet, id, &cwd, session, moved);
        }
        self.progress_before = progress;
        // Whatever the protocol, an agent running in a codebase gets its rules.
        let context =
            context.map(|context| crate::codebase_rules::with_codebase_rules_in(context, &cwd));
        // An agent inside a dev container or sandbox is started from the
        // data root here.
        let workdir = cwd.clone();
        let sandbox = match &cwd {
            Workdir::Sandbox { sandbox, .. } => Some(sandbox.clone()),
            _ => None,
        };
        let cwd = match cwd {
            Workdir::Host(path) => path,
            Workdir::Container { .. } | Workdir::Sandbox { .. } => {
                fleet.paths().root().to_path_buf()
            }
        };
        let opening = context.as_ref().map(|context| SessionOpening {
            context: Some(context.clone()),
        });
        let sent = context.as_deref().map_or(0, estimate_tokens)
            + estimate_tokens(&message)
            + images.len() as i64 * IMAGE_TOKENS;
        self.session_tokens = Some(self.session_tokens.unwrap_or(0) + sent);
        let cold_resume = resume.is_some();
        let options = match self.try_launch_options(fleet) {
            Ok(options) => options,
            Err(err) => {
                let message = format!("{err:#}");
                self.append(fleet, id, TurnRole::Error, &message)?;
                self.last_error = Some(message);
                return Err(err);
            }
        };
        self.launched = Some(options.clone());
        let turn_had_images = !images.is_empty();
        let turn = SessionTurn {
            key: key.clone(),
            owner_id: id.to_string(),
            title: title.clone(),
            cwd,
            options,
            resume_session_id: resume,
            opening,
            message,
            images,
            purpose: self.protocol.purpose(),
            env: turn_env,
            environment,
        };
        let (chars_at_start, handle, chars_sent) = agent.with(|a| {
            let before = a.session_context_chars(&key).unwrap_or(0);
            let handle = a.send_session_turn(turn);
            (before, handle, a.session_context_chars(&key).unwrap_or(0))
        });
        let handle = match handle {
            Ok(handle) => handle,
            Err(err) => {
                let message = format!("{err:#}");
                // The caller retries without the images; nothing to report.
                if !turn_had_images || !message.contains(IMAGES_REFUSED) {
                    self.append(fleet, id, TurnRole::Error, &message)?;
                }
                self.last_error = Some(message);
                return Err(err);
            }
        };
        // Once sent, the chars the provider reports include this message.
        let chars_at_start = chars_at_start.max(chars_sent);
        self.run = Some(Run {
            id: handle.id,
            key,
            user_seq,
            cold_resume,
            chars_at_start,
            title,
            session_saved: false,
            sandbox,
            workdir,
        });
        crate::journey::record(
            self.journey_key(),
            Actor::Agent { conversation: id },
            Event::AgentTurn {
                conversation: id,
                phase: TurnPhase::Started {
                    user_seq: user_seq as u64,
                },
            },
        );
        Ok(())
    }

    /// Store the session's id on the conversation as soon as the agent has
    /// given one, not when the turn ends: a turn that never ends still leaves
    /// the session resumable.
    fn save_session_id<A: AgentAccess + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
    ) -> Result<()> {
        let (Some(run), Some(id)) = (self.run.as_mut(), self.conversation_id) else {
            return Ok(());
        };
        if run.session_saved {
            return Ok(());
        }
        let Some(session) = agent.with(|a| a.session_id(&run.key)) else {
            return Ok(());
        };
        run.session_saved = true;
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::SetConversationSession {
                conversation_id: id,
                agent_session_id: Some(session),
                session_name: Some(run.title.clone()),
            },
        )?;
        Ok(())
    }

    /// The agent-side session name: surface, focus title, start time.
    fn session_title(&self, fleet: &FleetStore) -> Result<String> {
        let focus = fleet.read(|conn| focus_selection(conn, self.focus))?;
        Ok(session_name(
            Some(self.protocol.surface()),
            &focus.title,
            chrono::Local::now(),
        ))
    }

    /// The session's size when this driver did not see it grow: the opening
    /// (or the latest snapshot) plus every turn since the latest rotation.
    fn estimate_from_transcript(&self, fleet: &FleetStore, id: Uuid) -> Result<i64> {
        let base = self.protocol.opening(&self.env(fleet, id))?;
        fleet.read(|conn| {
            let turns = ConversationRepo::new(conn).turns(id)?;
            let since = turns
                .iter()
                .rposition(|t| t.role == TurnRole::Rotation)
                .map_or(0, |i| i + 1);
            Ok(estimate_tokens(&base)
                + turns[since..]
                    .iter()
                    .map(|t| estimate_tokens(&t.body))
                    .sum::<i64>())
        })
    }

    /// The protocol's delta, with `reported_stale` lent to it.
    fn protocol_delta(&mut self, fleet: &FleetStore, id: Uuid, since: i64) -> Result<String> {
        let mut reported = std::mem::take(&mut self.reported_stale);
        let result = self
            .protocol
            .delta(&self.env(fleet, id), since, &mut reported);
        self.reported_stale = reported;
        result
    }
}

/// Where a conversation about `focus` whose protocol works in `cwd` runs its
/// agent: a node whose Files name a dev container or sandbox runs it there.
pub(crate) fn launch_environment(
    fleet: &FleetStore,
    focus: Focus,
    cwd: &Workdir,
) -> Result<tod_agent::AgentEnvironment> {
    match focus.node_id() {
        Some(node) => fleet.agent_environment(&node.to_string(), cwd),
        None => tod_store::fleet::dev_container::environment_for(None, cwd, fleet.paths().root()),
    }
}

/// About what one attached image adds to a session's context.
const IMAGE_TOKENS: i64 = 1_600;

/// What an agent without image support says (`tod_agent`'s ACP provider).
const IMAGES_REFUSED: &str = "does not accept images";

/// The delta (if any), then the user's message.
fn join(changes: &str, text: &str) -> String {
    if changes.is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n# Message\n\n{text}", changes.trim_end())
    }
}

/// Refuse to send a turn when the `tod-cli` beside the app was built from
/// different source: the agent would be working from commands that no longer
/// match what it is told (`cargo run -p tod` rebuilds only `tod`). A match is
/// remembered; a mismatch is checked again on the next turn, after a rebuild.
fn check_tod_cli() -> Result<()> {
    static MATCHED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if MATCHED.get().is_some() {
        return Ok(());
    }
    let cli = crate::interview::tod_cli_path();
    if !cli.is_file() {
        // Tests, or no install: nothing the agent could run anyway.
        return Ok(());
    }
    let out = std::process::Command::new(&cli)
        .arg("--build-stamp")
        .output()
        .with_context(|| format!("run {}", cli.display()))?;
    let stamp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || stamp != crate::CLI_BUILD_STAMP {
        anyhow::bail!(
            "{} was built from different source than this app, so the agent \
            would get commands that do not match its instructions. Rebuild it \
            (`cargo build -p tod-cli`) and send again.",
            cli.display()
        );
    }
    let _ = MATCHED.set(());
    Ok(())
}

/// `PATH` with the directory holding the installed `tod-cli` first, so the
/// agent's `tod-cli` resolves to the one next to this app — never a build in
/// some other checkout it went looking for. Empty when there is no such
/// binary (tests, or a broken install).
fn tod_cli_path_env() -> Option<(String, String)> {
    let cli = crate::interview::tod_cli_path();
    let dir = cli.parent().filter(|_| cli.is_file())?;
    Some((
        "PATH".to_string(),
        prepend_path(dir, std::env::var_os("PATH"))?,
    ))
}

fn prepend_path(dir: &std::path::Path, path: Option<std::ffi::OsString>) -> Option<String> {
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(path) = path {
        dirs.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(dirs).ok()?.into_string().ok()
}

#[cfg(test)]
mod path_tests {
    use super::prepend_path;
    use std::path::Path;

    #[test]
    fn the_tod_cli_directory_goes_first_on_path() {
        let dir = Path::new("tod-bin");
        let rest = std::env::join_paths(["a", "b"]).unwrap();
        let path = prepend_path(dir, Some(rest)).unwrap();
        let dirs: Vec<_> = std::env::split_paths(&path).collect();
        assert_eq!(dirs, [Path::new("tod-bin"), Path::new("a"), Path::new("b")]);
    }
}
