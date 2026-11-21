//! The writers of improvement lessons (`doc/requests/design.md` 11.1), every
//! one mechanical (no model) and idempotent per source event through the
//! lesson's `event_key` ([`super::record_once`]), and the reads and writes the
//! process review and `tod-cli lessons` use.

use super::*;

/// Where lessons that belong to tod's own triage brief guidance point.
pub const TARGET_TRIAGE_BRIEF: &str = "triage brief guidance (crates/tod/media/context/surface/triage.md)";

/// The role doc of a lifecycle phase.
pub fn role_doc_target(phase: &str) -> String {
    format!("role doc of the `{phase}` phase (assets/process/agents/state/{phase}.md)")
}

/// Where the lesson of a triage reflection belongs, as specifically as the
/// request says (`doc/requests/design.md` 8.7). Empty for `necessary`: asking
/// was right, and nothing is to be improved.
pub fn triage_target(category: &str, reason: &str, protocol: Option<&str>) -> String {
    let asker = protocol.unwrap_or("an agent");
    match category {
        CATEGORY_ASKER_COULD_DECIDE => {
            format!("the asking phase's role doc, stance or a project skill (asked by {asker}, reason `{reason}`)")
        }
        CATEGORY_APP_SHOULD_HANDLE => {
            format!("tod code: rung 0 or a fix (a `{reason}` request from {asker} the app could have settled)")
        }
        CATEGORY_UPSTREAM_UNCLEAR => "the role doc of the earlier phase that left it unclear (named in the summary)".to_string(),
        CATEGORY_BRIEF_UNCLEAR => TARGET_TRIAGE_BRIEF.to_string(),
        _ => String::new(),
    }
}

/// Whether a triage reflection was recorded for `decision` (a user's or
/// another source's lesson about the same request does not count).
pub fn triage_lesson_exists(conn: &Connection, decision: Uuid) -> Result<bool> {
    Ok(by_event(conn, &triage_key(decision))?.is_some())
}

fn triage_key(decision: Uuid) -> String {
    format!("triage:{}", decision.simple())
}

/// A triage reflection: the one lesson triage records when a request it read
/// closes (`doc/requests/design.md` 8.7). The only writer of a triage lesson.
/// The request's closing is what it learns from: its `category`, what
/// `summary` says went wrong or right, and the user's own `user_quote` when
/// they said something about it. Its `target` is set from the category and the
/// request's reason.
pub fn record_triage_lesson(
    conn: &Connection,
    decision: Uuid,
    conversation: Uuid,
    category: &str,
    summary: &str,
    user_quote: &str,
) -> Result<Lesson> {
    if !TRIAGE_CATEGORIES.contains(&category) {
        bail!("unknown category `{category}` (expected {})", TRIAGE_CATEGORIES.join("|"));
    }
    let Some(found) = crate::decisions::DecisionRepo::new(conn).get(decision)? else {
        bail!("decision {decision} not found");
    };
    if found.triage_conversation_id != Some(conversation) {
        bail!("decision {decision} is not the request this triage session read");
    }
    if crate::decisions::is_open(&found.status) {
        bail!("the request is still open: reflect once it is answered");
    }
    let new = NewLesson {
        node_id: found.node_id,
        source: SOURCE_TRIAGE.to_string(),
        decision_id: Some(decision),
        conversation_id: Some(conversation),
        category: category.to_string(),
        target: triage_target(category, &found.reason, found.protocol.as_deref()),
        summary: summary.to_string(),
        user_quote: user_quote.to_string(),
        ..Default::default()
    };
    record_once(conn, &new, &triage_key(decision))?
        .ok_or_else(|| anyhow::anyhow!("a lesson was already recorded for this request"))
}

fn send_back_key(id: Uuid) -> String {
    format!("send_back:{}", id.simple())
}

