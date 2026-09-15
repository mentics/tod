//! Shared fixtures for interview tests: a fresh store with one Spec-enabled
//! node and an active requirements interview session on it.

use crate::interview::context::{ContextScope, delta, snapshot};
use crate::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::interview::*;
use tod_store::outline::{Capability, CreatePosition, KIND_REQUIREMENT, OutlineMutation};
use uuid::Uuid;

pub(crate) struct Fixture {
    pub root: PathBuf,
    pub fleet: Arc<FleetStore>,
    pub node: Uuid,
    pub session: Uuid,
}

pub(crate) fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("tod-interview-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let fleet = Arc::new(FleetStore::open(&root).unwrap());
    fleet
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "t".into(),
            title: "T".into(),
        })
        .unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Interview node".into(),
        })
        .unwrap();
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node,
            capabilities: vec![Capability::Spec],
        })
        .unwrap();
    let session = SessionStore::open(fleet.clone())
        .insert_session_with_metadata(
            NewInterviewSession {
                node_id: node,
                display_name: "Interview".into(),
                phase: "task-requirements-interview".into(),
            },
            InterviewSessionStatus::Active,
        )
        .unwrap()
        .id;
    Fixture {
        root,
        fleet,
        node,
        session,
    }
}

pub(crate) fn draft(question: &str) -> QuestionDraft {
    QuestionDraft {
        question: question.into(),
        options: vec!["Yes".into(), "No".into()],
        ..Default::default()
    }
}

impl Fixture {
    pub fn act(&self, actor: &str, command: InterviewCommand) -> anyhow::Result<Value> {
        self.fleet
            .interview(actor, command)
            .map_err(|err| anyhow::anyhow!("{err:#}"))
    }

    pub fn user(&self, command: InterviewCommand) -> Value {
        self.act(ACTOR_USER, command).unwrap()
    }

    /// Add a requirements-phase question as `actor`; returns its seq.
    pub fn question(&self, actor: &str, draft: QuestionDraft) -> i64 {
        let value = self
            .act(
                actor,
                InterviewCommand::AddQuestion {
                    node_id: self.node,
                    session_id: Some(self.session),
                    phase: Some(PHASE_REQUIREMENTS.into()),
                    draft,
                },
            )
            .unwrap();
        value["id"]
            .as_str()
            .unwrap()
            .trim_start_matches("q-")
            .parse()
            .unwrap()
    }

    pub fn answer(&self, seq: i64, option: Option<i64>, text: Option<&str>) {
        self.user(InterviewCommand::AnswerQuestion {
            node_id: self.node,
            seq,
            option,
            text: text.map(str::to_string),
            edited_text: None,
        });
    }

    pub fn exhaust(&self) {
        self.user(InterviewCommand::SetExhausted {
            session_id: self.session,
            reason: Some("nothing left".into()),
        });
    }

    pub fn outline(&self, mutation: OutlineMutation) {
        self.fleet.enqueue_outline(mutation).unwrap();
    }

    /// Add a requirement on the fixture node; returns its id.
    pub fn obligation(&self, body: &str) -> Uuid {
        self.obligation_with_phase(body, PHASE_REQUIREMENTS)
    }

    /// Add an obligation on the fixture node tagged with an explicit phase
    /// (e.g. `PHASE_DESIGN`); returns its id.
    pub fn obligation_with_phase(&self, body: &str, phase: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.outline(OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: self.node,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: phase.into(),
        });
        id
    }

    pub fn head(&self) -> i64 {
        self.fleet
            .read(|conn| InterviewRepo::new(conn).head_rev())
            .unwrap()
    }

    pub fn get_question(&self, seq: i64) -> InterviewQuestion {
        self.fleet
            .read(|conn| InterviewRepo::new(conn).get_question(self.node, seq))
            .unwrap()
            .unwrap()
    }

    fn scope<'a>(&'a self, role: Role, phase: &'a str) -> ContextScope<'a> {
        ContextScope {
            node_id: self.node,
            phase,
            role,
            interview_session_id: Some(self.session),
            data_root: &self.root,
            tod_cli: &self.root,
            answered_cap: 100,
        }
    }

    pub fn snapshot(&self, role: Role, phase: &str) -> String {
        self.fleet
            .read(|conn| snapshot(conn, &self.scope(role, phase)))
            .unwrap()
    }

    pub fn delta(&self, role: Role, phase: &str, since: i64, actor: &str) -> String {
        self.fleet
            .read(|conn| delta(conn, &self.scope(role, phase), since, actor))
            .unwrap()
    }
}
