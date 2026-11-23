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
use crate::conversation::quota;
use crate::interview::context::estimate_tokens;
use crate::media::MediaPaths;
use crate::session_name::session_name;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use tod_agent::{
    AgentLaunchOptions, AgentPlatform, AgentProvider, AgentRunState, PermissionRequest, PromptImage, RunId,
    SessionOpening, SessionTurn, SharedAgent, TokenUsage,
};
use tod_agent::agent_launch::{parse_platform, platform_storage};
use tod_journey::{Actor, Decision, Event, JourneyKey, TurnPhase};
use tod_store::conversation::{
    Conversation, ConversationRepo, Focus, ProtocolKind, ReplyPart, TurnAttachment, TurnRole,
    reply_answer,
};
use tod_store::fleet::FleetStore;
use tod_store::fleet::Workdir;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::outline::uuid_blob::now_ms;
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

fn can_resume_platform(saved: Option<AgentPlatform>, requested: AgentPlatform) -> bool {
    match saved {
        Some(saved) => saved == requested,
        // Untagged sessions predate the new ACP providers.
        None => matches!(requested, AgentPlatform::Claude | AgentPlatform::Cursor),
    }
}

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
    /// The turn could not start because another node holds the repository's
    /// worktree lock (`WorktreeLockBusy`): not a failure and not in the
    /// transcript. The caller waits and tries again.
    FilesBusy { message: String },
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
    /// What the session had reported spending before this turn, so the
    /// turn's own spending is the difference (`ConversationDriver::record_usage`).
    usage_before: tod_agent::TokenCounts,
    /// The original provider's wait, if the replacement cannot authenticate.
    quota_retry: Option<String>,
}

