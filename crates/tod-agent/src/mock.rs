use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState, RunId, SessionPurpose,
    SessionTurn,
};
use crate::agent_launch::AgentLaunchOptions;
use crate::agent_traffic::{
    AgentCategory, InterviewAgentCounts, SharedAgentTrafficLog, TrafficDirection,
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

/// Plays an interview agent for `--agent mock`. The transport cannot know how
/// interview data is stored, so the caller supplies the behavior.
pub type MockInterviewHandler = Arc<dyn Fn(&MockInterviewTurn) -> Result<String> + Send + Sync>;

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
    pending: HashMap<RunId, (AgentRunKind, mpsc::Receiver<Result<String, String>>)>,
    run_agent: HashMap<RunId, String>,
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
}

impl MockAgentProvider {
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
            pending: HashMap::new(),
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
        let (category, label) = match kind {
            AgentRunKind::QuestionMakerReplenishment => {
                (AgentCategory::QuestionMaker, "question-maker")
            }
            AgentRunKind::AnswerProcessor => (AgentCategory::AnswerProcessor, "answer-processor"),
            AgentRunKind::FleetAgent => (AgentCategory::Fleet, "fleet-agent"),
        };
        let agent_id = self
            .run_agent
            .get(&run_id)
            .cloned()
            .unwrap_or_else(|| format!("{run_id:?}"));
        log.lock()
            .expect("traffic log mutex")
            .record(category, agent_id, label, direction, content);
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
        let done: Vec<(RunId, AgentRunKind, Result<String, String>)> = self
            .pending
            .iter()
            .filter_map(|(id, (kind, rx))| rx.try_recv().ok().map(|r| (*id, *kind, r)))
            .collect();
        for (id, kind, result) in done {
            self.pending.remove(&id);
            let state = match result {
                Ok(text) => {
                    self.log_traffic(kind, id, TrafficDirection::Response, &text);
                    AgentRunState::Success(Some(text))
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
    ) -> Result<AgentRunHandle> {
        self.last_fleet_options = Some(options);
        self.last_fleet_session_title = Some(session_title);
        let preview: String = prompt.chars().take(200).collect();
        let reply = format!(
            "Fleet agent run complete (mock).\n\n\
             Owner: {owner_id}\n\
             Cwd: {}\n\n\
             Prompt preview:\n{preview}…",
            cwd.display()
        );
        let handle = self.finish(
            AgentRunKind::FleetAgent,
            Some(&prompt),
            AgentRunState::Success(Some(reply)),
        );
        self.run_agent
            .insert(handle.id, owner_id.to_string());
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
            });
        session.messages += 1;
        session.context_chars += request.len() as u64;
        let kind = turn.purpose.run_kind();

        if turn.purpose == SessionPurpose::Chat {
            let reply = mock_session_reply(session.messages, &turn);
            session.context_chars += reply.len() as u64;
            let handle = self.finish(kind, Some(&request), AgentRunState::Success(Some(reply)));
            self.run_agent.insert(handle.id, turn.owner_id);
            return Ok(handle);
        }

        let handler = handler_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(handler) = handler else {
            return Ok(self.finish(
                kind,
                Some(&request),
                AgentRunState::Failure("no mock interview handler registered".into()),
            ));
        };
        let id = RunId::new();
        self.run_agent.insert(id, turn.owner_id.clone());
        self.log_traffic(kind, id, TrafficDirection::Request, &request);
        let (tx, rx) = mpsc::channel();
        let mock_turn = MockInterviewTurn {
            purpose: turn.purpose,
            env: turn.env,
            blocks,
        };
        thread::spawn(move || {
            let _ = tx.send(handler(&mock_turn).map_err(|err| format!("{err:#}")));
        });
        self.runs.insert(id, AgentRunState::InFlight(None));
        self.pending.insert(id, (kind, rx));
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

    fn fetch_full_transcript(
        &self,
        _platform: crate::platform::AgentPlatform,
        _cwd: &std::path::Path,
        agent_session_id: &str,
    ) -> anyhow::Result<String> {
        Ok(format!(
            "User:\nmock prompt\n\nAssistant:\nmock reply (session {agent_session_id})"
        ))
    }

    fn session_context_chars(&self, key: &str) -> Option<u64> {
        self.sessions.get(key).map(|session| session.context_chars)
    }

    fn close_session(&mut self, key: &str) {
        self.sessions.remove(key);
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.drain_pending();
        self.runs.get(&id).cloned()
    }

    fn respond_to_permission(&mut self, _id: RunId, _option_id: &str) -> Result<()> {
        anyhow::bail!("mock agent never requests permission")
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        self.pending.remove(&id);
        self.runs.remove(&id);
        self.run_agent.remove(&id);
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        let mut counts = InterviewAgentCounts::default();
        for (kind, _) in self.pending.values() {
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
    if turn.message.contains("phase_purpose:** gate_check") {
        return mock_gate_check_reply(&turn.message);
    }

    let mut reply = format!("Mock session reply · message {message_number}\n\n");
    match &turn.opening {
        Some(opening) => {
            reply.push_str(&format!("- Session named **{}**\n", opening.title));
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
fn mock_gate_check_reply(message: &str) -> String {
    let forward_state = message
        .lines()
        .find_map(|line| line.trim().strip_prefix("forward_state:"))
        .map(str::trim)
        .unwrap_or("");
    let criterion_ids: Vec<&str> = message
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- id:"))
        .map(str::trim)
        .collect();

    let mut reply = format!(
        "result: pass\nforward_lifecycle: {forward_state}\npaused: false\nfindings: \"Mock gate check: pass.\"\n"
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
            cwd: PathBuf::from("."),
            options: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
            resume_session_id: resume_session_id.map(str::to_string),
            opening,
            message: message.into(),
            purpose,
            env: vec![("TOD_INTERVIEW_ACTOR".into(), "actor-1".into())],
        }
    }

    #[test]
    fn mock_session_sends_opening_once_and_keeps_its_id() {
        use crate::provider::SessionOpening;

        let mut mock = MockAgentProvider::new();
        let opening = SessionOpening {
            title: "Obligations · Demo".into(),
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
        assert!(reply.contains("Obligations · Demo"), "{reply}");
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
                Some(crate::provider::SessionOpening {
                    title: "design-to-planning gate".into(),
                    context: None,
                }),
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

    #[test]
    fn interview_turns_run_the_registered_handler_with_env() {
        set_mock_interview_handler(Arc::new(|turn: &MockInterviewTurn| {
            let actor = turn
                .env
                .iter()
                .find(|(k, _)| k == "TOD_INTERVIEW_ACTOR")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            Ok(format!("{:?} {actor} {}", turn.purpose, turn.blocks.join("|")))
        }));
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
}
