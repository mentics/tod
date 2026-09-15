//! Drafting writes, dispatched from [`crate::interview::execute`].

use super::repo::DraftingRepo;
use super::types::*;
use crate::interview::{ACTOR_USER, AgentSessionRow, ENTITY_OBLIGATION, PHASE_DESIGN};
use crate::outline::repos::GateRepo;
use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use crate::outline::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeGateEvaluation, OUTCOME_FAIL, OUTCOME_PASS,
    OUTCOME_PENDING, OutlineMutation, SOURCE_AGENT, SOURCE_HUMAN,
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub fn add_dump(conn: &Connection, node_id: Option<Uuid>, body: &str) -> Result<Value> {
    let body = body.trim();
    if body.is_empty() {
        bail!("a dump needs text");
    }
    let seq: i64 = conn.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM drafting_dumps", [], |r| {
        r.get(0)
    })?;
    conn.execute(
        "INSERT INTO drafting_dumps (id, seq, target_node_id, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            uuid_to_blob(Uuid::new_v4()),
            seq,
            node_id.map(uuid_to_blob),
            body,
            now_ms()
        ],
    )?;
    if let Some(node_id) = node_id {
        reset_buildable(conn, node_id)?;
    }
    Ok(json!({ "id": format!("d-{seq}") }))
}

pub fn add_choice(
    conn: &Connection,
    agent: Option<&AgentSessionRow>,
    node_id: Uuid,
    context: Option<&str>,
    question: &str,
    options: &[ChoiceOption],
) -> Result<Value> {
    let repo = DraftingRepo::new(conn);
    if question.trim().is_empty() {
        bail!("a choice needs a question");
    }
    if options.len() < 2 {
        bail!("a choice needs at least two options");
    }
    for option in options {
        if option.label.trim().is_empty() {
            bail!("every option needs a label");
        }
        for ob in &option.obligations {
            if ob.kind != KIND_REQUIREMENT && ob.kind != KIND_CONSTRAINT {
                bail!("option obligations need kind: requirement|constraint");
            }
            if ob.body.trim().is_empty() {
                bail!("option obligations need a body");
            }
            let missing = repo.missing_slugs(&ob.body)?;
            if !missing.is_empty() {
                bail!("no node has slug {}", missing.join(", "));
            }
        }
    }
    let open = repo.list_choices(node_id, &[CHOICE_OPEN])?.len();
    if open >= CHOICE_CAP {
        bail!(
            "this node already has {open} open choices (the cap is {CHOICE_CAP}); \
             draft your best call as a high-attention obligation instead"
        );
    }
    let phase = agent
        .map(|a| a.phase.clone())
        .unwrap_or_else(|| PHASE_DESIGN.to_string());
    let seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM drafting_choices WHERE node_id = ?1",
        params![uuid_to_blob(node_id)],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO drafting_choices
         (id, node_id, seq, phase, context, question, options, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8)",
        params![
            uuid_to_blob(Uuid::new_v4()),
            uuid_to_blob(node_id),
            seq,
            phase,
            context.map(str::trim).filter(|s| !s.is_empty()),
            question.trim(),
            serde_json::to_string(options)?,
            now_ms(),
        ],
    )?;
    Ok(json!({ "id": format!("c-{seq}") }))
}

