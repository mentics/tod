use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState, PermissionOption,
    PermissionRequest, RunId, SessionPurpose, SessionTurn,
};
use crate::ReplyPart;
use crate::agent_launch::AgentLaunchOptions;
use crate::agent_traffic::{
    InterviewAgentCounts, SharedAgentTrafficLog, TrafficDirection, TrafficTag,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

/// One interview turn handed to the registered mock handler.
#[derive(Debug, Clone)]
pub struct MockInterviewTurn {
    pub purpose: SessionPurpose,
    pub env: Vec<(String, String)>,
    pub blocks: Vec<String>,
}

/// What a mock handler replies: the text, and optionally the parts a real
/// agent would have streamed (see [`AgentProvider::session_reply_parts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockReply {
    pub text: String,
    pub parts: Option<Vec<ReplyPart>>,
}

impl From<String> for MockReply {
    fn from(text: String) -> Self {
        Self { text, parts: None }
    }
}

/// Plays an interview agent for `--agent mock`. The transport cannot know how
/// interview data is stored, so the caller supplies the behavior.
pub type MockInterviewHandler =
    Arc<dyn Fn(&MockInterviewTurn) -> Result<MockReply> + Send + Sync>;

fn handler_slot() -> &'static Mutex<Option<MockInterviewHandler>> {
    static SLOT: OnceLock<Mutex<Option<MockInterviewHandler>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Register the behavior mock interview sessions run on each turn.
pub fn set_mock_interview_handler(handler: MockInterviewHandler) {
    *handler_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}

/// Fast in-process agent backend for UI tests; never calls an external process.
pub struct MockAgentProvider {
    runs: HashMap<RunId, AgentRunState>,
    /// Runs the handler is still playing: kind, session key, and the result.
    pending: HashMap<RunId, (AgentRunKind, String, mpsc::Receiver<Result<MockReply, String>>)>,
    /// Runs held on a permission request, and how to release them.
    gated: HashMap<RunId, (PermissionRequest, mpsc::Sender<()>)>,
    /// Where each run's traffic is filed.
    run_agent: HashMap<RunId, TrafficTag>,
    fleet_run_sessions: HashMap<RunId, String>,
    sessions: HashMap<String, MockSession>,
    traffic_log: Option<SharedAgentTrafficLog>,
    /// Last options passed to [`Self::start_fleet_agent`] (tests / diagnostics).
    pub last_fleet_options: Option<AgentLaunchOptions>,
    /// Last session title passed to [`Self::start_fleet_agent`] (tests / diagnostics).
    pub last_fleet_session_title: Option<String>,
}

/// A conversation the mock is "holding": an id and how much it took in.
struct MockSession {
    session_id: String,
    messages: u32,
    purpose: SessionPurpose,
    context_chars: u64,
    /// The latest turn's parts, when the handler gave any.
    reply_parts: Option<Vec<ReplyPart>>,
}

impl MockAgentProvider {
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
            pending: HashMap::new(),
            gated: HashMap::new(),
            run_agent: HashMap::new(),
            fleet_run_sessions: HashMap::new(),
            sessions: HashMap::new(),
            traffic_log: None,
            last_fleet_options: None,
            last_fleet_session_title: None,
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
        let tag = self.run_agent.get(&run_id).cloned().unwrap_or_else(|| {
            TrafficTag::new(format!("{run_id:?}"), "", kind.traffic_label())
        });
        tag.record(log, kind.traffic_category(), direction, content);
    }

    fn finish(
        &mut self,
        kind: AgentRunKind,
        tag: TrafficTag,
        request: Option<&str>,
        state: AgentRunState,
    ) -> AgentRunHandle {
        let id = RunId::new();
        self.run_agent.insert(id, tag);
        if let Some(req) = request {
            self.log_traffic(kind, id, TrafficDirection::Request, req);
        }
        match &state {
            AgentRunState::Success(Some(text)) => {
                self.log_traffic(kind, id, TrafficDirection::Response, text)
            }
            AgentRunState::Failure(message) => {
                self.log_traffic(kind, id, TrafficDirection::Response, message)
            }
            _ => {}
        }
        self.runs.insert(id, state);
        AgentRunHandle { id }
    }

    fn drain_pending(&mut self) {
        let done: Vec<(RunId, AgentRunKind, String, Result<MockReply, String>)> = self
            .pending
            .iter()
            .filter_map(|(id, (kind, key, rx))| {
                rx.try_recv().ok().map(|r| (*id, *kind, key.clone(), r))
            })
            .collect();
        for (id, kind, key, result) in done {
            self.pending.remove(&id);
            let state = match result {
                Ok(reply) => {
                    self.log_traffic(kind, id, TrafficDirection::Response, &reply.text);
                    if let Some(session) = self.sessions.get_mut(&key) {
                        session.reply_parts = reply.parts;
                    }
                    AgentRunState::Success(Some(reply.text))
                }
                Err(err) => {
                    self.log_traffic(kind, id, TrafficDirection::Response, &err);
                    AgentRunState::Failure(err)
                }
            };
            self.runs.insert(id, state);
        }
    }
}

