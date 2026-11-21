//! Decisions: what the user answers. A structured agent (see
//! `doc/ui/unified-view.md` "Structured agents") records a question with
//! options and evidence links through `tod-cli decisions ask`; the user (or
//! an agent addressing them on the user's behalf) answers it. An answer never
//! updates or deletes a prior one — a change of mind is a new
//! `decision_answers` row, so the log stays append-only and it is plain the
//! user changed their mind (`doc/ui/unified-view.md` "Decisions").

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// Not yet answered, and waiting on the user (`doc/requests/design.md` 8.1).
pub const DECISION_PENDING: &str = "pending";
/// Filed by an agent for triage: a fresh session reads it cold and answers it,
/// converts it, or escalates it (then it becomes `pending`). The node is
/// `Working`, not `You`, meanwhile (`tod_core::next_actor`), and no list of
/// the user's requests holds it.
pub const DECISION_TRIAGE: &str = "triage";
/// Has at least one answer.
pub const DECISION_ANSWERED: &str = "answered";
/// Withdrawn: no longer needs an answer.
pub const DECISION_WITHDRAWN: &str = "withdrawn";

/// Every decision status.
pub const DECISION_STATUSES: [&str; 4] = [DECISION_PENDING, DECISION_TRIAGE, DECISION_ANSWERED, DECISION_WITHDRAWN];

/// Whether a decision with this status still waits for an answer, from the
/// user or from triage: the asking agent holds its turn for it either way.
pub fn is_open(status: &str) -> bool {
    status == DECISION_PENDING || status == DECISION_TRIAGE
}

/// The reasons whose request an agent files for triage rather than straight to
/// the user (`doc/requests/design.md` 7.2); access, broken and capability go
/// to the user, rendered by the app (7.1).
pub const TRIAGED_REASONS: [&str; 5] = [REASON_INTENT, REASON_MISSING_RULE, REASON_CONFLICT, REASON_RISK, REASON_OTHER];

/// The protocols whose agents' judgment questions are triaged: the lifecycle's
/// own sessions. A freeform chat (the user is already talking to it), the
/// incident investigation and the app's own stop questions go to the user.
pub const TRIAGED_PROTOCOLS: [&str; 6] = ["implementation", "verification", "review", "fix", "phase", "pr"];

/// Whether triage is on (`lifecycle.triage` in the settings of the data root
/// `conn`'s database lives in; `tod.db` sits directly at it). On for a database
/// with no file (in memory) and when no settings file exists. A settings file
/// that cannot be read is an error, not a guess: a request is never filed
/// under a setting nobody could read.
pub fn triage_enabled(conn: &Connection) -> Result<bool> {
    let root = conn.path().filter(|p| !p.is_empty()).and_then(|p| std::path::Path::new(p).parent());
    let Some(root) = root else { return Ok(true) };
    Ok(crate::settings::TodSettings::load_from_path(&crate::paths::TodPaths::at(root).settings_path())?.lifecycle.triage)
}

/// Whether an agent's ask with this `reason` from this `protocol` is filed for
/// triage. `guard_reask` is false for the app's own questions, never triaged.
pub fn goes_to_triage(protocol: Option<&str>, reason: &str, guard_reask: bool) -> bool {
    let reason = if reason.trim().is_empty() { REASON_OTHER } else { reason.trim() };
    guard_reask
        && TRIAGED_REASONS.contains(&reason)
        && protocol.is_some_and(|p| TRIAGED_PROTOCOLS.contains(&p))
}

/// The kinds of thing a decision's evidence can point at.
pub const EVIDENCE_KINDS: [&str; 6] = [
    "obligation",
    "plan_step",
    "test_run",
    "conversation",
    "finding",
    "node",
];

/// No obligation, plan step, or process doc settles it.
pub const REASON_MISSING_RULE: &str = "missing_rule";
/// Obligations or other requirements that cannot all hold.
pub const REASON_CONFLICT: &str = "conflict";
/// A secret, account, or permission the agent does not have.
pub const REASON_ACCESS: &str = "access";
/// A judgment call worth a human's sign-off.
pub const REASON_RISK: &str = "risk";
/// About a capability's own configuration.
pub const REASON_CAPABILITY: &str = "capability";
/// What the user wants is not stated anywhere: the node's purpose, scope,
/// or a preference only they can give.
pub const REASON_INTENT: &str = "intent";
/// Something tod or the work's environment provides is broken (a command
/// fails, a credential is rejected, a tool is missing): the agent will not
/// work around it, and the user needs to fix it.
pub const REASON_BROKEN: &str = "broken";
/// Does not fit the other reasons.
pub const REASON_OTHER: &str = "other";

/// Every reason a decision can be asked with (`doc/ui/task-panel.md`
/// "Requests"); mirrored by `tod_core::attention::RequestReason`, which this
/// crate cannot depend on (policy depends on transport, never the reverse).
pub const DECISION_REASONS: [&str; 8] = [
    REASON_INTENT,
    REASON_MISSING_RULE,
    REASON_CONFLICT,
    REASON_ACCESS,
    REASON_RISK,
    REASON_CAPABILITY,
    REASON_BROKEN,
    REASON_OTHER,
];

/// The reasons whose request the app renders from structured fields
/// ([`Brief`], `tod_core::request_brief`).
pub const BRIEF_REASONS: [&str; 3] = [REASON_ACCESS, REASON_BROKEN, REASON_CAPABILITY];

/// The options an access, broken or capability request gets when the agent
/// gives none: it was done, or the user will not do it.
pub const DEFAULT_BRIEF_OPTIONS: [&str; 2] = ["Done, continue", "Do not do it"];

/// The structured fields of an access, broken or capability request
/// (`decisions ask --need --command --error --tried --after`), which the app
/// renders from one template (`tod_core::request_brief`). All empty for any
/// other request, and for one asked before these fields existed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Brief {
    /// What is needed, in one line.
    #[serde(default)]
    pub need: String,
    /// The exact command to paste, every value filled in.
    #[serde(default)]
    pub command: String,
    /// The exact error (required for `broken`).
    #[serde(default)]
    pub error: String,
    /// What the agent tried, one line per attempt.
    #[serde(default)]
    pub tried: Vec<String>,
    /// What the agent will do or check once the user is done.
    #[serde(default)]
    pub after: String,
}

impl Brief {
    pub fn is_empty(&self) -> bool {
        self.need.trim().is_empty()
            && self.command.trim().is_empty()
            && self.error.trim().is_empty()
            && self.tried.iter().all(|t| t.trim().is_empty())
            && self.after.trim().is_empty()
    }

    fn trimmed(&self) -> Brief {
        Brief {
            need: self.need.trim().to_string(),
            command: self.command.trim().to_string(),
            error: self.error.trim().to_string(),
            tried: self.tried.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            after: self.after.trim().to_string(),
        }
    }
}

/// What picking an option does besides telling the agent: the app moves the
/// node back to the lifecycle state `to`, as the task panel's state chip does
/// (`tod_core::lifecycle::send_back`). A person answering "send the task back
/// to active" is deciding to move the node, and no agent has a way to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OptionMove {
    /// 1-based, as options are numbered to the user.
    pub option: i64,
    /// The lifecycle state the node is sent back to.
    pub to: String,
    /// The state the node was in when the question was asked, recorded by
    /// `DecisionRepo::create`. The move describes that node; once the node
    /// has gone on (the gate advanced it, a person moved it) it no longer
    /// does, and answering it is refused. Rows made before this was
    /// recorded have none, and are not refused.
    #[serde(default)]
    pub asked_in: Option<String>,
}

/// A change to an obligation's text that picking an option stands for. An
/// agent past planning may not edit its node's obligations (only the user
/// may), so when an option changes the approach the obligations it names must
/// change with it: the option carries the new text, the user sees it with the
/// option, and answering applies it as the user's own edit
/// (`DecisionRepo::answer_edits`), so the agent never asks a second time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OptionEdit {
    /// 1-based, as options are numbered to the user.
    pub option: i64,
    /// The obligation of the decision's node whose text changes.
    pub obligation_id: Uuid,
    /// The text it gets.
    pub body: String,
    /// The text it had when the question was asked, recorded by
    /// `DecisionRepo::create`. An obligation that has changed since (by
    /// someone else) no longer matches what the user was shown, and
    /// answering is refused.
    #[serde(default)]
    pub was: Option<String>,
    /// Picking the option approves the obligation as it stands
    /// (`--affirm-obligation`, `doc/requests/design.md` 10.1) instead of
    /// changing its text: it becomes the user's (`approved`), so no agent
    /// changes or removes it. `body` is empty then.
    #[serde(default)]
    pub affirm: bool,
}

/// The first lifecycle state a decision's option may not move a node into:
/// from here on the work is out on a pull request, and only the node's own
/// run (or the user's state chip) takes it there.
pub const FIRST_UNREACHABLE_STATE: &str = "pr_review";

fn state_rank(state: &str) -> Option<usize> {
    crate::settings::VALID_LIFECYCLE_STATES.iter().position(|s| *s == state)
}

/// Whether an option may send a node back to `state`: a real lifecycle state
/// before [`FIRST_UNREACHABLE_STATE`].
pub fn check_move_target(state: &str) -> Result<()> {
    let Some(rank) = state_rank(state) else {
        bail!(
            "`{state}` is not a lifecycle state (expected one of {})",
            crate::settings::VALID_LIFECYCLE_STATES.join(", ")
        );
    };
    if rank >= state_rank(FIRST_UNREACHABLE_STATE).unwrap_or(usize::MAX) {
        bail!("an option cannot move a node into `{state}`: only states before {FIRST_UNREACHABLE_STATE} can be sent back to");
    }
    Ok(())
}

/// A typed pointer to whatever backs up a decision's question: an
/// obligation, a plan step, a test run, a conversation, a review finding, or
/// another node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: Uuid,
    pub node_id: Uuid,
    /// The conversation that asked, while that conversation exists.
    pub conversation_id: Option<Uuid>,
    /// The protocol running that conversation, e.g. `implement`, `verify`.
    pub protocol: Option<String>,
    pub question: String,
    pub options: Vec<String>,
    pub evidence: Vec<EvidenceRef>,
    pub status: String,
    pub created_at: i64,
    /// Why the agent could not settle this itself, one of
    /// [`DECISION_REASONS`].
    pub reason: String,
    /// The options that move the node when picked.
    pub moves: Vec<OptionMove>,
    /// The obligation edits options carry, applied when the option is picked.
    pub edits: Vec<OptionEdit>,
    /// The plan steps this decision holds up (`decisions ask --blocks`, or
    /// `plan update --decision`): one cause, one request. Answering the
    /// decision reopens the ones still handed back, and retrying one of
    /// them resolves the decision (see [`DecisionRepo::answer`] and
    /// [`DecisionRepo::release_for_step`]).
    pub blocks: Vec<Uuid>,
    /// What the agent said was new when it asked again after an answer
    /// (`decisions ask --new-evidence`); shown to the user with the request.
    pub new_evidence: String,
    /// The structured fields of an access, broken or capability request
    /// (empty for older ones and for other reasons).
    pub brief: Brief,
    /// [`crate::failure_signature::of_text`] of `brief.error`; empty when
    /// there is no error. Another request about the same failure, and an
    /// incident, match on it.
    pub error_signature: String,
    /// The explicit cause the agent named (`decisions ask --cause`, schema
    /// v114), lowercased and trimmed; empty when it named none. Requests on a
    /// node that share it, or the same failure, are read by one triage
    /// session ([`DecisionRepo::triage_group`]).
    pub cause: String,
    /// The triage session that read this request (schema v112), set once one
    /// started; it stays linked after the request is escalated (the user's
    /// chat partner for it) and after it is answered (the evidence).
    pub triage_conversation_id: Option<Uuid>,
    /// What the asking agent wrote, kept when triage replaced it with a
    /// brief (`question`, `options`); `None` while it stands as asked.
    pub asked: Option<Asked>,
}

