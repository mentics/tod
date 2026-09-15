//! Interview row types and the vocabulary stored in their text columns.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Environment variable carrying an interview agent session id into the agent
/// process, so `tod-cli` can attribute and guard that agent's writes.
pub const ACTOR_ENV: &str = "TOD_INTERVIEW_ACTOR";
/// Actor for writes made by the person using the app.
pub const ACTOR_USER: &str = "user";
/// Actor for writes from an agent that has no interview session of its own
/// (a chat agent, or `tod-cli` run without `TOD_INTERVIEW_ACTOR`).
pub const ACTOR_AGENT: &str = "agent";

pub const AUTHOR_QUESTION_MAKER: &str = "question-maker";
pub const AUTHOR_ANSWER_PROCESSOR: &str = "answer-processor";
pub const AUTHOR_DRAFTER: &str = "drafter";
pub const AUTHOR_USER: &str = "user";

pub const STATUS_OPEN: &str = "open";
pub const STATUS_ANSWERED: &str = "answered";
pub const STATUS_DEFERRED: &str = "deferred";
pub const STATUS_WITHDRAWN: &str = "withdrawn";

pub const MEMORY_CONTEXT: &str = "context";
pub const MEMORY_HANDOFF: &str = "handoff";
pub const MEMORY_PARKED: &str = "parked";
pub const MEMORY_PLAN: &str = "plan";
pub const MEMORY_KINDS: [&str; 4] = [MEMORY_CONTEXT, MEMORY_HANDOFF, MEMORY_PARKED, MEMORY_PLAN];
pub const MEMORY_OPEN: &str = "open";
pub const MEMORY_DONE: &str = "done";

pub const PHASE_REQUIREMENTS: &str = "requirements";
pub const PHASE_DESIGN: &str = "design";
pub const PHASE_PLANNING: &str = "planning";
pub const PHASES: [&str; 3] = [PHASE_REQUIREMENTS, PHASE_DESIGN, PHASE_PLANNING];

/// Sentinel phase for obligations that predate phase-tagging (migrated rows,
/// imported docs). Never a valid target for creating a new obligation, but a
/// valid target for an explicit phase change.
pub const PHASE_UNKNOWN: &str = "unknown";
/// Valid phase values for an obligation, including the `unknown` sentinel.
/// `planning` is deliberately excluded: obligations are a requirements/design
/// artifact, while planning produces structured plan steps
/// (`outline::PlanStepRepo`) instead. Historical obligations tagged `planning`
/// from before this split still read back fine — this only gates creation.
pub const OBLIGATION_PHASES: [&str; 3] = [PHASE_UNKNOWN, PHASE_REQUIREMENTS, PHASE_DESIGN];

pub const QUESTION_MAKER_IDLE: &str = "idle";
pub const QUESTION_MAKER_EXHAUSTED: &str = "exhausted";

/// Reason recorded when the app withdraws a question whose proposal points
/// at an obligation that no longer exists (`withdrawn_by` is NULL).
pub const STALE_PROPOSAL_REASON: &str =
    "Its proposal refers to an obligation that no longer exists.";

/// Reason recorded when the user resets the question queue (`withdrawn_by` is NULL).
pub const RESET_QUESTIONS_REASON: &str = "The user reset all questions.";

pub const ENTITY_QUESTION: &str = "question";
pub const ENTITY_MEMORY: &str = "memory";
pub const ENTITY_OBLIGATION: &str = "obligation";
pub const ENTITY_CONTENT: &str = "content";
pub const ENTITY_PLAN_STEP: &str = "plan_step";
pub const ENTITY_PLAN_STEP_DEP: &str = "plan_step_dep";
pub const ENTITY_PLAN_STEP_OBLIGATION: &str = "plan_step_obligation";