/// A send-back or a dispute becomes a lesson (`doc/requests/design.md` 11.1,
/// source `send_back`: an earlier phase was unclear or wrong), written from its
/// reason when it is made and revised when its resolution is written: one
/// lesson per send-back, `upstream_unclear` until a dispute is resolved by a
/// fix, then `upstream_wrong`. A dispute passed up is noted and left for the
/// phase that settles it. Called by `send_back` for every move and every
/// resolution; running it again for the same state changes nothing.
pub fn record_dispute_lesson(conn: &Connection, row: &crate::send_back::SendBack) -> Result<()> {
    use crate::send_back::{RESOLUTION_FIXED, RESOLUTION_PASSED_UP};
    let what = if row.is_dispute() {
        format!(
            "The `{}` session disputed {} obligation(s) from `{}` and sent the task back to `{}`",
            row.protocol,
            row.obligations.len(),
            row.from_state,
            row.to_state
        )
    } else {
        format!("The `{}` session sent the task back from `{}` to `{}`", row.protocol, row.from_state, row.to_state)
    };
    let mut summary = format!("{what}: {}", row.reason.trim());
    let mut category = CATEGORY_UPSTREAM_UNCLEAR;
    if !row.resolution_kind.is_empty() {
        summary.push_str(&format!("\nResolution ({}): {}", row.resolution_kind, row.resolution.trim()));
        match row.resolution_kind.as_str() {
            RESOLUTION_FIXED => {
                category = CATEGORY_UPSTREAM_WRONG;
                summary.push_str(&format!(
                    "\nThe obligation was wrong; what the `{}` phase wrote should have been caught there.",
                    row.to_state
                ));
            }
            RESOLUTION_PASSED_UP => {}
            _ => summary.push_str("\nThe obligation held; it was only unclear how it was meant."),
        }
    }
    let key = send_back_key(row.id);
    let target = role_doc_target(&row.to_state);
    match by_event(conn, &key)? {
        None => {
            record_once(
                conn,
                &NewLesson {
                    node_id: row.node_id,
                    source: SOURCE_SEND_BACK.to_string(),
                    conversation_id: row.conversation_id,
                    send_back_id: Some(row.id),
                    category: category.to_string(),
                    target,
                    summary,
                    ..Default::default()
                },
                &key,
            )?;
        }
        // Once acted on or declined, a lesson's words stay as they were read.
        Some(found) if found.status == STATUS_APPLIED || found.status == STATUS_DECLINED => {}
        Some(found) => {
            if found.summary != summary || found.category != category {
                conn.execute(
                    "UPDATE improvement_lessons SET category = ?2, summary = ?3, target = ?4, updated_at = ?5 WHERE id = ?1",
                    params![uuid_to_blob(found.id), category, summary, target, now_ms()],
                )?;
            }
        }
    }
    Ok(())
}

/// A quick fix that had to escalate to the Fix protocol
/// (`doc/requests/design.md` 5): rung 1 could not settle it, so either the
/// quick-fix guidance, the check's configuration or tod's mechanics could be
/// better. Once per attempt.
pub fn record_quick_fix_escalation(conn: &Connection, fix: &crate::quick_fixes::QuickFix) -> Result<()> {
    let notes = fix.notes.trim();
    let summary = format!(
        "The quick fix of `{}` ({} turn(s), in `{}`) did not make it pass and escalated to the Fix protocol.{}",
        fix.check_name,
        fix.turns,
        fix.phase,
        if notes.is_empty() { String::new() } else { format!(" Its notes: {}", clip(notes, 600)) }
    );
    record_once(
        conn,
        &NewLesson {
            node_id: fix.node_id,
            source: SOURCE_LADDER.to_string(),
            conversation_id: fix.conversation_id,
            category: CATEGORY_APP_SHOULD_HANDLE.to_string(),
            target: format!("quick fix guidance (surface/quick-fix.md), or skip rung 1 for the check `{}`", fix.check_name),
            summary,
            ..Default::default()
        },
        &format!("ladder:quick_fix:{}", fix.id.simple()),
    )?;
    Ok(())
}

/// A check's configured fixer failed, so the failure went on to an agent
/// (`doc/requests/design.md` 4.2). Once per node and check, however many rounds.
pub fn record_fixer_failure(conn: &Connection, node: Uuid, check_name: &str, check_key: &str, why: &str) -> Result<()> {
    record_once(
        conn,
        &NewLesson {
            node_id: node,
            source: SOURCE_LADDER.to_string(),
            category: CATEGORY_APP_SHOULD_HANDLE.to_string(),
            target: format!("the configured fix of the check `{check_name}` (tod.yml), or tod's fixer"),
            summary: format!("The configured fix of `{check_name}` did not make it pass: {}", clip(why, 600)),
            ..Default::default()
        },
        &format!("ladder:fixer:{}:{}", node.simple(), key_hash(check_key)),
    )?;
    Ok(())
}