/// `option: Some(n)` applies option `n`'s obligations as the acting party;
/// `None` delegates the call to the drafter ("You pick").
pub fn answer_choice(
    conn: &Connection,
    media_root: &Path,
    node_id: Uuid,
    seq: i64,
    option: Option<i64>,
) -> Result<Value> {
    let choice = choice(conn, node_id, seq)?;
    if choice.status != CHOICE_OPEN {
        bail!("{} is {}", choice.label(), choice.status);
    }
    let now = now_ms();
    let mut created = Vec::new();
    match option {
        Some(n) => {
            let picked = usize::try_from(n - 1)
                .ok()
                .and_then(|i| choice.options.get(i))
                .with_context(|| format!("{} has no option {n}", choice.label()))?;
            for ob in &picked.obligations {
                let id = Uuid::new_v4();
                OutlineMutation::CreateObligation {
                    obligation_id: Some(id),
                    node_id,
                    kind: ob.kind.clone(),
                    after_id: None,
                    before: false,
                    section: ob.section.clone(),
                    body: ob.body.trim().to_string(),
                    phase: choice.phase.clone(),
                }
                .execute(conn, media_root)?;
                created.push(id.to_string());
            }
            conn.execute(
                "UPDATE drafting_choices SET status = 'answered', answer = ?1, answered_at = ?2
                 WHERE id = ?3",
                params![n, now, uuid_to_blob(choice.id)],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE drafting_choices SET status = 'delegated', answered_at = ?1 WHERE id = ?2",
                params![now, uuid_to_blob(choice.id)],
            )?;
        }
    }
    reset_buildable(conn, node_id)?;
    Ok(json!({ "id": choice.label(), "created": created }))
}

pub fn withdraw_choice(conn: &Connection, node_id: Uuid, seq: i64) -> Result<Value> {
    let choice = choice(conn, node_id, seq)?;
    if choice.status != CHOICE_OPEN {
        bail!("{} is {}", choice.label(), choice.status);
    }
    conn.execute(
        "UPDATE drafting_choices SET status = 'withdrawn', processed_at = ?1 WHERE id = ?2",
        params![now_ms(), uuid_to_blob(choice.id)],
    )?;
    Ok(json!({ "id": choice.label() }))
}

