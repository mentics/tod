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

/// Not yet answered.
pub const DECISION_PENDING: &str = "pending";
/// Has at least one answer.
pub const DECISION_ANSWERED: &str = "answered";
/// Withdrawn: no longer needs an answer.
pub const DECISION_WITHDRAWN: &str = "withdrawn";

/// Every decision status.
pub const DECISION_STATUSES: [&str; 3] = [DECISION_PENDING, DECISION_ANSWERED, DECISION_WITHDRAWN];

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
    /// The plan steps this decision holds up (`decisions ask --blocks`, or
    /// `plan update --decision`): one cause, one request. Answering the
    /// decision reopens the ones still handed back, and retrying one of
    /// them resolves the decision (see [`DecisionRepo::answer`] and
    /// [`DecisionRepo::release_for_step`]).
    pub blocks: Vec<Uuid>,
    /// What the agent said was new when it asked again after an answer
    /// (`decisions ask --new-evidence`); shown to the user with the request.
    pub new_evidence: String,
}

impl Decision {
    /// The state picking `option` (1-based) moves the node back to, if it does.
    pub fn move_for(&self, option: i64) -> Option<&str> {
        self.moves.iter().find(|m| m.option == option).map(|m| m.to.as_str())
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
}

/// The actor of an answer the app recorded, not the user or an agent. Such an
/// answer is never shown to an agent as the user's, and never counts against
/// asking again.
pub const SETTLED_BY: &str = "tod";

/// How long an answer stands against the same question being asked again:
/// 30 minutes, by the clock, whatever the number of turns in between.
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
                            CHECK (status IN ('pending','answered','withdrawn')),
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
    "id, node_id, conversation_id, protocol, question, options, evidence, status, created_at, reason, option_moves, blocks, new_evidence";

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

const ANSWER_COLUMNS: &str = "id, decision_id, option_index, text, actor, answered_at";

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
        let options: Vec<String> = decision
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
        let mut blocks: Vec<Uuid> = Vec::new();
        for step in &decision.blocks {
            if blocks.contains(step) {
                continue;
            }
            self.check_blockable(node_id, *step)?;
            blocks.push(*step);
        }
        if decision.guard_reask {
            self.check_not_pending(node_id, question)?;
            if decision.new_evidence.trim().is_empty() {
                self.check_not_reasked(node_id, question)?;
            }
        }
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO decisions
             (id, node_id, conversation_id, protocol, question, options, evidence, status, created_at, reason, option_moves, blocks, new_evidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, ?11, ?12)",
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

    /// Refuse a question that is already waiting on this node (the same text,
    /// case and spacing aside): a question withdrawn because its step was
    /// retried may be asked again for a new cause, but never twice at once.
    fn check_not_pending(&self, node_id: Uuid, question: &str) -> Result<()> {
        let normal = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let wanted = normal(question);
        if let Some(d) = self.list_pending_for_node(node_id)?.into_iter().find(|d| normal(&d.question) == wanted) {
            bail!(
                "this question is already waiting for the user's answer (request {}): do not ask it twice. \
                 Carry on with other work until it is answered.",
                crate::interview::short_id(d.id)
            );
        }
        Ok(())
    }