fn key_hash(key: &str) -> String {
    // FNV-1a: stable across runs and builds, unlike the std hasher.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn clip(text: &str, chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(chars).collect();
    out.push_str(" ...");
    out
}

/// An incident of kind `tool_broken` (`tod-cli` or a shim fails with one
/// message) opened or resolved: a tod bug candidate. `node` is the node that
/// met it first (the lesson belongs to it); `phase` is `"opened"` or
/// `"resolved"`, and each is written once per incident.
pub fn record_tool_incident(
    conn: &Connection,
    incident: Uuid,
    node: Uuid,
    summary: &str,
    signature: &str,
    phase: &str,
    nodes: usize,
) -> Result<()> {
    let text = match phase {
        "opened" => format!(
            "A tool tod runs broke on {nodes} node(s) at once: {summary} (signature `{signature}`). A tod bug candidate."
        ),
        _ => format!(
            "The broken-tool incident `{signature}` ({summary}) resolved after it held {nodes} node(s). Whatever fixed it is what tod should have prevented or handled."
        ),
    };
    record_once(
        conn,
        &NewLesson {
            node_id: node,
            source: SOURCE_INCIDENT.to_string(),
            incident_id: Some(incident),
            category: CATEGORY_APP_SHOULD_HANDLE.to_string(),
            target: "tod code: tod-cli or the shims (a tool_broken incident)".to_string(),
            summary: text,
            ..Default::default()
        },
        &format!("incident:{}:{phase}", incident.simple()),
    )?;
    Ok(())
}

/// Move the retired `request_feedback` rows into lessons (source `user`): a
/// "should not have asked" is `asker_could_decide`, a "bad question" is
/// `brief_unclear`, each quoting the user's note. The table stays where it is.
/// Once per row, so it is safe to run again. Returns how many were added.
pub fn migrate_request_feedback(conn: &Connection) -> Result<usize> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'request_feedback'",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    type FeedbackRow = (Vec<u8>, Vec<u8>, String, Vec<u8>, String, Option<Vec<u8>>, Option<String>, String, Option<String>, i64);
    let rows: Vec<FeedbackRow> = {
        let mut stmt = conn.prepare(
            "SELECT id, node_id, request_kind, request_id, reason, conversation_id, protocol, verdict, note, created_at
             FROM request_feedback ORDER BY created_at, rowid",
        )?;
        stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let mut added = 0;
    for (id, node, kind, request, reason, conversation, protocol, verdict, note, created_at) in rows {
        let id = blob_to_uuid_sql(&id)?;
        let key = format!("feedback:{}", id.simple());
        if by_event(conn, &key)?.is_some() {
            continue;
        }
        let (category, said) = if verdict == "should_not_ask" {
            (CATEGORY_ASKER_COULD_DECIDE, "the user said this request should not have been asked: the agent had what it needed")
        } else {
            (CATEGORY_BRIEF_UNCLEAR, "the user said this request was a bad question: malformed or unclear")
        };
        let new = NewLesson {
            node_id: blob_to_uuid_sql(&node)?,
            source: SOURCE_USER.to_string(),
            decision_id: (kind == "decision").then(|| blob_to_uuid_sql(&request)).transpose()?,
            conversation_id: conversation.as_deref().map(blob_to_uuid_sql).transpose()?,
            category: category.to_string(),
            target: triage_target(category, &reason, protocol.as_deref()),
            summary: format!("A `{kind}` request (reason `{reason}`): {said}."),
            user_quote: note.unwrap_or_default(),
            ..Default::default()
        };
        let lesson = insert(conn, &new, &key)?;
        // The lesson keeps the time the user clicked.
        conn.execute(
            "UPDATE improvement_lessons SET created_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![uuid_to_blob(lesson.id), created_at],
        )?;
        added += 1;
    }
    Ok(added)
}

/// Lessons, oldest first, optionally of one `status` and one `source`.
pub fn list(conn: &Connection, status: Option<&str>, source: Option<&str>) -> Result<Vec<Lesson>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM improvement_lessons
         WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR source = ?2)
         ORDER BY created_at, rowid"
    ))?;
    let rows = stmt.query_map(params![status, source], lesson_of)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The lesson `raw` names: the start of its id (at least 4 characters).