/// The user confirms an `agent` obligation: provenance becomes `user`.
pub fn confirm_obligation(conn: &Connection, actor: &str, obligation_id: Uuid) -> Result<Value> {
    if actor != ACTOR_USER {
        bail!("only the user can confirm an obligation");
    }
    let (node_id, provenance): (Vec<u8>, String) = conn
        .query_row(
            "SELECT node_id, provenance FROM node_obligations WHERE id = ?1",
            params![uuid_to_blob(obligation_id)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("obligation not found")?;
    if provenance == PROVENANCE_USER {
        return Ok(json!({ "provenance": PROVENANCE_USER }));
    }
    let now = now_ms();
    conn.execute(
        "UPDATE node_obligations SET provenance = 'user', attention = NULL, attention_why = NULL,
                updated_at = ?1
         WHERE id = ?2",
        params![now, uuid_to_blob(obligation_id)],
    )?;
    // Provenance changes are part of the change log (override history), but
    // don't reset `buildable`: the meaning is unchanged.
    conn.execute(
        "INSERT INTO interview_changes (node_id, entity, entity_id, op, fields, actor, at)
         VALUES (?1, ?2, ?3, 'update', 'provenance', ?4, ?5)",
        params![node_id, ENTITY_OBLIGATION, uuid_to_blob(obligation_id), actor, now],
    )?;
    Ok(json!({ "provenance": PROVENANCE_USER }))
}

pub fn set_attention(
    conn: &Connection,
    obligation_id: Uuid,
    attention: &str,
    why: Option<&str>,
) -> Result<Value> {
    if !ATTENTION_LEVELS.contains(&attention) {
        bail!("attention must be low|medium|high");
    }
    let why = why.map(str::trim).filter(|s| !s.is_empty());
    let mark = DraftingRepo::new(conn)
        .mark(obligation_id)?
        .context("obligation not found")?;
    if !mark.is_agent() {
        bail!("the user stated or confirmed this obligation; it has no attention score");
    }
    conn.execute(
        "UPDATE node_obligations SET attention = ?1, attention_why = ?2 WHERE id = ?3",
        params![attention, why, uuid_to_blob(obligation_id)],
    )?;
    Ok(json!({}))
}

pub fn set_buildable(
    conn: &Connection,
    actor: &str,
    node_id: Uuid,
    outcome: &str,
    detail: Option<&str>,
) -> Result<Value> {
    if ![OUTCOME_PASS, OUTCOME_FAIL, OUTCOME_PENDING].contains(&outcome) {
        bail!("buildable outcome must be pass|fail|pending");
    }
    let repo = DraftingRepo::new(conn);
    if outcome == OUTCOME_PASS {
        let open = repo.list_choices(node_id, &[CHOICE_OPEN])?;
        if !open.is_empty() {
            bail!(
                "not buildable while choices are open ({})",
                open.iter().map(|c| c.label()).collect::<Vec<_>>().join(", ")
            );
        }
    }
    let criterion = GateRepo::new(conn)
        .get_by_slug(BUILDABLE_CRITERION_SLUG)?
        .context("buildable gate criterion is not seeded")?;
    GateRepo::new(conn).upsert_evaluation(&NodeGateEvaluation {
        node_id,
        criterion_id: criterion.id,
        outcome: outcome.to_string(),
        detail: detail.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        source: if actor == ACTOR_USER { SOURCE_HUMAN } else { SOURCE_AGENT }.to_string(),
        evaluated_at: now_ms(),
        action: crate::outline::repos::gate::ACTION_NONE.to_string(),
    })?;
    Ok(json!({ "outcome": outcome }))
}

/// Close out a drafter turn: store its change summary and mark the dumps and
/// choices it was given as taken in.
pub fn record_drafting_turn(
    conn: &Connection,
    node_id: Uuid,
    summary: Option<&str>,
    dump_seqs: &[i64],
    choice_seqs: &[i64],
) -> Result<Value> {
    let now = now_ms();
    if let Some(summary) = summary.map(str::trim).filter(|s| !s.is_empty()) {
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM drafting_summaries WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO drafting_summaries (id, node_id, seq, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![uuid_to_blob(Uuid::new_v4()), uuid_to_blob(node_id), seq, summary, now],
        )?;
    }
    for seq in dump_seqs {
        conn.execute(
            "UPDATE drafting_dumps SET routed_at = COALESCE(routed_at, ?1) WHERE seq = ?2",
            params![now, seq],
        )?;
    }
    for seq in choice_seqs {
        conn.execute(
            "UPDATE drafting_choices SET processed_at = COALESCE(processed_at, ?1)
             WHERE node_id = ?2 AND seq = ?3",
            params![now, uuid_to_blob(node_id), seq],
        )?;
    }
    Ok(json!({}))
}

/// Refuse an agent's obligation text when it has no words (a placeholder such
/// as `-`) or references a slug no node has.
pub fn check_references(conn: &Connection, mutation: &OutlineMutation) -> Result<()> {
    let body = match mutation {
        OutlineMutation::CreateObligation { body, .. }
        | OutlineMutation::UpdateObligationBody { body, .. } => body,
        _ => return Ok(()),
    };
    if !body.chars().any(char::is_alphanumeric) {
        bail!(
            "obligation text `{}` has no words: pass the text itself, or `--body -` with the text on stdin",
            body.trim()
        );
    }
    let missing = DraftingRepo::new(conn).missing_slugs(body)?;
    if !missing.is_empty() {
        bail!(
            "no node has slug {} (find one with `tod-cli node search`, or create it first)",
            missing
                .iter()
                .map(|s| format!("[[{s}]]"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn choice(conn: &Connection, node_id: Uuid, seq: i64) -> Result<DraftingChoice> {
    DraftingRepo::new(conn)
        .get_choice(node_id, seq)?
        .with_context(|| format!("c-{seq} not found on this node"))
}

fn reset_buildable(conn: &Connection, node_id: Uuid) -> Result<()> {
    conn.execute(
        "UPDATE node_gate_evaluations SET outcome = 'pending', detail = NULL, evaluated_at = ?1
         WHERE node_id = ?2 AND outcome != 'pending'
           AND criterion_id = (SELECT id FROM gate_criteria WHERE slug = ?3)",
        params![now_ms(), uuid_to_blob(node_id), BUILDABLE_CRITERION_SLUG],
    )?;
    Ok(())
}