impl Default for MockAgentProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for MockAgentProvider {
    fn start_fleet_agent(
        &mut self,
        owner_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
        session_title: String,
        _environment: crate::AgentEnvironment,
    ) -> Result<AgentRunHandle> {
        self.last_fleet_options = Some(options);
        self.last_fleet_session_title = Some(session_title.clone());
        let preview: String = prompt.chars().take(200).collect();
        let reply = format!(
            "Fleet agent run complete (mock).\n\n\
             Owner: {owner_id}\n\
             Cwd: {}\n\n\
             Prompt preview:\n{preview}…",
            cwd.display()
        );
        let kind = AgentRunKind::FleetAgent;
        let tag = TrafficTag::new(owner_id, &session_title, kind.traffic_label());
        let handle = self.finish(
            kind,
            tag,
            Some(&prompt),
            AgentRunState::Success(Some(reply)),
        );
        self.fleet_run_sessions
            .insert(handle.id, format!("mock-fleet-session-{:?}", handle.id));
        Ok(handle)
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> Result<AgentRunHandle> {
        let blocks = turn.prompt_blocks();
        let request = blocks.join("\n\n");
        let session = self
            .sessions
            .entry(turn.key.clone())
            .or_insert_with(|| MockSession {
                session_id: turn
                    .resume_session_id
                    .clone()
                    .unwrap_or_else(|| format!("mock-session-{}", uuid::Uuid::new_v4())),
                messages: 0,
                purpose: turn.purpose,
                context_chars: 0,
                reply_parts: None,
            });
        session.messages += 1;
        session.reply_parts = None;
        session.context_chars += request.len() as u64;
        let kind = turn.purpose.run_kind();
        let tag = TrafficTag::new(turn.key.clone(), &turn.title, kind.traffic_label());

        if turn.purpose == SessionPurpose::Chat {
            let reply = mock_session_reply(session.messages, &turn);
            session.context_chars += reply.len() as u64;
            return Ok(self.finish(
                kind,
                tag,
                Some(&request),
                AgentRunState::Success(Some(reply)),
            ));
        }

        let handler = handler_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(handler) = handler else {
            return Ok(self.finish(
                kind,
                tag,
                Some(&request),
                AgentRunState::Failure("no mock interview handler registered".into()),
            ));
        };
        let id = RunId::new();
        self.run_agent.insert(id, tag);
        self.log_traffic(kind, id, TrafficDirection::Request, &request);
        let (tx, rx) = mpsc::channel();
        let mock_turn = MockInterviewTurn {
            purpose: turn.purpose,
            env: turn.env,
            blocks,
        };
        // A `permission <title>` line holds the turn on a permission request,
        // the way a real agent waits on one, until it is answered.
        let gate = permission_line(&turn.message).map(|title| {
            let (open, wait) = mpsc::channel();
            let request = PermissionRequest {
                run: id,
                title: title.to_string(),
                options: ["allow", "reject"]
                    .map(|id| PermissionOption {
                        id: id.to_string(),
                        label: format!("{}{}", id[..1].to_uppercase(), &id[1..]),
                    })
                    .to_vec(),
            };
            self.gated.insert(id, (request, open));
            wait
        });
        thread::spawn(move || {
            if let Some(wait) = gate
                && wait.recv().is_err()
            {
                return;
            }
            let _ = tx.send(handler(&mock_turn).map_err(|err| format!("{err:#}")));
        });
        self.runs.insert(id, AgentRunState::InFlight(None));
        self.pending.insert(id, (kind, turn.key, rx));
        Ok(AgentRunHandle { id })
    }

    fn session_id(&self, key: &str) -> Option<String> {
        self.sessions
            .get(key)
            .map(|session| session.session_id.clone())
    }

    fn fleet_run_session_id(&self, id: RunId) -> Option<String> {
        self.fleet_run_sessions.get(&id).cloned()
    }


    fn session_context_chars(&self, key: &str) -> Option<u64> {
        self.sessions.get(key).map(|session| session.context_chars)
    }

    fn session_reply_parts(&self, key: &str) -> Option<Vec<ReplyPart>> {
        self.sessions.get(key)?.reply_parts.clone()
    }

    fn close_session(&mut self, key: &str) {
        self.sessions.remove(key);
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.drain_pending();
        if let Some((request, _)) = self.gated.get(&id) {
            return Some(AgentRunState::NeedsPermission(request.clone()));
        }
        self.runs.get(&id).cloned()
    }

    /// Either answer lets the turn go on: the mock has nothing to withhold.
    fn respond_to_permission(&mut self, id: RunId, _option_id: &str) -> Result<()> {
        let (_, open) = self
            .gated
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("run has no pending permission request"))?;
        let _ = open.send(());
        Ok(())
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        self.gated.remove(&id);
        self.pending.remove(&id);
        self.runs.remove(&id);
        self.run_agent.remove(&id);
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        let mut counts = InterviewAgentCounts::default();
        for (kind, _, _) in self.pending.values() {
            match kind {
                AgentRunKind::QuestionMakerReplenishment => counts.question_maker_in_flight += 1,
                AgentRunKind::AnswerProcessor => counts.answer_active += 1,
                AgentRunKind::FleetAgent => {}
            }
        }
        counts.answer_pool = self
            .sessions
            .values()
            .filter(|s| s.purpose == SessionPurpose::AnswerProcessor)
            .count() as u32;
        if counts.answer_pool > 0 {
            counts.answer_max = 2;
        }
        counts
    }
}