    /// Refuse a question the user already answered on this node a moment ago
    /// (the same text, case and spacing aside): asking again makes them
    /// answer twice for one cause. Only the same question counts: a different
    /// question that holds up the same step is a different cause. Answers by
    /// `tod` (not the user's) never count. The message names the answer, and
    /// the way through when something really is new.
    fn check_not_reasked(&self, node_id: Uuid, question: &str) -> Result<()> {
        let since = now_ms() - REASK_WINDOW_MS;
        let normal = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let wanted = normal(question);
        for decision in self.list_for_node(node_id)? {
            if decision.status != DECISION_ANSWERED {
                continue;
            }
            let Some(found) = self.get_with_answers(decision.id)? else { continue };
            let Some(answer) =
                found.answers.iter().rev().find(|a| a.answered_at >= since && a.actor != SETTLED_BY)
            else {
                continue;
            };
            if normal(&decision.question) != wanted {
                continue;
            }
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
            let minutes = ((now_ms() - answer.answered_at) / 60_000).max(0);
            bail!(
                "this was already answered {minutes} minute(s) ago ({} by {}): {given}. Carry on with that \
                 answer; the steps it held up are open again. Ask again only for something new, by passing \
                 --new-evidence <TEXT> saying what changed since (what you tried with the answer, and the \
                 error it gave).",
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
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO decision_answers (decision_id, option_index, text, actor, answered_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![uuid_to_blob(decision_id), option, text, actor, now],
        )?;
        if decision.status == DECISION_PENDING {
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
        self.conn
            .query_row(
                &format!("SELECT {ANSWER_COLUMNS} FROM decision_answers WHERE id = ?1"),
                params![id],
                map_answer,
            )
            .context("answer vanished after insert")
    }

    /// Set the still-handed-back steps `decision` holds up to `ready`.
    fn reopen_blocked_steps(&self, decision: &Decision) -> Result<()> {
        let steps = crate::outline::repos::PlanStepRepo::new(self.conn);
        for id in &decision.blocks {
            let Some(step) = steps.get(*id)? else { continue };
            if !crate::outline::repos::plan_steps::needs_user(&step.status) {
                continue;
            }
            // Another pending decision still holds this step up: its cause
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
            .list_pending_for_node(node_id)?
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
            .list_pending_for_node(node_id)?
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
            if decision.status != DECISION_PENDING {
                continue;
            }
            self.conn.execute(
                "UPDATE decisions SET status = 'withdrawn' WHERE id = ?1 AND status = 'pending'",
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
            .any(|d| d.status != DECISION_PENDING && d.blocks.contains(&step)))
    }

    /// Whether `steps` may be held up by `decision_id` (pending, same node);
    /// what [`Self::link_steps`] checks, so a caller can validate before
    /// writing anything else.
    pub fn check_linkable(&self, decision_id: Uuid, steps: &[Uuid]) -> Result<()> {
        let decision = self
            .get(decision_id)?
            .with_context(|| format!("decision {decision_id} not found"))?;
        if decision.status != DECISION_PENDING {
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
            .list_pending_for_node(node_id)?
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
        let ids: Vec<Uuid> = self.list_pending_for_node(node_id)?.into_iter().map(|d| d.id).collect();
        self.withdraw_ids(&ids, false)
    }

    /// Withdraw the node's pending decisions asked by an agent of one of
    /// `protocols`, except `keep`: the stay they were asked in is over (the
    /// node was moved back past it). The steps they held up open again, for
    /// the same reason. Returns how many.
    pub fn withdraw_for_protocols(&self, node_id: Uuid, protocols: &[&str], keep: Option<Uuid>) -> Result<usize> {
        let ids: Vec<Uuid> = self
            .list_pending_for_node(node_id)?
            .into_iter()
            .filter(|d| d.protocol.as_deref().is_some_and(|p| protocols.contains(&p)) && Some(d.id) != keep)
            .map(|d| d.id)
            .collect();
        self.withdraw_ids(&ids, true)
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
        let dir = std::env::temp_dir().join(format!("tod-decisions-{}", Uuid::new_v4()));
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
        let dir = std::env::temp_dir().join(format!("tod-decisions-migrate-{}", Uuid::new_v4()));
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
        // A different question is a different cause, even for the same step.
        repo.create(fx.node, None, None, &guarded("Different words", vec![steps[0]], "")).unwrap();
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
    fn an_old_answer_no_longer_blocks_the_question() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo.create(fx.node, None, None, &decision("Old question?", &["yes"])).unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        fx.conn
            .execute("UPDATE decision_answers SET answered_at = answered_at - ?1", params![REASK_WINDOW_MS + 1000])
            .unwrap();
        repo.create(fx.node, None, None, &NewDecision { guard_reask: true, ..decision("Old question?", &["yes"]) })
            .unwrap();
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