/// An interview agent role. `Drafter` is the drafting (v3) agent; the other
/// two belong to the v2 question interview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    QuestionMaker,
    AnswerProcessor,
    Drafter,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuestionMaker => AUTHOR_QUESTION_MAKER,
            Self::AnswerProcessor => AUTHOR_ANSWER_PROCESSOR,
            Self::Drafter => AUTHOR_DRAFTER,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            AUTHOR_QUESTION_MAKER => Some(Self::QuestionMaker),
            AUTHOR_ANSWER_PROCESSOR => Some(Self::AnswerProcessor),
            AUTHOR_DRAFTER => Some(Self::Drafter),
            _ => None,
        }
    }

    /// Whether a memory note of `kind` is part of this role's context.
    pub fn sees_memory(self, kind: &str) -> bool {
        if self == Self::Drafter {
            return false;
        }
        match kind {
            MEMORY_CONTEXT | MEMORY_PARKED => true,
            MEMORY_HANDOFF | MEMORY_PLAN => self == Self::QuestionMaker,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalOp {
    Add,
    Update,
    Delete,
    Content,
}

/// A change applied when the user picks option 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub op: ProposalOp,
    /// `requirement` | `constraint` (add).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Target node for add; defaults to the interview node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<Uuid>,
    /// Obligation id (update / delete). A unique id prefix is accepted when
    /// the question is added and stored as the full id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `goal` | `design` | `plan` (content).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub append: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
}

/// A question as an agent writes it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QuestionDraft {
    #[serde(default)]
    pub covers: Vec<String>,
    #[serde(default)]
    pub context: Option<String>,
    pub question: String,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub recommend: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub proposal: Option<Proposal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterviewQuestion {
    pub id: Uuid,
    pub node_id: Uuid,
    pub session_id: Option<Uuid>,
    pub seq: i64,
    pub phase: String,
    pub author: String,
    pub status: String,
    pub covers: Vec<String>,
    pub context: Option<String>,
    pub question: Option<String>,
    pub intent: Option<String>,
    pub recommend: Option<String>,
    pub options: Vec<String>,
    pub proposal: Option<Proposal>,
    pub answer_option: Option<i64>,
    pub answer_text: Option<String>,
    pub answer_edited_text: Option<String>,
    pub applied: Option<serde_json::Value>,
    pub processed_at: Option<i64>,
    pub processed_summary: Option<String>,
    pub withdrawn_by: Option<String>,
    pub withdrawn_reason: Option<String>,
    pub created_at: i64,
    pub answered_at: Option<i64>,
    pub updated_at: i64,
}

impl InterviewQuestion {
    pub fn label(&self) -> String {
        format!("q-{}", self.seq)
    }

    pub fn is_open(&self) -> bool {
        self.status == STATUS_OPEN
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryNote {
    pub id: Uuid,
    pub node_id: Uuid,
    pub seq: i64,
    pub kind: String,
    pub phase: Option<String>,
    pub status: String,
    pub author: String,
    pub question_seq: Option<i64>,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl MemoryNote {
    pub fn label(&self) -> String {
        format!("m-{}", self.seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRow {
    pub id: Uuid,
    pub node_id: Uuid,
    pub interview_session_id: Option<Uuid>,
    pub phase: String,
    pub role: Role,
    pub lane: i64,
    pub agent_session_id: Option<String>,
    pub synced_rev: i64,
    pub est_tokens: i64,
    pub snapshot_tokens: i64,
    pub turns: i64,
    pub live: bool,
    pub created_at: i64,
    pub last_turn_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRow {
    pub rev: i64,
    pub node_id: Uuid,
    pub entity: String,
    pub entity_id: Uuid,
    pub op: String,
    pub fields: Vec<String>,
    pub actor: String,
}

/// Map a session phase key (`task-requirements-interview`, …, optionally with
/// a parenthesised suffix) to the stored phase.
pub fn phase_for_session_key(key: &str) -> &'static str {
    let base = key.split('(').next().unwrap_or(key).trim();
    match base {
        "design-interview" => PHASE_DESIGN,
        "planning-interview" => PHASE_PLANNING,
        _ => PHASE_REQUIREMENTS,
    }
}