/// Spell out what reached the agent, so UI checks can see a session's opening
/// arrive exactly once.
fn mock_session_reply(message_number: u32, turn: &SessionTurn) -> String {
    // A gate check's request is the session's opening context; the message
    // beside it is only the starter.
    let request = match turn.opening.as_ref().and_then(|o| o.context.as_deref()) {
        Some(context) => format!("{context}\n{}", turn.message),
        None => turn.message.clone(),
    };
    if request.contains("phase_purpose:** gate_check") {
        return mock_gate_check_reply(&request);
    }

    let mut reply = format!("Mock session reply · message {message_number}\n\n");
    match &turn.opening {
        Some(opening) => {
            reply.push_str(&format!("- Session named **{}**\n", turn.title));
            match opening.context.as_deref() {
                Some(context) => reply.push_str(&format!(
                    "- Received {} lines of context ahead of this message\n",
                    context.lines().count()
                )),
                None => reply.push_str("- No context was sent\n"),
            }
        }
        None => reply.push_str("- Nothing re-sent: the session already has its context\n"),
    }
    reply.push_str(&format!("\nYou said: {}", turn.message));
    reply
}

/// Canned pass reply for a gate-check turn (see `tod_core::gate::response`
/// for the format this must satisfy). The mock has no lifecycle policy of
/// its own — it just echoes the `forward_state` and criterion ids the
/// request's `gate_check:` YAML block already carried.
pub fn mock_gate_check_reply(message: &str) -> String {
    // The role doc's response format shows the same keys with placeholders
    // (`{target lifecycle}`, `{uuid}`); only real values count.
    let forward_state = message
        .lines()
        .filter_map(|line| line.trim().strip_prefix("forward_state:"))
        .map(str::trim)
        .rfind(|value| !value.is_empty() && !value.starts_with('{'))
        .unwrap_or("");
    let criterion_ids: Vec<&str> = message
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- id:"))
        .map(str::trim)
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .collect();

    let mut reply = format!(
        "result: pass\nforward_lifecycle: {forward_state}\npaused: false\nsummary: \"Mock gate check: pass.\"\nfindings: \"Mock gate check: pass.\"\n"
    );
    if !criterion_ids.is_empty() {
        reply.push_str("gate_results:\n");
        for id in criterion_ids {
            reply.push_str(&format!(
                "  - criterion_id: {id}\n    outcome: pass\n    detail: \"mock pass\"\n    action: none\n"
            ));
        }
    }
    reply
}