pub struct ConversationDriver {
    config: ConversationConfig,
    focus: Focus,
    /// What kind of conversation this is: context, cwd, what "done" means,
    /// and whether the app loops it without the user.
    protocol: &'static dyn Protocol,
    /// Turns sent without the user since the last user message.
    continuations: u32,
    /// Consecutive turns that changed nothing the protocol tracks.
    quiet_streak: u32,
    /// The protocol's progress fingerprint before the turn in flight started.
    progress_before: Option<String>,
    /// `None` until the first send creates the row.
    conversation_id: Option<Uuid>,
    /// For a review conversation, which of the round's reviewers the row is
    /// created for, and (for the risk reviewer) the round's main conversation.
    reviewer: Option<(tod_store::review::Reviewer, Option<Uuid>)>,
    /// The group of the phase's skills the row is created for.
    skill_group: usize,
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
    /// The proof the turn in flight is being processed
    /// (`tod_store::activity`): held from the turn's start to its end,
    /// refreshed by [`Self::tick`]. A lease on the focus's node, of the
    /// protocol's kind (`next_actor::Work::conversation_kind`).
    lease: Option<tod_store::activity::ActivityLease>,
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
            quiet_streak: 0,
            progress_before: None,
            conversation_id: None,
            skill_group: 0,
            reviewer: None,
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
            lease: None,
        }
    }

    /// Work with skill group `group` of the phase (`lifecycle_config::groups`)
    /// instead of the phase agent's own: a session of its own for that group.
    pub fn with_skill_group(mut self, group: usize) -> Self {
        self.skill_group = group;
        self
    }

    /// Be `reviewer` of a review round (find-it-first 6h); the risk reviewer
    /// names `round`, the main reviewer's conversation it runs beside.
    pub fn with_reviewer(mut self, reviewer: tod_store::review::Reviewer, round: Option<Uuid>) -> Self {
        self.reviewer = Some((reviewer, round));
        self
    }

    /// Which of a review round's reviewers this conversation is.
    pub fn reviewer(&self) -> Option<tod_store::review::Reviewer> {
        self.reviewer.map(|(r, _)| r)
    }

    /// Take the lease for the turn that just started. A failure to record it
    /// is told to the user (a notice), not dropped.
    fn start_lease(&mut self, fleet: &FleetStore) {
        let Some(node) = self.focus.node_id() else {
            return;
        };
        let kind = crate::next_actor::Work::conversation_kind(self.protocol.kind());
        match tod_store::activity::ActivityLease::start(fleet, node, &kind, &crate::next_actor::this_process()) {
            Ok(lease) => self.lease = Some(lease),
            Err(err) => self.notices.push(RunNotice::Error(format!("{err:#}"))),
        }
    }

    /// The turn ended (finished, failed, stopped): let the lease go.
    fn end_lease(&mut self, fleet: &FleetStore) {
        if let Some(lease) = self.lease.take()
            && let Err(err) = lease.end(fleet)
        {
            self.notices.push(RunNotice::Error(format!("{err:#}")));
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
        driver.skill_group = conversation.skill_group;
        driver.reviewer = conversation.reviewer.map(|r| (r, conversation.review_round));
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
            Some(path) => crate::conversation::launch::try_resolve_from(fleet, path, self.focus, kind, self.reviewer()),
            None => {
                let phase = crate::conversation::launch::phase_for(fleet, self.focus, kind, self.reviewer());
                crate::conversation::launch::try_for_focus(fleet, self.focus, phase.as_deref(), self.config.launch.clone())
            }
        }
    }

    /// What the next turn launches with: the settings for this kind of
    /// conversation, the focus node's Agent capability over them, and the
    /// Lifecycle config's model and effort for the phase over that. Reads
    /// the settings file and the store.
    pub fn launch_options(&self, fleet: &FleetStore) -> AgentLaunchOptions {
        let kind = self.protocol.kind();
        match &self.config.settings_path {
            Some(path) => crate::conversation::launch::resolve_from(fleet, path, self.focus, kind, self.reviewer()),
            None => {
                let phase = crate::conversation::launch::phase_for(fleet, self.focus, kind, self.reviewer());
                crate::conversation::launch::for_focus(fleet, self.focus, phase.as_deref(), self.config.launch.clone())
            }
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
        let Some(run) = self.run.as_ref() else {
            return Ok(());
        };
        agent.with(|a| a.cancel_run(run.id)).context("canceling the agent run")?;
        self.run.take();
        self.end_lease(fleet);
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
        // What the user answered since the last turn went out, for a session
        // that goes on (a rotation's snapshot carries it already). Read before
        // this turn is appended, which is what marks it as sent.
        let answers = self.unsent_answers(fleet, id, text)?;
        let sent_context = Some(join_sections(&changes, &answers)).filter(|c| !c.is_empty());
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
        self.quiet_streak = 0;

        let launch = match self.try_launch_options(fleet)
            .and_then(|desired| quota::route(fleet.paths().root(), desired, now_ms()))
        {
            Ok(launch) => launch,
            Err(err) => {
                self.start_failed(fleet, id, &err)?;
                return Err(err);
            }
        };
        let live = agent.with(|a| a.session_id(&key).is_some());
        let resumable = live || conversation.agent_session_id.is_some();
        let saved_platform = conversation.platform.as_deref().and_then(parse_platform);
        let session_platform = if live {
            self.launched.as_ref().map(|launch| launch.platform).or(saved_platform)
        } else {
            saved_platform
        };
        let switch_note = session_platform.filter(|previous| *previous != launch.platform)
            .map(|previous| format!("Switched from {} to {}", previous.label(), launch.platform.label()));
        let switch_session = resumable && !can_resume_platform(session_platform, launch.platform);
        let budget = self.config.context.context_budget_tokens as i64;
        if resumable && !switch_session && self.session_tokens.is_none() {
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
            let attempt = if resumable && !over_budget && !switch_session {
                let message = join(&join_sections(&changes, &answers), text);
                self.start(
                    fleet,
                    agent,
                    &conversation,
                    user_seq,
                    None,
                    message,
                    images.clone(),
                    conversation.agent_session_id.clone().filter(|_| !live),
                    launch.clone(),
                )
            } else if has_history || switch_session {
                let reason = if switch_session {
                    "provider changed"
                } else if !resumable {
                    "not resumable"
                } else {
                    "over budget"
                };
                let marker = (!rotated).then(|| switch_note.as_deref().unwrap_or(ROTATION_NOTE));
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
                    marker,
                    launch.clone(),
                )
            } else {
                // The first turn: the opening context, then the message.
                let context = self.with_recent_answers(
                    fleet,
                    self.protocol.opening(&self.env(fleet, id))?,
                    text,
                )?;
                // Kept so the user can read what the agent was given, with
                // what `start` puts at its very end (a reviewer's assignment).
                let shown = match self.protocol.opening_tail(&self.env(fleet, id))? {
                    Some(tail) => format!("{context}\n\n{tail}"),
                    None => context.clone(),
                };
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::SetConversationOpeningContext {
                        conversation_id: id,
                        context: shown,
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
                    launch.clone(),
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

    /// A consumer that has no loop of its own to wait and retry in (the chat
    /// drawer: a person sent the message) answers a
    /// [`ConversationEvent::FilesBusy`] with this: the turn ended unanswered,
    /// so say so in the transcript, naming who holds the lock. The drawer then
    /// offers its Retry on the failed message. One row per busy event, never
    /// per retry: only a person's own send can come back here.
    pub fn record_files_busy(&mut self, fleet: &FleetStore, holder: &str) -> Result<()> {
        let id = self
            .conversation_id
            .context("a busy lock for a conversation that was never started")?;
        let message = format!(
            "Another node is still making its files ({holder}), so this message did not reach the agent. Send it again once that is done."
        );
        self.append(fleet, id, TurnRole::Error, &message)?;
        self.last_error = Some(message);
        Ok(())
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
        if let Some(lease) = self.lease.as_mut()
            && let Err(err) = lease.beat(fleet)
        {
            events.push(ConversationEvent::Notice(RunNotice::Error(format!("{err:#}"))));
        }
        let key = self.run.as_ref().map(|run| run.key.clone());
        if let Err(err) = self.poll(fleet, agent, &mut events) {
            let error = format!("{err:#}");
            if tod_store::fleet::worktree::is_lock_busy(&err) {
                // Typed through the rotation and start paths: a wait, so no
                // error is remembered and none is in the transcript.
                events.push(ConversationEvent::FilesBusy { message: error });
                return events;
            }
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

    /// Records what the turn that just ended spent
    /// (`tod_store::conversation_usage`): what the session reports now less
    /// what it reported when the turn started, or the whole of it when it
    /// reports less than before (a session that started over).
    fn record_usage<A: AgentAccess + ?Sized>(
        &self,
        fleet: &FleetStore,
        agent: &mut A,
        run: &Run,
        conversation: Uuid,
    ) -> Result<()> {
        let Some(now) = agent.with(|a| a.session_token_usage(&run.key)) else {
            return Ok(());
        };
        let counts = usage_delta(&now.total, &run.usage_before);
        if counts.is_empty() {
            return Ok(());
        }
        let model = now
            .model
            .clone()
            .or_else(|| now.agent_model.clone())
            .or_else(|| self.launched.as_ref().map(|l| l.model.clone()));
        let phase = crate::conversation::launch::phase_for(fleet, self.focus, self.protocol.kind(), self.reviewer());
        fleet
            .enqueue(tod_store::fleet::FleetMutation::RecordTurnUsage(
                tod_store::conversation_usage::TurnUsage {
                    conversation_id: conversation,
                    at_ms: tod_store::outline::uuid_blob::now_ms(),
                    phase,
                    model,
                    counts,
                },
            ))
            .map_err(|e| anyhow::anyhow!("{e}"))
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
        let mut outcome = match agent.with(|a| a.poll_run(run.id)) {
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
        let now = now_ms();
        let quota_limit = match &outcome {
            Ok(reply) => quota::banner(reply, now),
            Err(message) => quota::detect(message, now),
        };
        if quota_limit.is_some() && let Ok(reply) = outcome {
            outcome = Err(reply);
        }
        let run = self.run.take().expect("checked above");
        self.end_lease(fleet);
        self.activity = None;
        let partial = std::mem::take(&mut self.parts);
        self.permission = None;
        let id = self
            .conversation_id
            .context("a run without a conversation")?;
        // The turn ended, well or not: either way it spent tokens.
        if let Err(err) = self.record_usage(fleet, agent, &run, id) {
            events.push(ConversationEvent::Notice(RunNotice::Error(format!(
                "could not record the turn's token usage: {err:#}"
            ))));
        }
        if let Some(original) = run.quota_retry.as_ref()
            && let Err(message) = &outcome
            && tod_store::incident_class::classify(tod_store::incident_class::Source::Agent, message).kind
                == tod_store::incident_class::Kind::Auth
        {
            self.append(fleet, id, TurnRole::Error, message)?;
            outcome = Err(original.clone());
        }
        if let Some(limit) = &quota_limit
            && let Some(launch) = self.launched.clone()
            && quota::equivalent(&launch).is_some()
        {
            let root = fleet.paths().root();
            quota::remember(root, launch.platform, limit, now)?;
            match quota::route(root, launch.clone(), now) {
                Ok(next) => {
                    let (conversation, text, images) = Self::read_retry(fleet, id, run.user_seq)?;
                    let mut handoff = String::from(
                        "The previous agent stopped at its usage limit. Check the task, recorded changes, and current working files before continuing. Preserve completed work and avoid repeating actions that already took effect.",
                    );
                    let parts = agent.with(|a| a.session_reply_parts(&run.key)).unwrap_or(partial);
                    let answer = reply_answer(&parts);
                    if !answer.is_empty() && quota::banner(&answer, now).is_none() {
                        let excerpt: String = answer.chars().take(2_000).collect();
                        handoff.push_str("\n\nPartial reply from the previous agent:\n");
                        handoff.push_str(&excerpt);
                    }
                    let marker = quota_switch_note(launch.platform, next.platform);
                    let progress_before = self.progress_before.clone();
                    let retry = quota::retry_error(outcome.as_ref().err().map_or("", String::as_str),
                        launch.platform, limit.reset_at_ms);
                    let destination = next.platform.label();
                    match self.rotate_and_start(fleet, agent, &conversation, run.user_seq, &handoff,
                        &text, images, "provider quota exhausted", Some(&marker), next)
                    {
                        Ok(()) => {
                            if let Some(run) = &mut self.run {
                                run.quota_retry = Some(retry);
                            }
                            self.progress_before = progress_before;
                            events.push(ConversationEvent::Rotated);
                            return Ok(());
                        }
                        Err(err) if tod_store::fleet::worktree::is_lock_busy(&err) => return Err(err),
                        Err(err) => {
                            events.push(ConversationEvent::Notice(RunNotice::Error(format!(
                                "Could not continue with {destination}: {err:#}"
                            ))));
                            outcome = Err(retry);
                        }
                    }
                }
                Err(blocked) => {
                    let blocked = blocked.to_string();
                    // Keep the bounded Claude retry for an unknown reset when
                    // its next check is the earliest route back to a provider.
                    let use_window = quota::scheduled_reset(&blocked) == Some(limit.reset_at_ms)
                        && outcome.as_ref().err().is_some_and(|message| {
                            tod_store::incident_class::acp_usage_window_without_reset(message).is_some()
                        });
                    if !use_window {
                        outcome = Err(blocked);
                    }
                }
            }
        }
        let chars = agent
            .with(|a| a.session_context_chars(&run.key))
            .unwrap_or(run.chars_at_start);
        match outcome {
            Ok(reply) => {
                if let Some(node) = self.focus.node_id().filter(|_| run.sandbox.is_some()) {
                    crate::cloud_sync::lost::remade(fleet, &node.to_string());
                }
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
                        launch: self.launched.clone(),
                    },
                )?;
                // With parts, the turn's body is the answer alone: the
                // narration around the work stays in the parts.
                // The agent's session log leaves the sandbox with each turn,
                // so a recreated sandbox can resume this session.
                let claude = self.launched.as_ref().is_some_and(|launch| launch.platform == AgentPlatform::Claude);
                if let (true, Some(sandbox), Some(node)) = (claude, run.sandbox.clone(), self.focus.node_id()) {
                    tod_store::fleet::session_log::pull_node_in_background(
                        fleet.paths().root().to_path_buf(),
                        node.to_string(),
                        sandbox,
                    );
                } else if let (true, Some(node), Some(session)) =
                    (claude, self.focus.node_id(), agent.with(|a| a.session_id(&run.key)))
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
            // The node's sandbox was deleted outside the app: forget it and
            // send the same message again, which makes it a new one.
            Err(message)
                if self.focus.node_id().zip(run.sandbox.as_deref()).is_some_and(|(node, sandbox)| {
                    // Remaking is bounded like the lost-sandbox check's: a spent
                    // bound leaves the real error to the arms below.
                    tod_store::fleet::provision::says_sandbox_gone(&message, sandbox)
                        && crate::cloud_sync::lost::can_remake(fleet, &node.to_string())
                        && tod_store::fleet::provision::forget_lost_sandbox(fleet, &node.to_string(), sandbox, &message)
                            .unwrap_or(false)
                        && {
                            // Counted only now that a remake is really begun.
                            crate::cloud_sync::lost::note_remake(fleet, &node.to_string());
                            true
                        }
                }) =>
            {
                let (conversation, text, images) = Self::read_retry(fleet, id, run.user_seq)?;
                tracing::warn!(conversation = %id, "sandbox gone, making a new one: {message}");
                self.rotate_and_start(
                    fleet,
                    agent,
                    &conversation,
                    run.user_seq,
                    "",
                    &text,
                    images,
                    "sandbox lost",
                    Some(ROTATION_NOTE),
                    self.launched.clone().context("no launch for the turn being retried")?,
                )?;
                events.push(ConversationEvent::Rotated);
            }
            Err(message) if run.cold_resume && quota_limit.is_none() => {
                // The recorded session could not be resumed: start fresh and
                // send the same message again, with its images.
                let (conversation, text, images) = Self::read_retry(fleet, id, run.user_seq)?;
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
                    Some(ROTATION_NOTE),
                    self.launched.clone().context("no launch for the turn being retried")?,
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

    fn read_retry(fleet: &FleetStore, id: Uuid, seq: i64) -> Result<(Conversation, String, Vec<PromptImage>)> {
        let (conversation, text, attachments) = fleet.read(|conn| {
            let repo = ConversationRepo::new(conn);
            let conversation = repo.get(id)?.context("conversation vanished")?;
            let (text, attachments) = repo.turns(id)?.into_iter().find(|turn| turn.seq == seq)
                .map(|turn| (turn.body, turn.attachments)).unwrap_or_default();
            Ok((conversation, text, attachments))
        })?;
        let images = attachments.into_iter().map(|attachment| {
            let data = attachment.read(fleet.paths().root())?;
            Ok(PromptImage { mime_type: attachment.mime_type, data })
        }).collect::<Result<Vec<_>>>()?;
        Ok((conversation, text, images))
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
        let new_streak: u32;
        let quiet_turn: bool;
        let next = {
            let env = self.env(fleet, id);
            let after = match self.protocol.progress(&env) {
                Ok(after) => after,
                Err(err) => {
                    let err = err.context("reading the state the turn changed");
                    let message = format!("{err:#}");
                    self.append(fleet, id, TurnRole::Error, &message)?;
                    self.last_error = Some(message);
                    return Err(err);
                }
            };
            let progressed = match (&self.progress_before, &after) {
                (Some(before), Some(after)) => before != after,
                // Nothing to compare: never stop the loop for it.
                _ => true,
            };
            quiet_turn = !progressed;
            // A quiet turn within the allowance is not a stall (an agent
            // waiting on a run it started); see `TurnContext::progressed`.
            let (allowed, streak) = super::protocol::quiet_streak_step(self.quiet_streak, progressed);
            new_streak = streak;
            // An agent that sent its node back this turn (`tod-cli lifecycle
            // send-back`) is done here whatever its protocol would say: the
            // node is in another phase, or the run stops for the user on a
            // refused one.
            match self.sent_back_this_turn(fleet, id, turn_seq)? {
                Some(why) => Next::Done(super::protocol::Stop::HandBack(why)),
                None => self.protocol.next(&TurnContext {
                    env: &env,
                    report: report.as_ref(),
                    continuations: self.continuations,
                    progressed: allowed,
                })?,
            }
        };
        self.quiet_streak = new_streak;
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
                self.quiet_streak = 0;
                for notice in self.protocol.finish(&self.env(fleet, id)) {
                    events.push(ConversationEvent::Notice(notice));
                }
                events.push(ConversationEvent::TurnFinished { error: None });
            }
            Next::Continue { message, .. } => {
                self.continuations += 1;
                // An answer given while the run was going reaches it here,
                // once, whatever the protocol.
                let answers = self.unsent_answers(fleet, id, &message)?;
                let message = join_sections(&message, &answers);
                let message = if quiet_turn {
                    format!("{}{message}", super::protocol::QUIET_TURN_NOTE)
                } else {
                    message
                };
                self.append(fleet, id, TurnRole::Continuation, &message)?;
                self.resend(fleet, agent, id, message)?;
                events.push(ConversationEvent::Continued);
            }
        }
        Ok(reply_seq)
    }

    /// Why the turn that started at `turn_seq` ends the exchange because the
    /// agent sent the node back in it, recording each send-back in the
    /// journey with the agent as actor; `None` when it sent none.
    fn sent_back_this_turn(&self, fleet: &FleetStore, id: Uuid, turn_seq: i64) -> Result<Option<String>> {
        let sent = fleet.read(|conn| tod_store::send_back::SendBackRepo::new(conn).for_conversation_since(id, turn_seq))?;
        let Some(last) = sent.last() else {
            return Ok(None);
        };
        for s in &sent {
            crate::journey::record(
                self.journey_key(),
                Actor::Agent { conversation: id },
                Event::UserAction {
                    action: format!("send_back:{}", s.to_state),
                    source: "tod-cli lifecycle send-back".to_string(),
                    surface: s.protocol.clone(),
                    presented: Default::default(),
                },
            );
            if s.moved() {
                crate::journey::record(
                    self.journey_key(),
                    Actor::Agent { conversation: id },
                    Event::Transition { from: s.from_state.clone(), to: s.to_state.clone() },
                );
            }
        }
        Ok(Some(if last.moved() {
            format!("sent the task back from {} to {}: {}", last.from_state, last.to_state, last.reason)
        } else {
            format!("asked to send the task back to {} and was stopped: {}", last.to_state, last.stop_why)
        }))
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
        let previous = self.launched.clone().context("no launch for the conversation being continued")?;
        let launch = match quota::route(fleet.paths().root(), previous.clone(), now_ms()) {
            Ok(launch) => launch,
            Err(err) => {
                self.start_failed(fleet, id, &err)?;
                return Err(err);
            }
        };
        if launch.platform != previous.platform {
            let marker = quota_switch_note(previous.platform, launch.platform);
            return self.rotate_and_start(fleet, agent, &conversation, last_seq, "", &message,
                Vec::new(), "provider quota exhausted", Some(&marker), launch);
        }
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
            launch,
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
                platform: Some(platform_storage(launch.platform).to_owned()),
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
        if self.skill_group > 0 {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::SetConversationSkillGroup {
                    conversation_id: id,
                    group: self.skill_group,
                },
            )?;
        }
        if let Some((reviewer, round)) = self.reviewer {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::SetConversationReviewer { conversation_id: id, reviewer, round },
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

    /// `context` followed by the answers the user gave on this node lately
    /// (`node_context::render_recent_answers`). Every fresh session gets
    /// them, whatever its protocol: answering a request reopens the step it
    /// held up, and the next run is a new session that sees only the step,
    /// so without this it never learns the answer and hands the step back
    /// again (ENG-2072 was answered three times). Nothing is added for a
    /// conversation without a node.
    fn with_recent_answers(&self, fleet: &FleetStore, mut context: String, sending: &str) -> Result<String> {
        let Some(node) = self.focus.node_id() else {
            return Ok(context);
        };
        let answered = fleet
            .read(|conn| Ok(crate::node_context::render_recent_answers(conn, node, sending)?))
            .context("could not read the user's recent answers")?;
        if !answered.is_empty() {
            context.push_str("\n\n");
            context.push_str(&answered);
        }
        Ok(context)
    }

    /// The answers the user gave on this node since this conversation last
    /// sent its agent anything (`node_context::render_unsent_answers`): what a
    /// session that is being resumed or continued has not been told. A fresh
    /// session gets them through [`Self::with_recent_answers`] instead, so a
    /// turn never carries both. Empty for a conversation without a node.
    fn unsent_answers(&self, fleet: &FleetStore, id: Uuid, sending: &str) -> Result<String> {
        let Some(node) = self.focus.node_id() else {
            return Ok(String::new());
        };
        fleet
            .read(|conn| Ok(crate::node_context::render_unsent_answers(conn, node, id, sending)?))
            .context("could not read the user's recent answers")
    }

    /// Mark the rotation, forget the old session, and send `text` to a fresh
    /// one that gets the conversation's snapshot. `reason` is recorded to the
    /// journey along with a marker in the transcript.
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
        marker: Option<&str>,
        launch: AgentLaunchOptions,
    ) -> Result<()> {
        let id = conversation.id;
        agent.with(|a| a.close_session(&Self::session_key(id)));
        if let Some(marker) = marker {
            self.append(fleet, id, TurnRole::Rotation, marker)?;
            crate::journey::record(
                self.journey_key(),
                Actor::App,
                Event::SessionRotated {
                    conversation: id,
                    reason: reason.to_string(),
                },
            );
        }
        let saved_platform = conversation.platform.as_deref().and_then(parse_platform);
        // Keep the old provider's resume ID until the replacement starts.
        // A failed switch can then return to the previous provider.
        if can_resume_platform(saved_platform, launch.platform) {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::SetConversationSession {
                    conversation_id: id,
                    agent_session_id: None,
                    session_name: None,
                    launch: None,
                },
            )?;
        }
        let budget = self.config.context.context_budget_tokens as i64;
        let context = self.with_recent_answers(
            fleet,
            self.protocol
                .resume_snapshot(&self.env(fleet, id), budget, Some(user_seq))?,
            text,
        )?;
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
            launch,
        )
    }

    /// A turn that could not start: the error is in the transcript and the
    /// view's last error, unless it is only the worktree lock being busy,
    /// which the caller waits out and retries (rows per retry would bury the
    /// transcript).
    fn start_failed(&mut self, fleet: &FleetStore, id: Uuid, err: &anyhow::Error) -> Result<()> {
        if tod_store::fleet::worktree::is_lock_busy(err) {
            return Ok(());
        }
        let message = format!("{err:#}");
        self.append(fleet, id, TurnRole::Error, &message)?;
        self.last_error = Some(message);
        Ok(())
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
        options: AgentLaunchOptions,
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
            // A node whose sandbox expired while it sat idle gets a new one
            // here (forked from its source, session logs restored) and the
            // turn goes on, instead of failing for the user to sort out.
            let resolved = match test_hooks::take_cwd_error() {
                Some(err) => Err(err),
                None => match self.focus.node_id() {
                    Some(node) => tod_store::fleet::provision::retry_if_sandbox_lost(fleet, &node.to_string(), || {
                        self.protocol.cwd(&env)
                    }),
                    None => self.protocol.cwd(&env),
                },
            };
            let cwd = match resolved {
                Ok(cwd) => cwd,
                Err(err) => {
                    // Shown in the transcript and the view: the turn is not sent.
                    self.start_failed(fleet, id, &err)?;
                    return Err(err);
                }
            };
            if let Err(err) = self.protocol.prepare(&env) {
                let err = err.context("preparing the working directory");
                self.start_failed(fleet, id, &err)?;
                return Err(err);
            }
            let mut turn_env = self.protocol.turn_env(&env);
            turn_env.extend(tod_cli_path_env());
            let progress = match self.protocol.progress(&env) {
                Ok(progress) => progress,
                Err(err) => {
                    let err = err.context("reading the state before the turn");
                self.start_failed(fleet, id, &err)?;
                    return Err(err);
                }
            };
            (cwd, turn_env, progress)
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
        if let Some(session) = &resume && options.platform == AgentPlatform::Claude {
            self.bring_session(fleet, id, &cwd, session, moved);
        }
        self.progress_before = progress;
        // Whatever the protocol, an agent running in a codebase gets its rules.
        let context =
            context.map(|context| crate::codebase_rules::with_codebase_rules_in(context, &cwd));
        // The opening names this machine's `tod-cli` path; an agent in a
        // sandbox or container has the relay shim on its PATH instead.
        let context = match &cwd {
            Workdir::Host(_) => context,
            Workdir::Container { .. } | Workdir::Sandbox { .. } => {
                context.map(crate::dynamic::for_remote_agent)
            }
        };
        // Last of all, what the protocol puts at the very end of a fresh
        // session's context (a reviewer's assignment), so everything before
        // it can be a prefix two sessions share.
        let context = match context {
            Some(mut context) => {
                if let Some(tail) = self.protocol.opening_tail(&self.env(fleet, id))? {
                    context.push_str("\n\n");
                    context.push_str(&tail);
                }
                Some(context)
            }
            None => None,
        };
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
        // Everything sent besides the user's text is kept on the user turn,
        // so the transcript can show it: the opening context or snapshot
        // (only on the turn that actually sent it), then the delta.
        if let Some(context) = &context {
            let delta = fleet
                .read(|conn| ConversationRepo::new(conn).turns(id))?
                .into_iter()
                .find(|t| t.seq == user_seq)
                .and_then(|t| t.sent_context);
            let sent_context = match delta {
                Some(delta) => format!("{}\n\n{delta}", context.trim_end()),
                None => context.clone(),
            };
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::SetConversationTurnSentContext {
                    conversation_id: id,
                    seq: user_seq,
                    sent_context,
                },
            )?;
        }
        let opening = context.as_ref().map(|context| SessionOpening {
            context: Some(context.clone()),
        });
        let sent = context.as_deref().map_or(0, estimate_tokens)
            + estimate_tokens(&message)
            + images.len() as i64 * IMAGE_TOKENS;
        self.session_tokens = Some(self.session_tokens.unwrap_or(0) + sent);
        let cold_resume = resume.is_some();
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
        let (chars_at_start, handle, chars_sent, usage_before) = agent.with(|a| {
            let before = a.session_context_chars(&key).unwrap_or(0);
            let usage_before = a.session_token_usage(&key).map(|u| u.total).unwrap_or_default();
            let handle = a.send_session_turn(turn);
            (before, handle, a.session_context_chars(&key).unwrap_or(0), usage_before)
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
            usage_before,
            quota_retry: None,
        });
        self.start_lease(fleet);
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
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::SetConversationSession {
                conversation_id: id,
                agent_session_id: Some(session),
                session_name: Some(run.title.clone()),
                launch: self.launched.clone(),
            },
        )?;
        run.session_saved = true;
        Ok(())
    }

    /// The agent-side session name: surface, focus title, start time.
    fn session_title(&self, fleet: &FleetStore) -> Result<String> {
        let focus = fleet.read(|conn| focus_selection(conn, self.focus))?;
        // A later group of the phase's skills is a session of its own, told
        // apart in the agent's session picker too.
        let mut surface = match self.skill_group {
            0 => self.protocol.surface().to_string(),
            group => format!("{} skills {}", self.protocol.surface(), group + 1),
        };
        // A review round's risk reviewer is a session of its own beside the
        // main one.
        if self.reviewer() == Some(tod_store::review::Reviewer::Risk) {
            surface.push_str(" (risk)");
        }
        Ok(session_name(Some(&surface), &focus.title, chrono::Local::now()))
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

/// `first`, then `second`, as paragraphs; whichever is empty is left out.
fn quota_switch_note(from: AgentPlatform, to: AgentPlatform) -> String {
    format!("{} reached its usage limit. Continuing with {}.", from.label(), to.label())
}

fn join_sections(first: &str, second: &str) -> String {
    match (first.trim_end(), second.trim_end()) {
        ("", second) => second.to_string(),
        (first, "") => first.to_string(),
        (first, second) => format!("{first}\n\n{second}"),
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
            (`{}`) and send again.",
            cli.display(),
            rebuild_command(&cli)
        );
    }
    let _ = MATCHED.set(());
    Ok(())
}

/// The command that rebuilds `tod-cli` in the profile it was built in: the
/// directory under `target/` that holds it names the profile.
fn rebuild_command(cli: &std::path::Path) -> String {
    let profile = cli.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str());
    match profile {
        Some("release") => "cargo build --release -p tod-cli".to_string(),
        Some("debug") | None => "cargo build -p tod-cli".to_string(),
        Some(other) => format!("cargo build --profile {other} -p tod-cli"),
    }
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

/// Lets a test stand for the protocol's working directory failing (a worktree
/// lock another node holds), which a real one makes only with a git
/// repository and a second process.
mod test_hooks {
    #[cfg(test)]
    thread_local! {
        pub static CWD_ERROR: std::cell::RefCell<Option<anyhow::Error>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(test)]
    pub fn set_cwd_error(err: anyhow::Error) {
        CWD_ERROR.with(|e| *e.borrow_mut() = Some(err));
    }

    #[cfg(test)]
    pub fn take_cwd_error() -> Option<anyhow::Error> {
        CWD_ERROR.with(|e| e.borrow_mut().take())
    }

    #[cfg(not(test))]
    pub fn take_cwd_error() -> Option<anyhow::Error> {
        None
    }
}

#[cfg(test)]
pub(crate) use test_hooks::set_cwd_error;

/// What a session spent between two readings of its running total.
fn usage_delta(now: &tod_agent::TokenCounts, before: &tod_agent::TokenCounts) -> tod_agent::TokenCounts {
    let restarted = now.input < before.input
        || now.output < before.output
        || now.cache_read < before.cache_read
        || now.cache_write < before.cache_write;
    if restarted {
        return now.clone();
    }
    tod_agent::TokenCounts {
        requests: now.requests.saturating_sub(before.requests),
        input: now.input - before.input,
        output: now.output - before.output,
        cache_read: now.cache_read - before.cache_read,
        cache_write: now.cache_write - before.cache_write,
        cache_write_5m: now.cache_write_5m.saturating_sub(before.cache_write_5m),
        cache_write_1h: now.cache_write_1h.saturating_sub(before.cache_write_1h),
        thinking: now.thinking.saturating_sub(before.thinking),
        web_searches: now.web_searches.saturating_sub(before.web_searches),
        web_fetches: now.web_fetches.saturating_sub(before.web_fetches),
    }
}

#[cfg(test)]
mod usage_delta_tests {
    use super::*;
    use tod_agent::TokenCounts;

    fn counts(input: u64, read: u64, write: u64, output: u64) -> TokenCounts {
        TokenCounts { input, cache_read: read, cache_write: write, output, ..Default::default() }
    }

    #[test]
    fn a_turn_is_the_growth_of_the_running_total() {
        let d = usage_delta(&counts(15, 900, 1100, 90), &counts(10, 0, 1000, 50));
        assert_eq!((d.input, d.cache_read, d.cache_write, d.output), (5, 900, 100, 40));
    }

    #[test]
    fn a_total_that_went_down_is_a_session_that_started_over() {
        let d = usage_delta(&counts(3, 0, 500, 9), &counts(10, 0, 1000, 50));
        assert_eq!((d.input, d.cache_write, d.output), (3, 500, 9));
    }
}