/// The asking agent's own words, kept when triage escalates with a brief.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Asked {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

impl Decision {
    /// This request is waiting on triage, not the user.
    pub fn in_triage(&self) -> bool {
        self.status == DECISION_TRIAGE
    }

    /// Whether this request may be read, and answered, together with others
    /// that share its cause: not one whose options move the task or edit
    /// obligations, which only the user decides.
    pub fn groupable(&self) -> bool {
        self.moves.is_empty() && self.edits.is_empty()
    }

    /// Whether `other` has this request's cause: the same explicit `cause` key.
    /// The other ways the app knows two requests are one are not grouping: the
    /// same question, the same held-up plan steps and the same failure
    /// (`error_signature`, which only an access, broken or capability request,
    /// never triaged, carries) are refused at ask time as duplicates
    /// ([`Cause`]).
    pub fn shares_cause_with(&self, other: &Decision) -> bool {
        !self.cause.is_empty() && self.cause == other.cause
    }

    /// The state picking `option` (1-based) moves the node back to, if it does.
    pub fn move_for(&self, option: i64) -> Option<&str> {
        self.moves.iter().find(|m| m.option == option).map(|m| m.to.as_str())
    }

    /// The obligation edits picking `option` (1-based) makes.
    pub fn edits_for(&self, option: i64) -> Vec<&OptionEdit> {
        self.edits.iter().filter(|e| e.option == option).collect()
    }

    /// The state the node was in when picking `option` was offered, if known.
    pub fn asked_in_for(&self, option: i64) -> Option<&str> {
        self.moves.iter().find(|m| m.option == option).and_then(|m| m.asked_in.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionAnswer {
    pub id: i64,
    pub decision_id: Uuid,
    /// 1-based index into the decision's `options`, when the answer picked
    /// one.
    pub option: Option<i64>,
    /// Free text, when the answer was not (only) a pick.
    pub text: Option<String>,
    /// `user`, or the agent role that answered on the user's behalf.
    pub actor: String,
    pub answered_at: i64,
    /// Who decided: [`DECIDED_BY_USER`], [`DECIDED_BY_TRIAGE`] or
    /// [`DECIDED_BY_TOD`] (schema v106).
    pub decided_by: String,
    /// The triage session whose transcript is the evidence for the answer.
    pub conversation_id: Option<Uuid>,
    /// For an answer not made by the user: how it was decided, in a line or
    /// two (what tod tried, or what triage found).
    pub rationale: Option<String>,
}

impl DecisionAnswer {
    /// The answer was made by tod or triage, not by the user.
    pub fn is_automatic(&self) -> bool {
        self.decided_by != DECIDED_BY_USER
    }
}

/// A decision with its full answer history, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionWithAnswers {
    pub decision: Decision,
    pub answers: Vec<DecisionAnswer>,
}

/// What an agent submits when asking.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewDecision {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    /// One of [`DECISION_REASONS`]; empty defaults to `other`.
    #[serde(default)]
    pub reason: String,
    /// Options that send the node back to an earlier state when picked
    /// (`tod-cli decisions ask --moves-to`). Each `option` names one of
    /// `options`, each `to` a state before the node's current one and before
    /// `pr_review`.
    #[serde(default)]
    pub moves: Vec<OptionMove>,
    /// Obligation edits options carry (`--edit-obligation`): each `option`
    /// names one of `options`, each `obligation_id` an obligation of the node.
    #[serde(default)]
    pub edits: Vec<OptionEdit>,
    /// Plan steps of the node this decision holds up (`--blocks`).
    #[serde(default)]
    pub blocks: Vec<Uuid>,
    /// Set when the asker is an agent that must not repeat an answered
    /// question: `create` then refuses one answered on the node within
    /// [`REASK_WINDOW_MS`] (or whose `blocks` an answer released in that
    /// window) unless `new_evidence` says what changed. Off for the app's own
    /// questions (a permission, a silent bot, a broken tool).
    #[serde(default)]
    pub guard_reask: bool,
    /// What is new since the answer (`--new-evidence`): lets a guarded ask
    /// through.
    #[serde(default)]
    pub new_evidence: String,
    /// The structured fields (`--need --command --error --tried --after`),
    /// only for the reasons in [`BRIEF_REASONS`].
    #[serde(default)]
    pub brief: Brief,
    /// File it with status `triage` (set by the ask command from
    /// [`goes_to_triage`], never by the caller of `tod-cli`).
    #[serde(default)]
    pub triage: bool,
    /// What these requests share (`--cause <KEY>`): requests on one node that
    /// give the same key are read, and settled, by one triage session. Empty
    /// for none.
    #[serde(default)]
    pub cause: String,
}

/// `decision_answers.decided_by`: the user picked an option or settled it in
/// a chat.
pub const DECIDED_BY_USER: &str = "user";
/// Triage answered without the user.
pub const DECIDED_BY_TRIAGE: &str = "triage";
/// tod answered mechanically (a bot that stayed silent, a failure the base
/// branch has too), with the rationale saying what was tried.
pub const DECIDED_BY_TOD: &str = "tod";

/// Protocols whose requests tod settles itself, mechanically
/// ([`DecisionRepo::answer_settled`]): nothing else may be answered as tod.
pub const SETTLEABLE_PROTOCOLS: &[&str] = &["bot_silence", "check_unrelated"];

/// The actor of an answer the app recorded, not the user or an agent. Such an
/// answer is never shown to an agent as the user's, and never counts against
/// asking again.
pub const SETTLED_BY: &str = "tod";

/// The actor a triage session's answers are recorded under.
pub const TRIAGE_ACTOR: &str = "triage";

/// How recent an answer is for the agent's next turn to carry it
/// (`tod_core::node_context::render_recent_answers`): 30 minutes, by the
/// clock. It is no longer a limit on asking again: an answered request stands
/// against a repeat at any age (see `DecisionRepo::create`).
pub const REASK_WINDOW_MS: i64 = 30 * 60 * 1000;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS decisions (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        protocol        TEXT,
        question        TEXT NOT NULL,
        options         TEXT NOT NULL DEFAULT '[]',
        evidence        TEXT NOT NULL DEFAULT '[]',
        status          TEXT NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending','triage','answered','withdrawn')),
        created_at      INTEGER NOT NULL
        -- `reason` is added by a later migration (schema v71),
        -- which runs after this on every install, fresh or upgraded.
    );
    CREATE INDEX IF NOT EXISTS idx_decisions_node ON decisions(node_id, created_at);

    CREATE TABLE IF NOT EXISTS decision_answers (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        decision_id  BLOB NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
        option_index INTEGER,
        text         TEXT,
        actor        TEXT NOT NULL,
        answered_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_decision_answers_decision ON decision_answers(decision_id, id);
";

const DECISION_COLUMNS: &str =
    "id, node_id, conversation_id, protocol, question, options, evidence, status, created_at, reason, option_moves, blocks, new_evidence, option_edits, brief_need, brief_command, brief_error, brief_tried, brief_after, error_signature, triage_conversation_id, asked, cause";

/// Schema v112: `decisions.status` may be `triage`, and a decision records its
/// triage session and what the asking agent wrote when triage replaced it.
/// SQLite cannot alter a CHECK, so an older table is rebuilt from its own
/// stored definition with the status list widened, every row (and rowid
/// order) kept. Idempotent; the journey triggers go with the old table and
/// are remade by the caller, the sync triggers by `sync::install`.
pub fn ensure_triage(conn: &Connection) -> Result<()> {
    const OLD: &str = "'pending','answered','withdrawn')";
    const NEW: &str = "'pending','triage','answered','withdrawn')";
    let sql: Option<String> = conn
        .query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'decisions'", [], |r| r.get(0))
        .optional()?;
    let Some(sql) = sql else { return Ok(()) };
    if sql.contains(OLD) {
        let indexes: Vec<String> = conn
            .prepare("SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'decisions' AND sql IS NOT NULL")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'decisions' AND sql IS NOT NULL")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let tx = conn.unchecked_transaction()?;
        for name in &names {
            tx.execute_batch(&format!("DROP INDEX IF EXISTS {name};"))?;
        }
        tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
        tx.execute_batch("ALTER TABLE decisions RENAME TO decisions_v112;")?;
        tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
        tx.execute_batch(&sql.replace(OLD, NEW))?;
        tx.execute_batch("INSERT INTO decisions SELECT * FROM decisions_v112 ORDER BY rowid; DROP TABLE decisions_v112;")?;
        for index in &indexes {
            tx.execute_batch(index)?;
        }
        tx.commit()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(&crate::journey_changes::decisions_triggers_sql())?;
    }
    let has = |name: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?)
    };
    if !has("triage_conversation_id")? {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN triage_conversation_id BLOB;")?;
    }
    if !has("asked")? {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN asked TEXT;")?;
    }
    Ok(())
}

/// Schema v114: `decisions.cause`, the key requests that share one cause give
/// (`decisions ask --cause`). A no-op when the column is there. The table is
/// journaled and synced already (the columns follow it).
pub fn ensure_cause(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = 'cause'",
        [],
        |row| row.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN cause TEXT NOT NULL DEFAULT '';")?;
    }
    Ok(())
}

/// The structured request fields and the error signature
/// (`decisions.brief_*`, `error_signature`, schema v107); a no-op for each
/// column already there.
pub fn add_brief(conn: &Connection) -> Result<()> {
    for (name, ddl) in [
        ("brief_need", "TEXT NOT NULL DEFAULT ''"),
        ("brief_command", "TEXT NOT NULL DEFAULT ''"),
        ("brief_error", "TEXT NOT NULL DEFAULT ''"),
        ("brief_tried", "TEXT NOT NULL DEFAULT '[]'"),
        ("brief_after", "TEXT NOT NULL DEFAULT ''"),
        ("error_signature", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let has: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE decisions ADD COLUMN {name} {ddl};"))?;
        }
    }
    Ok(())
}

/// `decisions.option_moves` (an option that sends the node back); a no-op
/// when the column is already there.
pub fn add_option_moves(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = 'option_moves'",
        [],
        |row| row.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN option_moves TEXT NOT NULL DEFAULT '[]';")?;
    }
    Ok(())
}

/// `decisions.option_edits` (obligation edits an option carries); a no-op
/// when the column is already there.
pub fn add_option_edits(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = 'option_edits'",
        [],
        |row| row.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN option_edits TEXT NOT NULL DEFAULT '[]';")?;
    }
    Ok(())
}

/// `decisions.blocks` (the plan steps a decision holds up, JSON ids); a no-op
/// when the column is already there.
pub fn add_blocks(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = 'blocks'",
        [],
        |row| row.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN blocks TEXT NOT NULL DEFAULT '[]';")?;
    }
    Ok(())
}