pub fn resolve(conn: &Connection, raw: &str) -> Result<Uuid> {
    let raw = raw.trim().to_ascii_lowercase().replace('-', "");
    if raw.len() < 4 {
        bail!("`{raw}` is too short to name a lesson: give at least 4 characters of its id");
    }
    let matches: Vec<Uuid> =
        list(conn, None, None)?.into_iter().map(|l| l.id).filter(|id| id.simple().to_string().starts_with(&raw)).collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => bail!("no lesson's id starts with `{raw}`"),
        _ => bail!("`{raw}` starts more than one lesson's id: give more of it"),
    }
}

/// The lessons a process-review run took in.
pub fn for_run(conn: &Connection, run_node: Uuid) -> Result<Vec<Lesson>> {
    let mut stmt = conn
        .prepare(&format!("SELECT {COLUMNS} FROM improvement_lessons WHERE run_node_id = ?1 ORDER BY created_at, rowid"))?;
    let rows = stmt.query_map(params![uuid_to_blob(run_node)], lesson_of)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Take `ids` (lessons still `new`) into the run made as `run_node`:
/// `grouped`, at `now`. Returns how many were taken.
pub fn group_into(conn: &Connection, ids: &[Uuid], run_node: Uuid, now: i64) -> Result<usize> {
    let mut taken = 0;
    for id in ids {
        taken += conn.execute(
            "UPDATE improvement_lessons SET status = 'grouped', run_node_id = ?2, grouped_at = ?3, updated_at = ?3
             WHERE id = ?1 AND status = 'new'",
            params![uuid_to_blob(*id), uuid_to_blob(run_node), now],
        )?;
    }
    Ok(taken)
}

/// When the last process-review run took lessons in.
pub fn last_grouped_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT MAX(grouped_at) FROM improvement_lessons", [], |r| r.get(0))?)
}

/// A write of the lessons the user, an agent or the process review make
/// (`InterviewCommand::Lessons`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// A lesson in the user's own words: what a highlight, or a remark in the
    /// chat about how something could have been avoided, records.
    Add {
        node: Uuid,
        text: String,
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        target: String,
    },
    /// The user flagged a request as confusing with **This is confusing**, and
    /// no triage session read it to explain itself: a lesson (source `user`,
    /// category `brief_unclear`) linked to the request and the conversation
    /// that asked it, with the user's remark (the highlighted words, or what
    /// they said) when there is one. Only the user.
    FlagRequest {
        decision: Uuid,
        #[serde(default)]
        remark: String,
    },
    /// The user declines a lesson: it is not acted on.
    Decline { id: Uuid },
    /// A lesson was acted on: by the commit or node `applied_ref`. By a
    /// process-review session (only for the lessons its run took in) or the user.
    Apply { ids: Vec<Uuid>, applied_ref: String },
    /// A check's configured fixer ran and the check still failed, so the failure
    /// goes on to an agent (rung 2 of the ladder). Once per task and check.
    FixerFailed { node: Uuid, check_name: String, check_key: String, why: String },
    /// tod takes lessons (still `new`) into the process-review run made as
    /// `run_node`. Only the app does this, never a session.
    Group { ids: Vec<Uuid>, run_node: Uuid },
}

fn session_of(conn: &Connection, actor: &str) -> Result<Option<(Uuid, crate::conversation::Conversation)>> {
    let Some(id) =
        crate::conversation::actor_conversation(actor).or_else(|| crate::conversation::agent_actor_conversation(actor))
    else {
        return Ok(None);
    };
    let found = crate::conversation::ConversationRepo::new(conn)
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("conversation {id} not found ({actor})"))?;
    Ok(Some((id, found)))
}