/// The title of the message's `permission <title>` line, if it has one.
fn permission_line(message: &str) -> Option<&str> {
    message
        .lines()
        .find_map(|line| line.trim().strip_prefix("permission "))
        .map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::AgentPlatform;
    use std::time::Duration;

    fn poll_run(mock: &mut MockAgentProvider, id: RunId) -> AgentRunState {
        for _ in 0..400 {
            if let Some(state) = mock.poll_run(id) {
                if !matches!(state, AgentRunState::InFlight(_)) {
                    return state;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for run {id:?}");
    }

    fn session_turn(
        key: &str,
        opening: Option<crate::provider::SessionOpening>,
        resume_session_id: Option<&str>,
        message: &str,
        purpose: SessionPurpose,
    ) -> SessionTurn {
        SessionTurn {
            key: key.into(),
            owner_id: "config".into(),
            title: format!("Session {key}"),
            cwd: PathBuf::from("."),
            options: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
            resume_session_id: resume_session_id.map(str::to_string),
            opening,
            message: message.into(),
            purpose,
            env: vec![("TOD_INTERVIEW_ACTOR".into(), "actor-1".into())],
            environment: crate::AgentEnvironment::Host,
        }
    }

    #[test]
    fn mock_session_sends_opening_once_and_keeps_its_id() {
        use crate::provider::SessionOpening;

        let mut mock = MockAgentProvider::new();
        let opening = SessionOpening {
            context: Some("line one\nline two".into()),
        };
        let first = mock
            .send_session_turn(session_turn(
                "run-7",
                Some(opening),
                None,
                "first",
                SessionPurpose::Chat,
            ))
            .unwrap();
        let AgentRunState::Success(Some(reply)) = poll_run(&mut mock, first.id) else {
            panic!("first message failed");
        };
        assert!(reply.contains("Session named **Session run-7**"), "{reply}");
        assert!(reply.contains("2 lines of context"), "{reply}");
        let session_id = mock
            .session_id("run-7")
            .expect("session id after first message");

        let second = mock
            .send_session_turn(session_turn("run-7", None, None, "second", SessionPurpose::Chat))
            .unwrap();
        let AgentRunState::Success(Some(reply)) = poll_run(&mut mock, second.id) else {
            panic!("second message failed");
        };
        assert!(reply.contains("Nothing re-sent"), "{reply}");
        assert_eq!(mock.session_id("run-7"), Some(session_id));
        assert!(mock.session_context_chars("run-7").unwrap() > 0);
    }

    #[test]
    fn a_session_is_one_transcript_listed_under_its_name() {
        let traffic = crate::agent_traffic::shared_log();
        let mut mock = MockAgentProvider::new().with_traffic_log(traffic.clone());
        for message in ["first", "second"] {
            let run = mock
                .send_session_turn(session_turn("run-7", None, None, message, SessionPurpose::Chat))
                .unwrap();
            poll_run(&mut mock, run.id);
        }
        let run = mock
            .start_fleet_agent(
                "task-1",
                PathBuf::from("."),
                "go".into(),
                AgentLaunchOptions::for_platform(AgentPlatform::Claude),
                "Fleet · Fix login".into(),
                crate::AgentEnvironment::Host,
            )
            .unwrap();
        poll_run(&mut mock, run.id);

        let mut summaries: Vec<_> = traffic
            .lock()
            .unwrap()
            .agent_summaries()
            .into_iter()
            .map(|s| (s.id, s.label, s.entry_count))
            .collect();
        summaries.sort();
        assert_eq!(
            summaries,
            [
                ("run-7".to_string(), "Session run-7".to_string(), 4),
                ("task-1".to_string(), "Fleet · Fix login".to_string(), 2),
            ]
        );
    }

    /// The role doc's response format precedes the request's own block with
    /// placeholders of the same keys; the reply echoes only the real ones.
    #[test]
    fn the_mock_gate_reply_skips_the_response_format_placeholders() {
        let message = "forward_lifecycle: {target lifecycle}\n\
             forward_state: {target lifecycle}\n\
             - id: {uuid}\n\
             gate_check:\n  forward_state: done\n";
        let reply = mock_gate_check_reply(message);
        assert!(reply.contains("forward_lifecycle: done\n"), "{reply}");
        assert!(!reply.contains("gate_results"), "{reply}");
    }

    #[test]
    fn mock_session_replies_with_gate_check_yaml() {
        let mut mock = MockAgentProvider::new();
        let message = "- **phase_purpose:** gate_check\n\n\
             ```yaml\n\
             gate_check:\n  \
             forward_state: planning\n  \
             criteria:\n    \
             - id: a1000001-0001-4001-8001-000000000001\n      \
             slug: design-planning.done-criteria-clear\n      \
             label: \"Do I know what done looks like?\"\n  \
             prior_evaluations: []\n\
             ```\n";
        let run = mock
            .send_session_turn(session_turn(
                "gate-1",
                Some(crate::provider::SessionOpening { context: None }),
                None,
                message,
                SessionPurpose::Chat,
            ))
            .unwrap();
        let AgentRunState::Success(Some(reply)) = poll_run(&mut mock, run.id) else {
            panic!("gate check turn failed");
        };
        assert!(reply.starts_with("result: pass"), "{reply}");
        assert!(reply.contains("forward_lifecycle: planning"), "{reply}");
        assert!(
            reply.contains("criterion_id: a1000001-0001-4001-8001-000000000001"),
            "{reply}"
        );
    }

    #[test]
    fn mock_session_resumes_a_recorded_session_id() {
        let mut mock = MockAgentProvider::new();
        mock.send_session_turn(session_turn(
            "run-8",
            None,
            Some("earlier-session"),
            "again",
            SessionPurpose::Chat,
        ))
        .unwrap();
        assert_eq!(mock.session_id("run-8").as_deref(), Some("earlier-session"));
        mock.close_session("run-8");
        assert_eq!(mock.session_id("run-8"), None);
    }

    /// The one handler these tests register. The slot is process-wide and
    /// tests run in parallel, so every test must register the same behavior.
    fn echo_handler(turn: &MockInterviewTurn) -> Result<MockReply> {
        let actor = turn
            .env
            .iter()
            .find(|(k, _)| k == "TOD_INTERVIEW_ACTOR")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        Ok(format!("{:?} {actor} {}", turn.purpose, turn.blocks.join("|")).into())
    }

    #[test]
    fn interview_turns_run_the_registered_handler_with_env() {
        set_mock_interview_handler(Arc::new(echo_handler));
        let mut mock = MockAgentProvider::new();
        let run = mock
            .send_session_turn(session_turn(
                "qm-1",
                None,
                None,
                "Target open questions: 8.",
                SessionPurpose::QuestionMaker,
            ))
            .unwrap();
        let AgentRunState::Success(Some(reply)) = poll_run(&mut mock, run.id) else {
            panic!("interview turn failed");
        };
        assert_eq!(reply, "QuestionMaker actor-1 Target open questions: 8.");
    }

    /// Conversation turns are played by the registered handler, like the
    /// interview purposes, not by the canned chat reply.
    #[test]
    fn conversation_turns_run_the_registered_handler() {
        set_mock_interview_handler(Arc::new(echo_handler));
        let mut mock = MockAgentProvider::new();
        let run = mock
            .send_session_turn(session_turn(
                "conversation-1",
                None,
                None,
                "add obligation x: y",
                SessionPurpose::Conversation,
            ))
            .unwrap();
        let AgentRunState::Success(Some(reply)) = poll_run(&mut mock, run.id) else {
            panic!("conversation turn failed");
        };
        assert_eq!(reply, "Conversation actor-1 add obligation x: y");
        assert_eq!(
            SessionPurpose::Conversation.run_kind(),
            AgentRunKind::FleetAgent
        );
    }

    /// A `permission` line holds the turn until the request is answered.
    #[test]
    fn a_permission_line_holds_the_turn_until_answered() {
        set_mock_interview_handler(Arc::new(echo_handler));
        let mut mock = MockAgentProvider::new();
        let run = mock
            .send_session_turn(session_turn(
                "conversation-2",
                None,
                None,
                "permission Edit `/elsewhere/Cargo.toml`",
                SessionPurpose::Conversation,
            ))
            .unwrap();
        let AgentRunState::NeedsPermission(request) = poll_run(&mut mock, run.id) else {
            panic!("expected a permission request");
        };
        assert_eq!(request.title, "Edit `/elsewhere/Cargo.toml`");
        mock.respond_to_permission(run.id, "allow").unwrap();
        assert!(matches!(
            poll_run(&mut mock, run.id),
            AgentRunState::Success(Some(_))
        ));
    }
}