/// `decisions.new_evidence` (what was new when an answered question was asked
/// again); a no-op when the column is already there.
pub fn add_new_evidence(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('decisions') WHERE name = 'new_evidence'",
        [],
        |row| row.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE decisions ADD COLUMN new_evidence TEXT NOT NULL DEFAULT '';")?;
    }
    Ok(())
}

const ANSWER_COLUMNS: &str =
    "id, decision_id, option_index, text, actor, answered_at, decided_by, conversation_id, rationale";

/// `decision_answers.decided_by`, `.conversation_id` and `.rationale`
/// (schema v106: who decided, the triage session behind it, and how); a no-op
/// when the columns are already there. Synced (sync copies the columns both
/// sides have); the table is journaled already.
pub fn add_answer_columns(conn: &Connection) -> Result<()> {
    let has = |name: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('decision_answers') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?)
    };
    if !has("decided_by")? {
        conn.execute_batch(
            "ALTER TABLE decision_answers ADD COLUMN decided_by TEXT NOT NULL DEFAULT 'user'
                 CHECK (decided_by IN ('user','triage','tod'));",
        )?;
    }
    if !has("conversation_id")? {
        conn.execute_batch("ALTER TABLE decision_answers ADD COLUMN conversation_id BLOB;")?;
    }
    if !has("rationale")? {
        conn.execute_batch("ALTER TABLE decision_answers ADD COLUMN rationale TEXT;")?;
    }
    Ok(())
}

/// What a request is about, for telling whether the node has asked it before.
struct Cause<'a> {
    question: &'a str,
    blocks: &'a [Uuid],
    signature: &'a str,
}

fn normal_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn same_steps(a: &[Uuid], b: &[Uuid]) -> bool {
    !a.is_empty() && a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

impl Cause<'_> {
    fn matches(&self, other: &Decision) -> bool {
        !self.why(other).is_empty()
    }

    /// How `other` is the same cause, or empty when it is not.
    fn why(&self, other: &Decision) -> &'static str {
        if normal_text(&other.question) == normal_text(self.question) {
            "the same question"
        } else if same_steps(self.blocks, &other.blocks) {
            "it holds up the same plan steps"
        } else if !self.signature.is_empty() && other.error_signature == self.signature {
            "the same failure"
        } else {
            ""
        }
    }
}