/// Runs `op` as `actor` and returns its result.
pub fn run_op(conn: &Connection, actor: &str, op: &Op) -> Result<serde_json::Value> {
    match op {
        Op::Add { node, text, category, target } => {
            let category = category.as_deref().unwrap_or(CATEGORY_ASKER_COULD_DECIDE);
            if text.trim().is_empty() {
                bail!("a lesson needs the user's words: `--text`");
            }
            let conversation = session_of(conn, actor)?.map(|(id, _)| id);
            let lesson = record(
                conn,
                &NewLesson {
                    node_id: *node,
                    source: SOURCE_USER.to_string(),
                    conversation_id: conversation,
                    category: category.to_string(),
                    target: target.clone(),
                    summary: text.clone(),
                    user_quote: text.clone(),
                    ..Default::default()
                },
            )?;
            Ok(serde_json::json!({ "id": lesson.id }))
        }
        Op::FlagRequest { decision, remark } => {
            if session_of(conn, actor)?.is_some() {
                bail!("only the user flags a request as confusing");
            }
            let found = crate::decisions::DecisionRepo::new(conn)
                .get(*decision)?
                .ok_or_else(|| anyhow::anyhow!("decision {decision} not found"))?;
            let remark = remark.trim();
            let first = found.question.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            let first: String = first.chars().take(160).collect();
            let asker = found.protocol.as_deref().unwrap_or("an agent");
            let mut summary = format!(
                "The user found request {} confusing (a `{}` request from {asker}): \"{first}\"",
                crate::interview::short_id(found.id),
                found.reason,
            );
            if !remark.is_empty() {
                summary.push_str(&format!("\nThey said: {remark}"));
            }
            let lesson = record(
                conn,
                &NewLesson {
                    node_id: found.node_id,
                    source: SOURCE_USER.to_string(),
                    decision_id: Some(found.id),
                    conversation_id: found.conversation_id,
                    category: CATEGORY_BRIEF_UNCLEAR.to_string(),
                    target: format!(
                        "how the asking phase words a request: its role doc or stance (asked by {asker}, reason `{}`)",
                        found.reason
                    ),
                    summary,
                    user_quote: remark.to_string(),
                    ..Default::default()
                },
            )?;
            Ok(serde_json::json!({ "id": lesson.id }))
        }
        Op::Decline { id } => {
            if session_of(conn, actor)?.is_some() {
                bail!("only the user declines a lesson");
            }
            let found = get(conn, *id)?.ok_or_else(|| anyhow::anyhow!("lesson {id} not found"))?;
            if found.status == STATUS_APPLIED {
                bail!("lesson {id} was already applied ({}): it cannot be declined", found.applied_ref);
            }
            conn.execute(
                "UPDATE improvement_lessons SET status = 'declined', updated_at = ?2 WHERE id = ?1",
                params![uuid_to_blob(*id), now_ms()],
            )?;
            Ok(serde_json::json!({ "id": id }))
        }
        Op::FixerFailed { node, check_name, check_key, why } => {
            if session_of(conn, actor)?.is_some() {
                bail!("only the app records a fixer failure");
            }
            record_fixer_failure(conn, *node, check_name, check_key, why)?;
            Ok(serde_json::json!({ "node": node }))
        }
        Op::Group { ids, run_node } => {
            if session_of(conn, actor)?.is_some() {
                bail!("only the app groups lessons into a process review");
            }
            Ok(serde_json::json!({ "grouped": group_into(conn, ids, *run_node, now_ms())? }))
        }
        Op::Apply { ids, applied_ref } => {
            if applied_ref.trim().is_empty() {
                bail!("name what acted on the lessons: `--ref` (a commit, or the node)");
            }
            if ids.is_empty() {
                bail!("name the lessons that were acted on");
            }
            let session = session_of(conn, actor)?;
            let mut run = None;
            if let Some((_, conversation)) = &session {
                if conversation.protocol != crate::conversation::ProtocolKind::ProcessReview {
                    bail!(
                        "only the process review (or the user) marks a lesson applied, not a {} session",
                        conversation.protocol.as_str()
                    );
                }
                run = match conversation.focus {
                    crate::conversation::Focus::Node(n) => Some(n),
                    _ => None,
                };
            }
            for id in ids {
                let found = get(conn, *id)?.ok_or_else(|| anyhow::anyhow!("lesson {id} not found"))?;
                if session.is_some() && (found.run_node_id.is_none() || found.run_node_id != run) {
                    bail!("lesson {id} is not one this review's run took in");
                }
                if found.status == STATUS_DECLINED {
                    bail!("lesson {id} was declined: it is not applied");
                }
            }
            let now = now_ms();
            for id in ids {
                conn.execute(
                    "UPDATE improvement_lessons SET status = 'applied', applied_ref = ?2, updated_at = ?3 WHERE id = ?1",
                    params![uuid_to_blob(*id), applied_ref.trim(), now],
                )?;
            }
            Ok(serde_json::json!({ "applied": ids.len() }))
        }
    }
}