/// `5 minutes`, `3 hours`, `2 days`.
fn age_label(ms: i64) -> String {
    let minutes = ms / 60_000;
    let (n, unit) = if minutes >= 2 * 24 * 60 {
        (minutes / (24 * 60), "day")
    } else if minutes >= 120 {
        (minutes / 60, "hour")
    } else {
        (minutes, "minute")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

pub struct DecisionRepo<'a> {
    conn: &'a Connection,
}

impl<'a> DecisionRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record a new pending decision on `node_id`.
    pub fn create(
        &self,
        node_id: Uuid,
        conversation_id: Option<Uuid>,
        protocol: Option<&str>,
        decision: &NewDecision,
    ) -> Result<Decision> {
        let question = decision.question.trim();
        if question.is_empty() {
            bail!("a decision needs a question");
        }
        let mut options: Vec<String> = decision
            .options
            .iter()
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
            .collect();
        for evidence in &decision.evidence {
            if !EVIDENCE_KINDS.contains(&evidence.kind.as_str()) {
                bail!(
                    "unknown evidence kind `{}` (expected {})",
                    evidence.kind,
                    EVIDENCE_KINDS.join("|")
                );
            }
        }
        let reason = decision.reason.trim();
        let reason = if reason.is_empty() { REASON_OTHER } else { reason };
        if !DECISION_REASONS.contains(&reason) {
            bail!(
                "unknown reason `{}` (expected {})",
                reason,
                DECISION_REASONS.join("|")
            );
        }
        let brief = decision.brief.trimmed();
        if !brief.is_empty() {
            if !BRIEF_REASONS.contains(&reason) {
                bail!(
                    "--need, --command, --error, --tried and --after belong to a request with --reason {} (this one is `{reason}`)",
                    BRIEF_REASONS.join("|")
                );
            }
            if options.is_empty() {
                options = DEFAULT_BRIEF_OPTIONS.iter().map(|o| o.to_string()).collect();
            }
        }
        let error_signature = crate::failure_signature::of_text(&brief.error);
        let mut seen = Vec::new();
        let mut moves = Vec::new();
        for m in &decision.moves {
            if m.option < 1 || m.option > options.len() as i64 {
                bail!(
                    "--moves-to names option {}, but the decision has {} option(s)",
                    m.option,
                    options.len()
                );
            }
            if seen.contains(&m.option) {
                bail!("option {} is given two --moves-to states", m.option);
            }
            seen.push(m.option);
            check_move_target(&m.to)?;
            // Only a move back is a send-back: the node must be past it now.
            let current: Option<String> = self
                .conn
                .query_row(
                    "SELECT state FROM node_lifecycle WHERE node_id = ?1",
                    params![uuid_to_blob(node_id)],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(current) = current else {
                bail!("--moves-to: the node has no lifecycle, so there is no earlier state to go back to");
            };
            if state_rank(&m.to) >= state_rank(&current) {
                bail!("--moves-to {}: the node is in `{current}`, which is not later than `{}`", m.to, m.to);
            }
            moves.push(OptionMove { option: m.option, to: m.to.clone(), asked_in: Some(current) });
        }
        let mut edits: Vec<OptionEdit> = Vec::new();
        for e in &decision.edits {
            let flag = if e.affirm { "--affirm-obligation" } else { "--edit-obligation" };
            if e.option < 1 || e.option > options.len() as i64 {
                bail!(
                    "{flag} names option {}, but the decision has {} option(s)",
                    e.option,
                    options.len()
                );
            }
            if e.affirm {
                let short = crate::interview::short_id(e.obligation_id);
                let Some(current) = crate::outline::repos::ObligationRepo::new(self.conn).get(e.obligation_id)? else {
                    bail!("--affirm-obligation: obligation {short} does not exist");
                };
                if current.node_id != node_id {
                    bail!("--affirm-obligation: obligation {short} belongs to another node");
                }
                if current.user_owned() {
                    bail!(
                        "--affirm-obligation: obligation {short} is already the user's ({}); there is nothing to approve",
                        current.authority
                    );
                }
                if edits.iter().any(|x| x.option == e.option && x.obligation_id == e.obligation_id) {
                    bail!("option {} edits or approves obligation {short} twice", e.option);
                }
                edits.push(OptionEdit {
                    option: e.option,
                    obligation_id: e.obligation_id,
                    body: String::new(),
                    was: None,
                    affirm: true,
                });
                continue;
            }
            let body = e.body.trim();
            if body.is_empty() {
                bail!("--edit-obligation: the new text of obligation {} is empty", crate::interview::short_id(e.obligation_id));
            }
            if edits.iter().any(|x| x.option == e.option && x.obligation_id == e.obligation_id) {
                bail!(
                    "option {} edits obligation {} twice",
                    e.option,
                    crate::interview::short_id(e.obligation_id)
                );
            }
            let Some(current) = crate::outline::repos::ObligationRepo::new(self.conn).get(e.obligation_id)? else {
                bail!("--edit-obligation: obligation {} does not exist", crate::interview::short_id(e.obligation_id));
            };
            if current.node_id != node_id {
                bail!(
                    "--edit-obligation: obligation {} belongs to another node",
                    crate::interview::short_id(e.obligation_id)
                );
            }
            if current.body.trim() == body {
                bail!(
                    "--edit-obligation: obligation {} already says that; an option that changes nothing needs no edit",
                    crate::interview::short_id(e.obligation_id)
                );
            }
            edits.push(OptionEdit {
                option: e.option,
                obligation_id: e.obligation_id,
                body: body.to_string(),
                was: Some(current.body),
                affirm: false,
            });
        }
        let mut blocks: Vec<Uuid> = Vec::new();
        for step in &decision.blocks {
            if blocks.contains(step) {
                continue;
            }
            self.check_blockable(node_id, *step)?;
            blocks.push(*step);
        }
        if decision.guard_reask {
            let cause = Cause { question, blocks: &blocks, signature: &error_signature };
            self.check_not_pending(node_id, &cause)?;
            if decision.new_evidence.trim().is_empty() {
                self.check_not_reasked(node_id, &cause)?;
            }
        }
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO decisions
             (id, node_id, conversation_id, protocol, question, options, evidence, status, created_at, reason, option_moves, blocks, new_evidence, option_edits,
              brief_need, brief_command, brief_error, brief_tried, brief_after, error_signature, cause)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?20, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?21)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(node_id),
                conversation_id.map(uuid_to_blob),
                protocol,
                question,
                serde_json::to_string(&options)?,
                serde_json::to_string(&decision.evidence)?,
                now,
                reason,
                serde_json::to_string(&moves)?,
                serde_json::to_string(&blocks)?,
                decision.new_evidence.trim(),
                serde_json::to_string(&edits)?,
                brief.need,
                brief.command,
                brief.error,
                serde_json::to_string(&brief.tried)?,
                brief.after,
                error_signature,
                if decision.triage { DECISION_TRIAGE } else { DECISION_PENDING },
                normal_text(&decision.cause),
            ],
        )?;
        self.get(id)?.context("decision vanished after insert")
    }

    /// `step` must be a plan step of `node_id`.
    fn check_blockable(&self, node_id: Uuid, step: Uuid) -> Result<()> {
        let owner: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT node_id FROM node_plan_steps WHERE id = ?1",
                params![uuid_to_blob(step)],
                |row| row.get(0),
            )
            .optional()?;
        match owner {
            None => bail!("--blocks: plan step {} does not exist", crate::interview::short_id(step)),
            Some(owner) if owner != uuid_to_blob(node_id) => {
                bail!("--blocks: plan step {} belongs to another node", crate::interview::short_id(step))
            }
            Some(_) => Ok(()),
        }
    }

    /// Refuse a request whose cause is already waiting on this node, in any
    /// of its sessions: the same question (case and spacing aside), the same
    /// plan steps held up, or the same failure ([`Cause`]). The message names
    /// the existing request. A question withdrawn because its step was
    /// retried may be asked again for a new cause, but never twice at once.
    fn check_not_pending(&self, node_id: Uuid, cause: &Cause<'_>) -> Result<()> {
        if let Some(d) = self.list_open_for_node(node_id)?.into_iter().find(|d| cause.matches(d)) {
            bail!(
                "this is already waiting for the user's answer (request {}, {}): do not ask it twice. \
                 Carry on with other work until it is answered.",
                crate::interview::short_id(d.id),
                cause.why(&d)
            );
        }
        Ok(())
    }

    /// Refuse a request the user already answered on this node, however long
    /// ago (the same question, the same plan steps, or the same failure:
    /// [`Cause`]): asking again makes them answer twice for one cause. Answers
    /// by `tod` (not the user's) never count. The message names the answer, and
    /// the way through when something really is new.
    fn check_not_reasked(&self, node_id: Uuid, cause: &Cause<'_>) -> Result<()> {
        for decision in self.list_for_node(node_id)? {
            if decision.status != DECISION_ANSWERED || !cause.matches(&decision) {
                continue;
            }
            let Some(found) = self.get_with_answers(decision.id)? else { continue };
            let Some(answer) = found.answers.iter().rev().find(|a| a.actor != SETTLED_BY) else {
                continue;
            };
            let given = match (&answer.option, &answer.text) {
                (Some(o), Some(t)) => format!("option {o} ({t})"),
                (Some(o), None) => decision
                    .options
                    .get((*o - 1) as usize)
                    .map(|label| format!("\"{label}\""))
                    .unwrap_or_else(|| format!("option {o}")),
                (None, Some(t)) => format!("\"{t}\""),
                (None, None) => "(no text)".to_string(),
            };
            bail!(
                "this was already answered {} ago (request {}, {}; {} by {}): {given}. Carry on with that \
                 answer; the steps it held up are open again. Ask again only for something new, by passing \
                 --new-evidence <TEXT> saying what changed since (what you tried with the answer, and the \
                 error it gave).",
                age_label((now_ms() - answer.answered_at).max(0)),
                crate::interview::short_id(decision.id),
                cause.why(&decision),
                decision.question.lines().next().unwrap_or(""),
                answer.actor
            );
        }
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Decision>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DECISION_COLUMNS} FROM decisions WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_decision,
            )
            .optional()?)
    }

    /// A decision with its full answer log, oldest answer first.
    pub fn get_with_answers(&self, id: Uuid) -> Result<Option<DecisionWithAnswers>> {
        let Some(decision) = self.get(id)? else {
            return Ok(None);
        };
        let answers = self.list_answers(id)?;
        Ok(Some(DecisionWithAnswers { decision, answers }))
    }

    fn list_answers(&self, decision_id: Uuid) -> Result<Vec<DecisionAnswer>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ANSWER_COLUMNS} FROM decision_answers WHERE decision_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(decision_id)], map_answer)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pending decisions of one `protocol` on any node, oldest first.
    pub fn list_pending_by_protocol(&self, protocol: &str) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE status = 'pending' AND protocol = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(params![protocol], map_decision)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Decisions of one `protocol` on any node, in any status, oldest first.
    pub fn list_by_protocol(&self, protocol: &str) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE protocol = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(params![protocol], map_decision)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every pending decision of every node, oldest first.
    pub fn list_pending(&self) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE status = 'pending' ORDER BY created_at"
        ))?;
        let rows = stmt.query_map([], map_decision)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pending decisions on one node, oldest first.
    pub fn list_pending_for_node(&self, node_id: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE node_id = ?1 AND status = 'pending' ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Decisions on one node still waiting on an answer from the user or from
    /// triage ([`is_open`]), oldest first: what the asking agent holds its
    /// turn for.
    pub fn list_open_for_node(&self, node_id: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE node_id = ?1 AND status IN ('pending','triage') ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Decisions on one node waiting on triage, oldest first.
    pub fn list_triage_for_node(&self, node_id: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE node_id = ?1 AND status = 'triage' ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every decision of every node waiting on triage, oldest first.
    pub fn list_triage(&self) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE status = 'triage' ORDER BY created_at"
        ))?;
        let rows = stmt.query_map([], map_decision)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The decision a triage conversation reads or read, if any: with a group
    /// of requests sharing a cause ([`Self::triage_group`]), the **lead**, the
    /// oldest one still open (else the oldest of them all).
    pub fn for_triage_conversation(&self, conversation: Uuid) -> Result<Option<Decision>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {DECISION_COLUMNS} FROM decisions WHERE triage_conversation_id = ?1
                     ORDER BY status IN ('pending','triage') DESC, created_at, rowid LIMIT 1"
                ),
                params![uuid_to_blob(conversation)],
                map_decision,
            )
            .optional()?)
    }

    /// Every decision a triage conversation reads or read, oldest first: the
    /// lead and the requests that share its cause.
    pub fn all_for_triage_conversation(&self, conversation: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE triage_conversation_id = ?1 ORDER BY created_at, rowid"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(conversation)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The requests on `lead`'s node that share its cause and wait on triage
    /// with no session yet: one session reads them together and its answer
    /// settles every one of them (`doc/lifecycle/triage.md`, "Requests that
    /// share a cause"). They share a cause when they name the same `--cause`
    /// key (`Decision::shares_cause_with`).
    /// A request whose options move the task or edit obligations is never
    /// grouped (only the user decides those, one by one), and neither is one
    /// that is not in triage.
    pub fn triage_group(&self, lead: &Decision) -> Result<Vec<Decision>> {
        if !lead.in_triage() || !lead.groupable() {
            return Ok(Vec::new());
        }
        Ok(self
            .list_triage_for_node(lead.node_id)?
            .into_iter()
            .filter(|d| {
                d.id != lead.id && d.triage_conversation_id.is_none() && d.groupable() && lead.shares_cause_with(d)
            })
            .collect())
    }

    /// Whether `decision` is read by a triage session through another request
    /// that leads it: it waits in triage while the lead is open (escalated and
    /// pending, or being read), and is answered when the lead is. It is never
    /// a card of its own for the user, and no session is started for it.
    pub fn is_triage_follower(&self, decision: &Decision) -> Result<bool> {
        let Some(conversation) = decision.triage_conversation_id else { return Ok(false) };
        if !decision.in_triage() {
            return Ok(false);
        }
        Ok(self.for_triage_conversation(conversation)?.is_some_and(|lead| lead.id != decision.id))
    }

    /// Hand a request that waits on triage to the user as it was asked
    /// (`pending`): what happens to the requests in triage when the setting
    /// that turns triage off is off. Its followers (a group's other requests)
    /// are released too.
    pub fn release_to_user(&self, id: Uuid) -> Result<()> {
        let decision = self.get(id)?.with_context(|| format!("decision {id} not found"))?;
        if !decision.in_triage() {
            bail!("decision {id} is not waiting on triage");
        }
        self.conn.execute(
            "UPDATE decisions SET status = 'pending' WHERE id = ?1 AND status = 'triage'",
            params![uuid_to_blob(id)],
        )?;
        Ok(())
    }

    /// Pending decisions across every node in `node_ids`, oldest first, in
    /// one query — for a list view over many nodes at once.
    pub fn list_pending_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<Decision>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE status = 'pending' AND node_id IN ({placeholders})
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every decision on a node, newest first — for `tod-cli decisions list --all`.
    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE node_id = ?1 ORDER BY created_at DESC"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record an answer: an option pick, free text, or both. The decision
    /// becomes `answered` on its first answer; later answers append without
    /// touching earlier ones, per the append-only answer log.
    pub fn answer(
        &self,
        decision_id: Uuid,
        option: Option<i64>,
        text: Option<&str>,
        actor: &str,
    ) -> Result<DecisionAnswer> {
        self.answer_by(decision_id, option, text, actor, DECIDED_BY_USER, None, None)
    }

    /// Record tod's own answer to a request of one of the
    /// [`SETTLEABLE_PROTOCOLS`]: `decided_by = tod`, with the `rationale` saying
    /// what was tried. Anything else is refused: tod never answers what is
    /// the user's to decide.
    pub fn answer_settled(
        &self,
        decision_id: Uuid,
        option: i64,
        text: Option<&str>,
        rationale: &str,
    ) -> Result<DecisionAnswer> {
        let decision = self
            .get(decision_id)?
            .with_context(|| format!("decision {decision_id} not found"))?;
        if !decision.protocol.as_deref().is_some_and(|p| SETTLEABLE_PROTOCOLS.contains(&p)) {
            bail!("tod does not answer a {} request itself: it is the user's to decide", decision.protocol.as_deref().unwrap_or("this"));
        }
        if rationale.trim().is_empty() {
            bail!("an answer by tod needs a rationale saying what was tried");
        }
        self.answer_by(decision_id, Some(option), text, SETTLED_BY, DECIDED_BY_TOD, None, Some(rationale.trim()))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn answer_by(
        &self,
        decision_id: Uuid,
        option: Option<i64>,
        text: Option<&str>,
        actor: &str,
        decided_by: &str,
        conversation_id: Option<Uuid>,
        rationale: Option<&str>,
    ) -> Result<DecisionAnswer> {
        self.answer_inner(decision_id, option, text, actor, decided_by, conversation_id, rationale, true)
    }

    /// The followers of `lead` (the other requests its triage session reads,
    /// still waiting in triage) are answered with what the lead's answer says,
    /// as the same decision by the same hand: one cause, one answer.
    #[allow(clippy::too_many_arguments)]
    fn settle_followers(
        &self,
        lead: &Decision,
        followers: &[Decision],
        option: Option<i64>,
        text: Option<&str>,
        actor: &str,
        decided_by: &str,
        conversation_id: Option<Uuid>,
        rationale: Option<&str>,
    ) -> Result<()> {
        let label = option.and_then(|o| lead.options.get(usize::try_from(o).ok()?.checked_sub(1)?)).cloned();
        let given = match (label, text.map(str::trim).filter(|t| !t.is_empty())) {
            (Some(label), Some(text)) => format!("{label} ({text})"),
            (Some(label), None) => label,
            (None, Some(text)) => text.to_string(),
            (None, None) => bail!("an answer needs an option or text"),
        };
        let why = rationale.map(str::trim).filter(|r| !r.is_empty()).unwrap_or("the lead request was answered");
        let why = format!("Same cause as request {}: {why}", crate::interview::short_id(lead.id));
        for follower in followers {
            self.answer_inner(follower.id, None, Some(&given), actor, decided_by, conversation_id, Some(&why), false)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn answer_inner(
        &self,
        decision_id: Uuid,
        option: Option<i64>,
        text: Option<&str>,
        actor: &str,
        decided_by: &str,
        conversation_id: Option<Uuid>,
        rationale: Option<&str>,
        settle_group: bool,
    ) -> Result<DecisionAnswer> {
        let decision = self
            .get(decision_id)?
            .with_context(|| format!("decision {decision_id} not found"))?;
        if decision.status == DECISION_WITHDRAWN {
            bail!("decision {decision_id} was withdrawn");
        }
        let text = text.map(str::trim).filter(|t| !t.is_empty());
        if let Some(option) = option {
            if option < 1 || option > decision.options.len() as i64 {
                bail!("decision {decision_id} has no option {option}");
            }
        }
        if option.is_none() && text.is_none() {
            bail!("an answer needs an option or text");
        }
        // Answering the lead of a group answers every request that waits behind
        // it (computed before this one is closed, which changes who leads).
        let followers = match decision.triage_conversation_id {
            Some(conversation) if settle_group && is_open(&decision.status) => {
                if self.for_triage_conversation(conversation)?.is_some_and(|lead| lead.id == decision_id) {
                    self.all_for_triage_conversation(conversation)?
                        .into_iter()
                        .filter(|d| d.id != decision_id && d.in_triage() && d.groupable())
                        .collect()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO decision_answers
                 (decision_id, option_index, text, actor, answered_at, decided_by, conversation_id, rationale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid_to_blob(decision_id),
                option,
                text,
                actor,
                now,
                decided_by,
                conversation_id.map(uuid_to_blob),
                rationale
            ],
        )?;
        if is_open(&decision.status) {
            self.conn.execute(
                "UPDATE decisions SET status = 'answered' WHERE id = ?1",
                params![uuid_to_blob(decision_id)],
            )?;
            // One cause, one answer: the steps this decision held up are
            // open again, so nobody also has to retry each of them. The
            // decision is already answered, so reopening a step does not
            // resolve it again.
            self.reopen_blocked_steps(&decision)?;
        }
        let id = self.conn.last_insert_rowid();
        if !followers.is_empty() {
            self.settle_followers(&decision, &followers, option, text, actor, decided_by, conversation_id, rationale)?;
        }
        self.conn
            .query_row(
                &format!("SELECT {ANSWER_COLUMNS} FROM decision_answers WHERE id = ?1"),
                params![id],
                map_answer,
            )
            .context("answer vanished after insert")
    }

    /// The obligation edits picking `option` makes, checked against the
    /// obligations as they are now: each must still exist on the node and
    /// still read as it did when the question was asked (or already read as
    /// the new text). Returns the edits still to make, as mutations to run as
    /// the user: `(obligation, Some(new text))`, or `(obligation, None)` to
    /// approve it (`--affirm-obligation`). Fails, naming the obligation, when
    /// one no longer fits.
    pub fn pending_edits(&self, decision: &Decision, option: Option<i64>) -> Result<Vec<(Uuid, Option<String>)>> {
        let Some(option) = option else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for edit in decision.edits_for(option) {
            let short = crate::interview::short_id(edit.obligation_id);
            let Some(current) = crate::outline::repos::ObligationRepo::new(self.conn).get(edit.obligation_id)? else {
                bail!("obligation {short} no longer exists, so this option's edit to it cannot be made");
            };
            if current.node_id != decision.node_id {
                bail!("obligation {short} is no longer on this node, so this option's edit to it cannot be made");
            }
            if edit.affirm {
                if !current.user_owned() {
                    out.push((edit.obligation_id, None));
                }
                continue;
            }
            if current.body.trim() == edit.body.trim() {
                continue;
            }
            if let Some(was) = &edit.was {
                if current.body.trim() != was.trim() {
                    bail!(
                        "obligation {short} changed since this was asked, so the edit shown with the option no \
                         longer applies: it now reads \"{}\"",
                        current.body
                    );
                }
            }
            out.push((edit.obligation_id, Some(edit.body.clone())));
        }
        Ok(out)
    }

    /// Set the still-handed-back steps `decision` holds up to `ready`.
    fn reopen_blocked_steps(&self, decision: &Decision) -> Result<()> {
        let steps = crate::outline::repos::PlanStepRepo::new(self.conn);
        for id in &decision.blocks {
            let Some(step) = steps.get(*id)? else { continue };
            if !crate::outline::repos::plan_steps::needs_user(&step.status) {
                continue;
            }
            // Another open decision still holds this step up: its cause
            // stands, so the step stays handed back until the last holder is
            // settled. (`update_status` would otherwise withdraw that holder.)
            if self.pending_holder_of(*id, step.node_id, Some(decision.id))?.is_some() {
                continue;
            }
            steps.update_status(*id, crate::outline::repos::plan_steps::STATUS_READY, None, None)?;
        }
        Ok(())
    }

    /// A pending decision on `node_id` that holds `step` up, other than
    /// `except`.
    pub fn pending_holder_of(&self, step: Uuid, node_id: Uuid, except: Option<Uuid>) -> Result<Option<Uuid>> {
        Ok(self
            .list_open_for_node(node_id)?
            .into_iter()
            .find(|d| Some(d.id) != except && d.blocks.contains(&step))
            .map(|d| d.id))
    }

    /// Hold `steps` up with the pending `decision_id` (`plan update
    /// --decision`): same node, and the decision must still be waiting.
    pub fn link_steps(&self, decision_id: Uuid, steps: &[Uuid]) -> Result<()> {
        self.check_linkable(decision_id, steps)?;
        let decision = self
            .get(decision_id)?
            .with_context(|| format!("decision {decision_id} not found"))?;
        let mut blocks = decision.blocks.clone();
        for step in steps {
            if !blocks.contains(step) {
                blocks.push(*step);
            }
        }
        self.conn.execute(
            "UPDATE decisions SET blocks = ?1 WHERE id = ?2",
            params![serde_json::to_string(&blocks)?, uuid_to_blob(decision_id)],
        )?;
        Ok(())
    }

    /// A step the decision holds up left `partial`/`blocked` by some route
    /// other than this decision's own answer (the user's Retry, a rework):
    /// the cause the decision asked about is gone, so every pending decision
    /// holding that step is settled, and its other steps open again. They are
    /// *withdrawn*, never answered: nobody answered them, so no agent is told
    /// the user did and the re-ask guard does not count them.
    pub fn release_for_step(&self, step: Uuid, node_id: Uuid) -> Result<()> {
        let ids: Vec<Uuid> = self
            .list_open_for_node(node_id)?
            .into_iter()
            .filter(|d| d.blocks.contains(&step))
            .map(|d| d.id)
            .collect();
        self.withdraw_ids(&ids, true)?;
        Ok(())
    }

    /// Withdraw the pending `ids`. With `reopen`, the steps each held up that
    /// are still handed back open again (`ready`): the question that held them
    /// is gone, so they must not come back as separate requests. A decision
    /// is withdrawn before its steps are touched, so reopening them does not
    /// settle it again.
    fn withdraw_ids(&self, ids: &[Uuid], reopen: bool) -> Result<usize> {
        let mut changed = 0;
        for id in ids {
            let Some(decision) = self.get(*id)? else { continue };
            if !is_open(&decision.status) {
                continue;
            }
            self.conn.execute(
                "UPDATE decisions SET status = 'withdrawn' WHERE id = ?1 AND status IN ('pending','triage')",
                params![uuid_to_blob(*id)],
            )?;
            changed += 1;
            if reopen {
                self.reopen_blocked_steps(&decision)?;
            }
        }
        Ok(changed)
    }

    /// Whether a decision that is no longer pending (answered or withdrawn)
    /// held `step` up: a step still handed back after that is stale.
    pub fn settled_hold(&self, step: Uuid, node_id: Uuid) -> Result<bool> {
        Ok(self
            .list_for_node(node_id)?
            .iter()
            .any(|d| !is_open(&d.status) && d.blocks.contains(&step)))
    }

    /// Whether `steps` may be held up by `decision_id` (pending, same node);
    /// what [`Self::link_steps`] checks, so a caller can validate before
    /// writing anything else.
    pub fn check_linkable(&self, decision_id: Uuid, steps: &[Uuid]) -> Result<()> {
        let decision = self
            .get(decision_id)?
            .with_context(|| format!("decision {decision_id} not found"))?;
        if !is_open(&decision.status) {
            bail!(
                "decision {} is already {}: a step it no longer holds up cannot be linked to it",
                crate::interview::short_id(decision_id),
                decision.status
            );
        }
        for step in steps {
            self.check_blockable(decision.node_id, *step)?;
        }
        Ok(())
    }

    /// Withdraw the node's pending decisions that have an option moving the
    /// node, except `keep`: the node was moved by hand, so "send it back to
    /// X" no longer describes it, and answering it later would redo a move
    /// the person has already made their own way. The steps those decisions
    /// held up open again: the stay they were handed back in is over.
    /// Returns how many.
    pub fn withdraw_moving_for_node(&self, node_id: Uuid, keep: Option<Uuid>) -> Result<usize> {
        let ids: Vec<Uuid> = self
            .list_open_for_node(node_id)?
            .into_iter()
            .filter(|d| !d.moves.is_empty() && Some(d.id) != keep)
            .map(|d| d.id)
            .collect();
        self.withdraw_ids(&ids, true)
    }

    /// Withdraw every pending decision of the node: it is finished, so
    /// nothing is waiting on an answer (`crate::done_close`). The steps they
    /// held up are left as they are: a finished node's history is not
    /// rewritten, and nothing runs them again. Returns how many.
    pub fn withdraw_pending_for_node(&self, node_id: Uuid) -> Result<usize> {
        let ids: Vec<Uuid> = self.list_open_for_node(node_id)?.into_iter().map(|d| d.id).collect();
        self.withdraw_ids(&ids, false)
    }

    /// Withdraw the node's pending decisions asked by an agent of one of
    /// `protocols`, except `keep`: the stay they were asked in is over (the
    /// node was moved back past it). The steps they held up open again, for
    /// the same reason. Returns how many.
    pub fn withdraw_for_protocols(&self, node_id: Uuid, protocols: &[&str], keep: Option<Uuid>) -> Result<usize> {
        let ids: Vec<Uuid> = self
            .list_open_for_node(node_id)?
            .into_iter()
            .filter(|d| d.protocol.as_deref().is_some_and(|p| protocols.contains(&p)) && Some(d.id) != keep)
            .map(|d| d.id)
            .collect();
        self.withdraw_ids(&ids, true)
    }

    /// Link the triage session that reads a request in triage; once.
    pub fn link_triage(&self, id: Uuid, conversation: Uuid) -> Result<()> {
        let decision = self.get(id)?.with_context(|| format!("decision {id} not found"))?;
        if !decision.in_triage() {
            bail!("decision {id} is not waiting on triage");
        }
        if decision.triage_conversation_id.is_some_and(|c| c != conversation) {
            bail!("decision {id} already has a triage session");
        }
        self.conn.execute(
            "UPDATE decisions SET triage_conversation_id = ?2 WHERE id = ?1",
            params![uuid_to_blob(id), uuid_to_blob(conversation)],
        )?;
        Ok(())
    }

    /// The request as `conversation`, its triage session, may act on it: in
    /// triage (`need_triage`) or escalated and pending, linked to that session.
    fn triage_target(&self, id: Uuid, conversation: Uuid, need_triage: bool) -> Result<Decision> {
        let decision = self.get(id)?.with_context(|| format!("decision {id} not found"))?;
        if decision.triage_conversation_id != Some(conversation) {
            bail!("decision {id} is not the request this triage session reads");
        }
        let ok = if need_triage { decision.in_triage() } else { decision.status == DECISION_PENDING };
        if !ok {
            bail!(
                "decision {id} is {}: {}",
                decision.status,
                if need_triage { "triage acts only while a request waits on it" } else { "this applies to an escalated request the user is answering" }
            );
        }
        Ok(decision)
    }

    /// Triage's own answer (`decided_by = triage`): an option or free text,
    /// with the rationale. Refused, telling triage to escalate, for a request
    /// whose options edit obligations, and for a chosen option that moves the
    /// node: those are the user's to decide.
    pub fn answer_triaged(
        &self,
        id: Uuid,
        conversation: Uuid,
        option: Option<i64>,
        text: Option<&str>,
        rationale: &str,
    ) -> Result<DecisionAnswer> {
        let decision = self.triage_target(id, conversation, true)?;
        if rationale.trim().is_empty() {
            bail!("a triage answer needs a rationale: why this is the answer, in the user's terms");
        }
        if !decision.edits.is_empty() {
            bail!("refused: this request carries obligation edits, which only the user may make; escalate it (`triage escalate`)");
        }
        if let Some(option) = option {
            if let Some(to) = decision.move_for(option) {
                bail!("refused: option {option} moves the task back to `{to}`, which only the user decides; escalate it (`triage escalate`), or convert it to a dispute (`triage convert-dispute`) when it disputes an obligation");
            }
        }
        self.answer_by(id, option, text, TRIAGE_ACTOR, DECIDED_BY_TRIAGE, Some(conversation), Some(rationale.trim()))
    }

    /// Triage closes a request it converted to something else (a suggestion,
    /// a dispute): answered in free text saying what it became.
    pub fn close_converted(&self, id: Uuid, conversation: Uuid, what: &str, rationale: &str) -> Result<DecisionAnswer> {
        let decision = self.get(id)?.with_context(|| format!("decision {id} not found"))?;
        if decision.triage_conversation_id != Some(conversation) {
            bail!("decision {id} is not the request this triage session reads");
        }
        if !is_open(&decision.status) {
            bail!("decision {id} is {}", decision.status);
        }
        self.answer_by(id, None, Some(what), TRIAGE_ACTOR, DECIDED_BY_TRIAGE, Some(conversation), Some(rationale.trim()))
    }

    /// Triage hands a request to the user with a brief: the brief replaces the
    /// question (and `options`, when given and when no option carries a move
    /// or an edit the new list would misalign), the asking agent's own words
    /// are kept in `asked`, and the request is `pending` for the user, its
    /// triage session still linked as the person's chat partner for it.
    pub fn escalate(&self, id: Uuid, conversation: Uuid, brief: &str, options: Option<&[String]>) -> Result<()> {
        let decision = self.triage_target(id, conversation, true)?;
        let brief = brief.trim();
        if brief.is_empty() {
            bail!("an escalation needs a brief: what the user must decide, with the facts they need");
        }
        let options: Vec<String> = match options {
            Some(options) => {
                let options: Vec<String> = options.iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect();
                if (!decision.moves.is_empty() || !decision.edits.is_empty()) && options.len() != decision.options.len() {
                    bail!(
                        "this request's options move the task or edit obligations, so the options cannot be \
                         replaced by a different number: keep the same {} in the same order, or give none",
                        decision.options.len()
                    );
                }
                options
            }
            None => decision.options.clone(),
        };
        let asked = decision
            .asked
            .clone()
            .unwrap_or(Asked { question: decision.question.clone(), options: decision.options.clone() });
        self.conn.execute(
            "UPDATE decisions SET status = 'pending', question = ?2, options = ?3, asked = ?4 WHERE id = ?1",
            params![
                uuid_to_blob(id),
                brief,
                serde_json::to_string(&options)?,
                serde_json::to_string(&asked)?,
            ],
        )?;
        Ok(())
    }

    /// The user settles an escalated request in the triage chat
    /// (`decided_by = user`): the session is the linked one, the request is
    /// pending, and the user has spoken in it.
    pub fn answer_in_chat(
        &self,
        id: Uuid,
        conversation: Uuid,
        option: Option<i64>,
        text: Option<&str>,
        rationale: &str,
    ) -> Result<DecisionAnswer> {
        self.triage_target(id, conversation, false)?;
        if crate::conversation::ConversationRepo::new(self.conn).max_user_seq(conversation)? == 0 {
            bail!("the user has not said anything in this conversation: do not settle the request for them");
        }
        let rationale = rationale.trim();
        self.answer_by(
            id,
            option,
            text,
            DECIDED_BY_USER,
            DECIDED_BY_USER,
            Some(conversation),
            if rationale.is_empty() { None } else { Some(rationale) },
        )
    }

    /// Withdraw a decision: it no longer needs an answer, because what it
    /// asked about was settled without the user (the lost-work and bot
    /// callers). The steps it held up open again: their cause is gone.
    pub fn withdraw(&self, id: Uuid) -> Result<()> {
        if self.withdraw_ids(&[id], true)? == 0 {
            bail!("decision {id} not found or not pending");
        }
        Ok(())
    }

    /// A decision from its full id or the 8-character prefix listings show.
    pub fn resolve(&self, raw: &str) -> Result<Uuid> {
        let raw = raw.trim();
        if let Ok(id) = Uuid::parse_str(raw) {
            return Ok(id);
        }
        let prefix = raw.to_ascii_lowercase();
        if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("`{raw}` is not a decision id");
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM decisions WHERE lower(hex(id)) LIKE ?1 || '%'")?;
        let ids = stmt
            .query_map(params![prefix], |row| {
                blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        match ids.as_slice() {
            [id] => Ok(*id),
            [] => bail!("no decision matches `{raw}`"),
            _ => bail!(
                "`{raw}` matches {} decisions; give more of the id",
                ids.len()
            ),
        }
    }
}

fn map_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    let conversation_id: Option<Vec<u8>> = row.get(2)?;
    let options: String = row.get(5)?;
    let evidence: String = row.get(6)?;
    Ok(Decision {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        conversation_id: conversation_id
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        protocol: row.get(3)?,
        question: row.get(4)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
        status: row.get(7)?,
        created_at: row.get(8)?,
        reason: row.get(9)?,
        moves: {
            // A corrupt value must not read as "no moves": the answer would
            // then move nothing, the failure this column exists to prevent.
            let raw: String = row.get(10)?;
            serde_json::from_str(&raw).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(err))
            })?
        },
        blocks: {
            let raw: String = row.get(11)?;
            serde_json::from_str(&raw).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(err))
            })?
        },
        new_evidence: row.get(12)?,
        edits: {
            let raw: String = row.get(13)?;
            serde_json::from_str(&raw).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(err))
            })?
        },
        brief: Brief {
            need: row.get(14)?,
            command: row.get(15)?,
            error: row.get(16)?,
            tried: {
                let raw: String = row.get(17)?;
                serde_json::from_str(&raw).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(17, rusqlite::types::Type::Text, Box::new(err))
                })?
            },
            after: row.get(18)?,
        },
        error_signature: row.get(19)?,
        triage_conversation_id: row
            .get::<_, Option<Vec<u8>>>(20)?
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        asked: match row.get::<_, Option<String>>(21)? {
            Some(raw) => Some(serde_json::from_str(&raw).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(21, rusqlite::types::Type::Text, Box::new(err))
            })?),
            None => None,
        },
        cause: row.get(22)?,
    })
}

fn map_answer(row: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionAnswer> {
    let decision_id: Vec<u8> = row.get(1)?;
    Ok(DecisionAnswer {
        id: row.get(0)?,
        decision_id: blob_to_uuid_sql(&decision_id)?,
        option: row.get(2)?,
        text: row.get(3)?,
        actor: row.get(4)?,
        answered_at: row.get(5)?,
        decided_by: row.get(6)?,
        conversation_id: row
            .get::<_, Option<Vec<u8>>>(7)?
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        rationale: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    struct Fx {
        dir: std::path::PathBuf,
        conn: Connection,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> Fx {
        let dir = tod_test_util::scratch_dir(format!("tod-decisions-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "decided", "Decided")
            .unwrap();
        Fx { dir, conn, node }
    }

    fn decision(question: &str, options: &[&str]) -> NewDecision {
        NewDecision {
            question: question.into(),
            options: options.iter().map(|s| s.to_string()).collect(),
            evidence: vec![EvidenceRef {
                kind: "obligation".into(),
                id: Uuid::new_v4(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn tod_settles_only_what_it_may_and_says_why() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let silent = repo
            .create(fx.node, None, Some("bot_silence"), &decision("Proceed without the bot?", &["Proceed", "Wait"]))
            .unwrap();
        assert!(repo.answer_settled(silent.id, 1, None, "  ").is_err(), "a rationale is required");
        let answer = repo.answer_settled(silent.id, 1, None, "asked 2 times, no review").unwrap();
        assert_eq!(answer.decided_by, DECIDED_BY_TOD);
        assert_eq!(answer.actor, "tod");
        assert_eq!(answer.rationale.as_deref(), Some("asked 2 times, no review"));
        let back = repo.get_with_answers(silent.id).unwrap().unwrap();
        assert_eq!(back.answers[0].decided_by, DECIDED_BY_TOD);

        let users = repo
            .create(fx.node, None, Some("implement"), &decision("Per line?", &["yes", "no"]))
            .unwrap();
        assert!(repo.answer_settled(users.id, 1, None, "because").is_err(), "a user's decision is never tod's");
        let by_user = repo.answer(users.id, Some(1), None, "user").unwrap();
        assert_eq!(by_user.decided_by, DECIDED_BY_USER);
        assert!(by_user.rationale.is_none());
    }

    #[test]
    fn create_defaults_to_pending_and_round_trips_options_and_evidence() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(
                fx.node,
                None,
                Some("implement"),
                &decision("Round per line or per invoice?", &["per line", "per invoice"]),
            )
            .unwrap();
        assert_eq!(d.status, DECISION_PENDING);
        assert_eq!(d.options, ["per line", "per invoice"]);
        assert_eq!(d.evidence.len(), 1);
        assert_eq!(d.evidence[0].kind, "obligation");
        assert_eq!(d.protocol.as_deref(), Some("implement"));
        assert_eq!(d.reason, REASON_OTHER);
        let got = repo.get(d.id).unwrap().unwrap();
        assert_eq!(got, d);
    }

    #[test]
    fn create_round_trips_an_explicit_reason_and_rejects_an_unknown_one() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let mut with_reason = decision("Missing a secret?", &["ok"]);
        with_reason.reason = REASON_ACCESS.to_string();
        let d = repo.create(fx.node, None, None, &with_reason).unwrap();
        assert_eq!(d.reason, REASON_ACCESS);

        let mut bad_reason = decision("x?", &["ok"]);
        bad_reason.reason = "spaceship".to_string();
        assert!(repo.create(fx.node, None, None, &bad_reason).is_err());
    }

    #[test]
    fn a_decision_needs_a_question_and_known_evidence_kinds() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        assert!(repo.create(fx.node, None, None, &decision(" ", &[])).is_err());
        let mut bad = decision("x?", &[]);
        bad.evidence = vec![EvidenceRef {
            kind: "spaceship".into(),
            id: Uuid::new_v4(),
        }];
        assert!(repo.create(fx.node, None, None, &bad).is_err());
    }

    #[test]
    fn pending_per_node_lists_only_pending_oldest_first() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let first = repo
            .create(fx.node, None, None, &decision("First?", &["a", "b"]))
            .unwrap();
        let second = repo
            .create(fx.node, None, None, &decision("Second?", &["a", "b"]))
            .unwrap();
        repo.answer(first.id, Some(1), None, "user").unwrap();
        let pending: Vec<_> = repo
            .list_pending_for_node(fx.node)
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(pending, [second.id]);
    }

    #[test]
    fn pending_for_many_nodes_is_one_query_across_all_of_them() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let other = Uuid::new_v4();
        NodeRepo::new(&fx.conn)
            .create_with_id(other, "other", "Other")
            .unwrap();
        let a = repo
            .create(fx.node, None, None, &decision("A?", &["x"]))
            .unwrap();
        let b = repo
            .create(other, None, None, &decision("B?", &["x"]))
            .unwrap();
        let mut ids: Vec<_> = repo
            .list_pending_for_nodes(&[fx.node, other])
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        ids.sort();
        let mut expected = [a.id, b.id];
        expected.sort();
        assert_eq!(ids, expected);
        assert!(repo.list_pending_for_nodes(&[]).unwrap().is_empty());
    }

    #[test]
    fn answering_sets_status_and_changing_an_answer_appends_never_updates() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        let first = repo.answer(d.id, Some(1), None, "user").unwrap();
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, DECISION_ANSWERED);
        let second = repo
            .answer(d.id, Some(2), Some("changed my mind"), "user")
            .unwrap();
        assert_ne!(first.id, second.id);
        let with_answers = repo.get_with_answers(d.id).unwrap().unwrap();
        assert_eq!(with_answers.answers.len(), 2);
        assert_eq!(with_answers.answers[0].option, Some(1));
        assert_eq!(with_answers.answers[1].option, Some(2));
        assert_eq!(with_answers.answers[1].text.as_deref(), Some("changed my mind"));
        // Still answered, not reset.
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, DECISION_ANSWERED);
    }

    #[test]
    fn an_option_move_targets_only_states_before_pr_review() {
        for ok in ["proposed", "design", "planning", "ready", "active", "verifying", "review", "pr", "pr_draft"] {
            check_move_target(ok).unwrap();
        }
        for bad in ["pr_review", "approved", "merged", "released", "learn", "done", "nope", ""] {
            assert!(check_move_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_move_needs_a_lifecycle_to_go_back_in() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let mut d = decision("Back?", &["Send it back"]);
        d.moves = vec![OptionMove { option: 1, to: "active".into(), asked_in: None }];
        // The node has no lifecycle state here.
        assert!(repo.create(fx.node, None, None, &d).is_err());
    }

    #[test]
    fn an_answer_needs_an_option_in_range_or_text() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        assert!(repo.answer(d.id, None, None, "user").is_err());
        assert!(repo.answer(d.id, Some(3), None, "user").is_err());
        assert!(repo.answer(d.id, Some(0), None, "user").is_err());
        repo.answer(d.id, None, Some("free text is fine too"), "user")
            .unwrap();
    }

    #[test]
    fn withdrawing_stops_it_being_pending_and_refuses_a_second_withdraw() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        repo.withdraw(d.id).unwrap();
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, DECISION_WITHDRAWN);
        assert!(repo.list_pending_for_node(fx.node).unwrap().is_empty());
        assert!(repo.withdraw(d.id).is_err());
    }

    #[test]
    fn resolve_finds_by_full_id_or_prefix() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        let prefix = &d.id.simple().to_string()[..8];
        assert_eq!(repo.resolve(prefix).unwrap(), d.id);
        assert_eq!(repo.resolve(&d.id.to_string()).unwrap(), d.id);
    }

    #[test]
    fn deleting_the_node_cascades_the_decision_and_its_answers() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        fx.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_blob(fx.node)])
            .unwrap();
        assert!(repo.get(d.id).unwrap().is_none());
        assert!(
            repo.list_answers(d.id).unwrap().is_empty(),
            "answers should cascade with the decision"
        );
    }

    #[test]
    fn migration_from_previous_version_adds_the_tables() {
        let dir = tod_test_util::scratch_dir(format!("tod-decisions-migrate-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("tod.db");
        {
            // A fresh store already at the current version; wind it back and
            // drop the tables this version adds, as if it predated them.
            let conn = schema::open_writer_connection(&db_path).unwrap();
            // v64 added them.
            conn.pragma_update(None, "user_version", 63).unwrap();
            conn.execute_batch(
                "DROP TABLE IF EXISTS decision_answers; DROP TABLE IF EXISTS decisions;",
            )
            .unwrap();
        }
        // Reopening runs the migrations again, including the one that
        // recreates decisions/decision_answers.
        let conn = schema::open_writer_connection(&db_path).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "post-migrate", "Post migrate")
            .unwrap();
        let repo = DecisionRepo::new(&conn);
        let d = repo
            .create(node, None, None, &decision("Works?", &["yes"]))
            .unwrap();
        assert_eq!(d.status, DECISION_PENDING);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Steps handed back for one cause.
    fn blocked_steps(fx: &Fx, n: usize) -> Vec<Uuid> {
        use crate::outline::repos::PlanStepRepo;
        use crate::outline::repos::plan_steps::{HandoffReason, STATUS_BLOCKED};
        let steps = PlanStepRepo::new(&fx.conn);
        (0..n)
            .map(|i| {
                let id = Uuid::new_v4();
                steps.insert_at(id, fx.node, i, &format!("Step {i}")).unwrap();
                steps
                    .update_status(
                        id,
                        STATUS_BLOCKED,
                        Some("no access"),
                        Some(&HandoffReason::Access { needs: "a key".into(), tried: "ran it".into() }),
                    )
                    .unwrap();
                id
            })
            .collect()
    }

    fn status_of(fx: &Fx, id: Uuid) -> String {
        crate::outline::repos::PlanStepRepo::new(&fx.conn).get(id).unwrap().unwrap().status
    }

    #[test]
    fn answering_a_decision_reopens_the_steps_it_blocks() {
        let fx = setup();
        let steps = blocked_steps(&fx, 3);
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(
                fx.node,
                None,
                None,
                &NewDecision { blocks: vec![steps[0], steps[1]], ..decision("Carry on?", &["yes", "no"]) },
            )
            .unwrap();
        assert_eq!(d.blocks, vec![steps[0], steps[1]]);
        repo.answer(d.id, Some(1), None, "user").unwrap();
        assert_eq!(status_of(&fx, steps[0]), "ready");
        assert_eq!(status_of(&fx, steps[1]), "ready");
        // The step the decision did not hold up stays handed back.
        assert_eq!(status_of(&fx, steps[2]), "blocked");
    }

    #[test]
    fn retrying_one_step_resolves_the_decision_and_releases_the_others() {
        let fx = setup();
        let steps = blocked_steps(&fx, 2);
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &NewDecision { blocks: steps.clone(), ..decision("Carry on?", &["yes"]) })
            .unwrap();
        crate::outline::repos::PlanStepRepo::new(&fx.conn)
            .update_status(steps[0], "in_progress", None, None)
            .unwrap();
        // Nobody answered it: it is withdrawn, with no answer on record to
        // show an agent or to count against asking again.
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, "withdrawn");
        assert_eq!(status_of(&fx, steps[1]), "ready");
        assert!(repo.list_pending_for_node(fx.node).unwrap().is_empty());
        assert!(repo.get_with_answers(d.id).unwrap().unwrap().answers.is_empty());
        // So the same question may be asked again for a new cause.
        repo.create(
            fx.node,
            None,
            None,
            &NewDecision { guard_reask: true, ..decision("Carry on?", &["yes"]) },
        )
        .unwrap();
    }

    #[test]
    fn withdrawing_a_decision_reopens_the_steps_it_held_up() {
        let fx = setup();
        let steps = blocked_steps(&fx, 2);
        let repo = DecisionRepo::new(&fx.conn);
        let held = |q: &str, s: &[Uuid]| {
            repo.create(fx.node, None, Some("implement"), &NewDecision { blocks: s.to_vec(), ..decision(q, &["a"]) })
                .unwrap()
        };
        let a = held("A?", &steps[..1]);
        repo.withdraw(a.id).unwrap();
        assert_eq!(status_of(&fx, steps[0]), "ready");
        assert_eq!(status_of(&fx, steps[1]), "blocked");
        // The protocol sweep does the same.
        held("B?", &steps[1..]);
        repo.withdraw_for_protocols(fx.node, &["implement"], None).unwrap();
        assert_eq!(status_of(&fx, steps[1]), "ready");
    }

    #[test]
    fn closing_a_done_node_withdraws_without_rewriting_its_steps() {
        let fx = setup();
        let steps = blocked_steps(&fx, 1);
        let repo = DecisionRepo::new(&fx.conn);
        repo.create(fx.node, None, None, &NewDecision { blocks: steps.clone(), ..decision("A?", &["a"]) }).unwrap();
        assert_eq!(repo.withdraw_pending_for_node(fx.node).unwrap(), 1);
        assert_eq!(status_of(&fx, steps[0]), "blocked");
    }

    #[test]
    fn restoring_a_handed_back_step_whose_question_was_answered_opens_it() {
        use crate::conversation::EntitySnapshot;
        let fx = setup();
        crate::outline::repos::NodeRepo::new(&fx.conn)
            .enable_capability(fx.node, crate::outline::Capability::Spec)
            .unwrap();
        let steps = blocked_steps(&fx, 1);
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &NewDecision { blocks: steps.clone(), ..decision("Q?", &["a"]) })
            .unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        // The step is deleted, then restored from a snapshot taken while it was blocked.
        fx.conn
            .execute(
                "DELETE FROM node_plan_steps WHERE id = ?1",
                params![uuid_to_blob(steps[0])],
            )
            .unwrap();
        let snapshot = EntitySnapshot::PlanStep {
            node_id: fx.node,
            ordinal: 0,
            body: "Step 0".into(),
            status: "blocked".into(),
            note: Some("no access".into()),
            reason: None,
            depends_on: vec![],
            satisfies: vec![],
            phase: "active".into(),
        };
        crate::outline::mutations::restore_plan_step(&fx.conn, steps[0], &snapshot).unwrap();
        assert_eq!(status_of(&fx, steps[0]), "ready");
    }

    #[test]
    fn new_evidence_is_kept_on_the_decision() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &NewDecision { new_evidence: " it failed with E1 ".into(), ..decision("Q?", &["a"]) })
            .unwrap();
        assert_eq!(d.new_evidence, "it failed with E1");
        assert_eq!(repo.get(d.id).unwrap().unwrap().new_evidence, "it failed with E1");
    }

    #[test]
    fn linking_is_validated_before_anything_is_written() {
        let fx = setup();
        let steps = blocked_steps(&fx, 1);
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo.create(fx.node, None, None, &decision("Q?", &["a"])).unwrap();
        repo.check_linkable(d.id, &steps).unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        assert!(repo.check_linkable(d.id, &steps).is_err());
    }

    #[test]
    fn a_step_can_be_linked_to_a_pending_decision_later() {
        let fx = setup();
        let steps = blocked_steps(&fx, 1);
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo.create(fx.node, None, None, &decision("Carry on?", &["yes"])).unwrap();
        repo.link_steps(d.id, &steps).unwrap();
        assert_eq!(repo.get(d.id).unwrap().unwrap().blocks, steps);
        repo.answer(d.id, Some(1), None, "user").unwrap();
        assert_eq!(status_of(&fx, steps[0]), "ready");
        // An answered decision holds nothing more up.
        assert!(repo.link_steps(d.id, &steps).is_err());
    }

    #[test]
    fn a_step_of_another_node_cannot_be_linked() {
        let fx = setup();
        let other = Uuid::new_v4();
        NodeRepo::new(&fx.conn).create_with_id(other, "other", "Other").unwrap();
        let step = Uuid::new_v4();
        crate::outline::repos::PlanStepRepo::new(&fx.conn).insert_at(step, other, 0, "Theirs").unwrap();
        let err = DecisionRepo::new(&fx.conn)
            .create(fx.node, None, None, &NewDecision { blocks: vec![step], ..decision("Q?", &["a"]) })
            .unwrap_err();
        assert!(err.to_string().contains("another node"), "{err}");
    }

    #[test]
    fn a_guarded_ask_for_an_answered_question_is_refused_unless_something_is_new() {
        let fx = setup();
        let steps = blocked_steps(&fx, 1);
        let repo = DecisionRepo::new(&fx.conn);
        let guarded = |question: &str, blocks: Vec<Uuid>, evidence: &str| NewDecision {
            blocks,
            guard_reask: true,
            new_evidence: evidence.into(),
            ..decision(question, &["Carry on without it", "Wait"])
        };
        let first = repo
            .create(fx.node, None, None, &guarded("Verify on PG 18?", vec![steps[0]], ""))
            .unwrap();
        repo.answer(first.id, Some(1), None, "user").unwrap();
        // The same words, even re-spaced and re-cased.
        let err = repo
            .create(fx.node, None, None, &guarded("verify on  PG 18?", vec![], ""))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Carry on without it") && err.contains("--new-evidence"), "{err}");
        // A different question holding up the same step is the same cause.
        let err = repo
            .create(fx.node, None, None, &guarded("Different words", vec![steps[0]], ""))
            .unwrap_err()
            .to_string();
        assert!(err.contains("same plan steps"), "{err}");
        // An unrelated question is fine.
        repo.create(fx.node, None, None, &guarded("Something else entirely?", vec![], "")).unwrap();
        // New evidence lets the repeat through.
        repo.create(fx.node, None, None, &guarded("Verify on PG 18?", vec![steps[0]], "it failed with E1")).unwrap();
    }

    #[test]
    fn the_apps_own_questions_are_not_guarded() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo.create(fx.node, None, None, &decision("Proceed without the bot?", &["yes"])).unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        repo.create(fx.node, None, None, &decision("Proceed without the bot?", &["yes"])).unwrap();
    }

    #[test]
    fn an_old_answer_still_blocks_the_question_but_new_evidence_lets_it_through() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo.create(fx.node, None, None, &decision("Old question?", &["yes"])).unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        fx.conn
            .execute("UPDATE decision_answers SET answered_at = answered_at - ?1", params![3 * 24 * 60 * 60 * 1000i64])
            .unwrap();
        let ask = |evidence: &str| {
            repo.create(
                fx.node,
                None,
                None,
                &NewDecision { guard_reask: true, new_evidence: evidence.into(), ..decision("Old question?", &["yes"]) },
            )
        };
        let err = ask("").unwrap_err().to_string();
        assert!(err.contains("3 days ago") && err.contains(&crate::interview::short_id(d.id)), "{err}");
        ask("it broke again").unwrap();
    }

    #[test]
    fn a_pending_request_is_found_across_sessions_by_text_steps_or_failure() {
        let fx = setup();
        let steps = blocked_steps(&fx, 2);
        let repo = DecisionRepo::new(&fx.conn);
        let broken = |question: &str, error: &str, blocks: Vec<Uuid>| NewDecision {
            reason: REASON_BROKEN.into(),
            guard_reask: true,
            blocks,
            brief: Brief { error: error.into(), ..Default::default() },
            ..decision(question, &[])
        };
        let first = repo
            .create(fx.node, None, None, &broken("Redis is down", "ECONNREFUSED 10.0.0.5:6379 at 2026-11-20T10:00:00Z", vec![steps[0]]))
            .unwrap();
        assert!(!first.error_signature.is_empty());
        let short = crate::interview::short_id(first.id);
        // Same failure, other words, other session, later time.
        let err = repo
            .create(fx.node, None, None, &broken("Cannot reach cache", "ECONNREFUSED 10.0.0.5:6379 at 2026-11-21T01:02:03Z", vec![]))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&short) && err.contains("same failure"), "{err}");
        // Same step.
        let err = repo.create(fx.node, None, None, &broken("Other words", "different error", vec![steps[0]])).unwrap_err().to_string();
        assert!(err.contains(&short) && err.contains("same plan steps"), "{err}");
        // Nothing in common.
        repo.create(fx.node, None, None, &broken("Another", "disk full", vec![steps[1]])).unwrap();
    }

    #[test]
    fn an_answered_failure_is_not_asked_again_without_new_evidence() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let ask = |evidence: &str| NewDecision {
            reason: REASON_BROKEN.into(),
            guard_reask: true,
            new_evidence: evidence.into(),
            brief: Brief { error: "permission denied for table orders".into(), ..Default::default() },
            ..decision("The orders table is unreadable", &[])
        };
        let d = repo.create(fx.node, None, None, &ask("")).unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        let again = NewDecision { question: "Worded another way".into(), ..ask("") };
        assert!(repo.create(fx.node, None, None, &again).unwrap_err().to_string().contains("--new-evidence"));
        repo.create(fx.node, None, None, &NewDecision { question: "Worded another way".into(), ..ask("granted, still denied") }).unwrap();
    }

    #[test]
    fn a_brief_is_stored_with_default_options_and_only_for_its_reasons() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let brief = Brief {
            need: " Read access to prod-kb ".into(),
            command: "aws sso login".into(),
            error: "AccessDenied".into(),
            tried: vec!["login".into(), " ".into(), "retry".into()],
            after: "I rerun the query".into(),
        };
        let d = repo
            .create(fx.node, None, None, &NewDecision { reason: REASON_ACCESS.into(), brief: brief.clone(), ..decision("Need access", &[]) })
            .unwrap();
        assert_eq!(d.options, DEFAULT_BRIEF_OPTIONS);
        let got = repo.get(d.id).unwrap().unwrap();
        assert_eq!(got.brief.need, "Read access to prod-kb");
        assert_eq!(got.brief.tried, vec!["login", "retry"]);
        assert_eq!(got.brief.after, "I rerun the query");
        assert_eq!(got.error_signature, crate::failure_signature::of_text("AccessDenied"));
        // Own options are kept.
        let own = repo
            .create(fx.node, None, None, &NewDecision { reason: REASON_ACCESS.into(), brief: brief.clone(), ..decision("Need access 2", &["Granted"]) })
            .unwrap();
        assert_eq!(own.options, vec!["Granted"]);
        // An older-style request has no brief.
        let plain = repo.create(fx.node, None, None, &decision("Plain?", &["a"])).unwrap();
        assert!(plain.brief.is_empty() && plain.error_signature.is_empty());
        // Other reasons refuse the fields.
        let err = repo
            .create(fx.node, None, None, &NewDecision { reason: REASON_RISK.into(), brief, ..decision("Risky", &["a"]) })
            .unwrap_err()
            .to_string();
        assert!(err.contains("--reason access|broken|capability"), "{err}");
    }

    /// Two requests hold the same step for different causes.
    fn two_holders(fx: &Fx) -> (Uuid, Decision, Decision) {
        let step = blocked_steps(fx, 1)[0];
        let repo = DecisionRepo::new(&fx.conn);
        let a = repo.create(fx.node, None, None, &NewDecision { blocks: vec![step], ..decision("A: the key?", &["ok"]) }).unwrap();
        let b = repo.create(fx.node, None, None, &NewDecision { blocks: vec![step], ..decision("B: the login?", &["ok"]) }).unwrap();
        (step, a, b)
    }

    #[test]
    fn answering_one_of_two_holders_leaves_the_other_and_the_step() {
        let fx = setup();
        let (step, a, b) = two_holders(&fx);
        let repo = DecisionRepo::new(&fx.conn);
        repo.answer(a.id, Some(1), None, "user").unwrap();
        assert_eq!(repo.get(b.id).unwrap().unwrap().status, DECISION_PENDING);
        assert_eq!(status_of(&fx, step), "blocked");
        // The last holder settled opens it.
        repo.answer(b.id, Some(1), None, "user").unwrap();
        assert_eq!(status_of(&fx, step), "ready");
    }

    #[test]
    fn withdrawing_one_of_two_holders_leaves_the_other_and_the_step() {
        let fx = setup();
        let (step, a, b) = two_holders(&fx);
        let repo = DecisionRepo::new(&fx.conn);
        repo.withdraw(a.id).unwrap();
        assert_eq!(repo.get(b.id).unwrap().unwrap().status, DECISION_PENDING);
        assert_eq!(status_of(&fx, step), "blocked");
        repo.withdraw(b.id).unwrap();
        assert_eq!(status_of(&fx, step), "ready");
    }

    #[test]
    fn a_shared_other_step_stays_blocked_while_any_holder_remains() {
        let fx = setup();
        let steps = blocked_steps(&fx, 2);
        let repo = DecisionRepo::new(&fx.conn);
        let a = repo.create(fx.node, None, None, &NewDecision { blocks: steps.clone(), ..decision("A?", &["ok"]) }).unwrap();
        let b = repo.create(fx.node, None, None, &NewDecision { blocks: vec![steps[1]], ..decision("B?", &["ok"]) }).unwrap();
        repo.answer(a.id, Some(1), None, "user").unwrap();
        assert_eq!(status_of(&fx, steps[0]), "ready");
        assert_eq!(status_of(&fx, steps[1]), "blocked");
        assert_eq!(repo.get(b.id).unwrap().unwrap().status, DECISION_PENDING);
    }

    #[test]
    fn retrying_a_step_settles_every_request_holding_it() {
        let fx = setup();
        let (step, a, b) = two_holders(&fx);
        crate::outline::repos::PlanStepRepo::new(&fx.conn).update_status(step, "in_progress", None, None).unwrap();
        let repo = DecisionRepo::new(&fx.conn);
        assert_eq!(repo.get(a.id).unwrap().unwrap().status, DECISION_WITHDRAWN);
        assert_eq!(repo.get(b.id).unwrap().unwrap().status, DECISION_WITHDRAWN);
    }

    #[test]
    fn restoring_a_step_keeps_it_blocked_while_a_request_is_pending() {
        use crate::conversation::EntitySnapshot;
        let fx = setup();
        NodeRepo::new(&fx.conn).enable_capability(fx.node, crate::outline::Capability::Spec).unwrap();
        let step = blocked_steps(&fx, 1)[0];
        let repo = DecisionRepo::new(&fx.conn);
        let old = repo.create(fx.node, None, None, &NewDecision { blocks: vec![step], ..decision("Old?", &["ok"]) }).unwrap();
        repo.answer(old.id, Some(1), None, "user").unwrap();
        crate::outline::repos::PlanStepRepo::new(&fx.conn)
            .update_status(step, "blocked", Some("again"), Some(&crate::outline::repos::plan_steps::HandoffReason::Access { needs: "x".into(), tried: "y".into() }))
            .unwrap();
        let pending = repo.create(fx.node, None, None, &NewDecision { blocks: vec![step], ..decision("New?", &["ok"]) }).unwrap();
        for snapshot_status in ["blocked", "ready"] {
            fx.conn.execute("DELETE FROM node_plan_steps WHERE id = ?1", params![uuid_to_blob(step)]).unwrap();
            let snapshot = EntitySnapshot::PlanStep {
                node_id: fx.node,
                ordinal: 0,
                body: "Step 0".into(),
                status: snapshot_status.into(),
                note: None,
                reason: None,
                depends_on: vec![],
                satisfies: vec![],
                phase: "active".into(),
            };
            crate::outline::mutations::restore_plan_step(&fx.conn, step, &snapshot).unwrap();
            assert_eq!(status_of(&fx, step), "blocked", "restored as {snapshot_status}");
            assert_eq!(repo.get(pending.id).unwrap().unwrap().status, DECISION_PENDING);
        }
    }

    #[test]
    fn a_question_withdrawn_by_a_retry_may_be_asked_again_but_never_twice_at_once() {
        let fx = setup();
        let steps = blocked_steps(&fx, 1);
        let repo = DecisionRepo::new(&fx.conn);
        let ask = |q: &str| NewDecision { guard_reask: true, blocks: steps.clone(), ..decision(q, &["ok"]) };
        repo.create(fx.node, None, None, &ask("Key?")).unwrap();
        // Still waiting: the same question again is refused, whatever the spacing.
        let err = repo.create(fx.node, None, None, &ask("key?  ")).unwrap_err().to_string();
        assert!(err.contains("already waiting"), "{err}");
        assert_eq!(repo.list_pending_for_node(fx.node).unwrap().len(), 1);
        // Retried (withdrawn), the same question is a new cause and may return once.
        crate::outline::repos::PlanStepRepo::new(&fx.conn).update_status(steps[0], "in_progress", None, None).unwrap();
        repo.create(fx.node, None, None, &ask("Key?")).unwrap();
        assert_eq!(repo.list_pending_for_node(fx.node).unwrap().len(), 1);
    }
}
