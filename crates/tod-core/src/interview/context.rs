//! What interview agents are told: a compact snapshot when a session starts,
//! then only what changed since that session's previous turn — never anything
//! the session already has.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::fmt::Write as _;
use std::path::Path;
use tod_store::interview::*;
use tod_store::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use tod_store::outline::{
    EXTRA_CONTENT_GOAL, EXTRA_CONTENT_SUMMARY, KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation,
    PlanStep, ancestor_chain, phase_visible, resolve_obligations, uuid_to_blob,
};
use uuid::Uuid;

/// Whose context is being built.
pub struct ContextScope<'a> {
    pub node_id: Uuid,
    /// Stored phase (`requirements` | `design` | `planning`).
    pub phase: &'a str,
    pub role: Role,
    pub interview_session_id: Option<Uuid>,
    pub data_root: &'a Path,
    pub tod_cli: &'a Path,
    pub answered_cap: usize,
}

/// Rough token count for `text`.
pub fn estimate_tokens(text: &str) -> i64 {
    (text.len() / 4) as i64
}

/// Content types part of a phase's context. Design decisions live as
/// design-phase obligations now (see `resolve_obligations`), not as an extra
/// content blob, so every phase only ever needs `goal` here.
fn phase_content_types(_phase: &str) -> &'static [&'static str] {
    &["goal"]
}

pub(crate) fn plan_step_line(step: &PlanStep, deps: &[Uuid], obligations: &[Uuid]) -> String {
    let deps = if deps.is_empty() {
        String::new()
    } else {
        format!(
            " deps=[{}]",
            deps.iter().map(|id| short_id(*id)).collect::<Vec<_>>().join(",")
        )
    };
    let satisfies = if obligations.is_empty() {
        String::new()
    } else {
        format!(
            " satisfies=[{}]",
            obligations
                .iter()
                .map(|id| short_id(*id))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    format!(
        "[{}] {}{deps}{satisfies}: {}",
        short_id(step.id),
        step.status,
        one_line(&step.body)
    )
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn indent(text: &str, prefix: &str) -> String {
    text.trim()
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn obligation_line(o: &NodeObligation) -> String {
    let section = o
        .section
        .as_deref()
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    let visual_design = if o.visual_design_path.is_some() {
        " [visual design attached]"
    } else {
        ""
    };
    format!(
        "[{}] {}{section}: {}{visual_design}",
        short_id(o.id),
        o.kind,
        one_line(&o.body)
    )
}

fn node_title(nodes: &NodeRepo<'_>, id: Uuid) -> String {
    if id.is_nil() {
        return "global".into();
    }
    nodes
        .get(id)
        .ok()
        .flatten()
        .map(|n| n.title)
        .unwrap_or_else(|| short_id(id))
}

/// Renders `node_id`'s ancestor (and global) obligations for interview and
/// gate-check context. Each ancestor contributes its title, its generated
/// summary (`EXTRA_CONTENT_SUMMARY`), and its constraint-kind obligations in
/// full. Its requirements are never listed: the summary stands in for them,
/// and a deep tree would otherwise put hundreds into every context. The
/// drafting driver writes missing summaries before a turn
/// (`crate::drafting::summary`); anywhere else, an ancestor still without
/// one gets a pointer to `tod-cli` instead. Global (no owning node)
/// obligations always show in full; there is nothing to summarize about
/// them.
pub fn render_inherited_context(
    conn: &Connection,
    nodes: &NodeRepo<'_>,
    node_id: Uuid,
    max_phase: Option<&str>,
) -> Result<String> {
    let inherited: Vec<_> = resolve_obligations(conn, node_id, max_phase)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.source_node_id != node_id)
        .collect();
    if inherited.is_empty() {
        return Ok(String::new());
    }

    let mut order: Vec<Uuid> = Vec::new();
    let mut groups: std::collections::HashMap<Uuid, Vec<NodeObligation>> =
        std::collections::HashMap::new();
    for item in inherited {
        if !order.contains(&item.source_node_id) {
            order.push(item.source_node_id);
        }
        groups.entry(item.source_node_id).or_default().push(item.obligation);
    }

    let mut out = String::new();
    out.push_str("\n## Inherited context (ancestors)\n\n");
    out.push_str(
        "Each ancestor below is summarized, not fully restated — its scope \
         is settled and out of bounds here. Only decide what belongs to \
         *this* node; a gap in an ancestor's own scope belongs on that \
         ancestor, not as a question or obligation on this node.\n",
    );

    for source_id in order {
        let items = groups.remove(&source_id).unwrap_or_default();
        if source_id.is_nil() {
            out.push_str("\n### Global\n");
            for o in &items {
                writeln!(out, "- {}", obligation_line(o))?;
            }
            continue;
        }
        let title = node_title(nodes, source_id);
        writeln!(out, "\n### From \"{title}\"")?;
        let summary = nodes
            .get_extra_content(source_id, EXTRA_CONTENT_SUMMARY)
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty());
        let constraints: Vec<&NodeObligation> =
            items.iter().filter(|o| o.kind == KIND_CONSTRAINT).collect();
        match summary {
            Some(summary) => writeln!(out, "{}", one_line(&summary))?,
            None if constraints.len() < items.len() => writeln!(
                out,
                "(No summary yet. Its requirements, if you need them: `obligations list --node {source_id}`.)"
            )?,
            None => {}
        }
        if !constraints.is_empty() {
            out.push_str("\nConstraints:\n");
            for o in constraints {
                writeln!(out, "- {}", obligation_line(o))?;
            }
        }
    }
    Ok(out)
}

pub fn snapshot(conn: &Connection, scope: &ContextScope<'_>) -> Result<String> {
    let nodes = NodeRepo::new(conn);
    let repo = InterviewRepo::new(conn);
    let mut out = String::from("# Interview state\n\n");
    writeln!(out, "Data root: {}", scope.data_root.display())?;
    writeln!(out, "tod-cli: {}", scope.tod_cli.display())?;
    writeln!(
        out,
        "Node: {} \"{}\"",
        scope.node_id,
        node_title(&nodes, scope.node_id)
    )?;
    if let Some(lifecycle) = nodes.get_lifecycle(scope.node_id)? {
        writeln!(out, "Lifecycle: {lifecycle}")?;
    }
    writeln!(out, "Phase: {}", scope.phase)?;
    if let Some(session) = scope.interview_session_id {
        writeln!(out, "Session: {session}")?;
    }
    writeln!(out, "You are the {}.", scope.role.as_str().replace('-', " "))?;

    let chain = ancestor_chain(conn, scope.node_id)?;
    let purposes: Vec<(Uuid, String)> = chain
        .iter()
        .filter(|id| **id != scope.node_id)
        .filter_map(|id| {
            nodes
                .get_extra_content(*id, EXTRA_CONTENT_GOAL)
                .ok()
                .flatten()
                .filter(|p| !p.trim().is_empty())
                .map(|p| (*id, p))
        })
        .collect();
    if !purposes.is_empty() {
        out.push_str("\n## Purpose (root first)\n\n");
        for (id, purpose) in purposes {
            writeln!(out, "- {}: {}", node_title(&nodes, id), one_line(&purpose))?;
        }
    }

    out.push_str("\n## Obligations\n");
    let local: Vec<NodeObligation> = ObligationRepo::new(conn)
        .list_for_node(scope.node_id)?
        .into_iter()
        .filter(|o| phase_visible(&o.phase, scope.phase))
        .collect();
    if local.is_empty() {
        out.push_str("\n(none yet)\n");
    }
    for (kind, heading) in [(KIND_REQUIREMENT, "Requirements"), (KIND_CONSTRAINT, "Constraints")] {
        let items: Vec<&NodeObligation> = local.iter().filter(|o| o.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        writeln!(out, "\n### {heading}")?;
        let mut sections: Vec<Option<&str>> = Vec::new();
        for o in &items {
            if !sections.contains(&o.section.as_deref()) {
                sections.push(o.section.as_deref());
            }
        }
        sections.sort_by_key(|s| s.is_some());
        for section in sections {
            if let Some(section) = section {
                writeln!(out, "{section}:")?;
            }
            for o in items.iter().filter(|o| o.section.as_deref() == section) {
                writeln!(out, "- [{}] {}", short_id(o.id), one_line(&o.body))?;
            }
        }
    }

    out.push_str(&render_inherited_context(conn, &nodes, scope.node_id, Some(scope.phase))?);

    if scope.phase == PHASE_PLANNING {
        let plan_repo = PlanStepRepo::new(conn);
        let steps = plan_repo.list_for_node(scope.node_id)?;
        out.push_str("\n## Plan steps\n");
        if steps.is_empty() {
            out.push_str("\n(none yet)\n");
        }
        for step in &steps {
            let deps = plan_repo.list_dependencies(step.id)?;
            let obligations = plan_repo.list_obligations(step.id)?;
            writeln!(out, "- {}", plan_step_line(step, &deps, &obligations))?;
        }
    }

    let mut content = String::new();
    for ty in phase_content_types(scope.phase) {
        if let Some(body) = nodes
            .get_extra_content(scope.node_id, ty)?
            .filter(|b| !b.trim().is_empty())
        {
            writeln!(content, "\n### {ty}\n\n{}", body.trim())?;
        }
    }
    if !content.is_empty() {
        out.push_str("\n## Content\n");
        out.push_str(&content);
    }

    let memory: Vec<MemoryNote> = repo
        .list_memory(scope.node_id, None, Some(MEMORY_OPEN))?
        .into_iter()
        .filter(|m| memory_visible(scope, m))
        .collect();
    if !memory.is_empty() {
        out.push_str("\n## Memory\n\n");
        for note in &memory {
            writeln!(out, "- {}", memory_line(note))?;
        }
    }

    let questions = repo.list_questions(scope.node_id, &[])?;
    let mut section = String::new();
    for q in questions.iter().filter(|q| q.status == STATUS_OPEN) {
        writeln!(section, "{}", question_full(q, scope.role))?;
    }
    if !section.is_empty() {
        writeln!(out, "\n## Open questions\n\n{}", section.trim_end())?;
    }
    let deferred: Vec<_> = questions
        .iter()
        .filter(|q| q.status == STATUS_DEFERRED)
        .collect();
    if !deferred.is_empty() {
        out.push_str("\n## Deferred questions\n\n");
        for q in deferred {
            writeln!(out, "- {}: {}", q.label(), one_line(q.question.as_deref().unwrap_or("")))?;
        }
    }
    let answered: Vec<_> = questions
        .iter()
        .filter(|q| q.status == STATUS_ANSWERED && q.phase == scope.phase)
        .filter(|q| !(scope.role == Role::AnswerProcessor && q.processed_at.is_none()))
        .collect();
    if !answered.is_empty() {
        out.push_str("\n## Answered questions\n\n");
        let skip = answered.len().saturating_sub(scope.answered_cap);
        if skip > 0 {
            writeln!(out, "({skip} earlier answers omitted.)")?;
        }
        for q in answered.into_iter().skip(skip) {
            writeln!(out, "- {}", answered_line(q))?;
        }
    }
    let withdrawn: Vec<_> = questions
        .iter()
        .filter(|q| {
            q.status == STATUS_WITHDRAWN
                && q.phase == scope.phase
                // Agents' own withdrawals are not news to anyone.
                && matches!(q.withdrawn_by.as_deref(), Some(AUTHOR_USER) | None)
        })
        .collect();
    if !withdrawn.is_empty() {
        out.push_str("\n## Withdrawn by the user or the app\n\n");
        for q in withdrawn {
            writeln!(
                out,
                "- {} (by {}): {} — {}",
                q.label(),
                if q.withdrawn_by.is_some() { "the user" } else { "the app" },
                one_line(q.question.as_deref().unwrap_or("")),
                one_line(q.withdrawn_reason.as_deref().unwrap_or(""))
            )?;
        }
    }
    if scope.role == Role::AnswerProcessor {
        let waiting: Vec<_> = questions
            .iter()
            .filter(|q| q.status == STATUS_ANSWERED && q.processed_at.is_none())
            .collect();
        if !waiting.is_empty() {
            out.push_str("\n## Answers awaiting processing\n\n");
            for q in waiting {
                writeln!(out, "{}", question_full(q, scope.role))?;
                writeln!(out, "  {}", answer_detail(q))?;
            }
        }
    }
    Ok(out)
}

/// Everything that changed since `since` that `actor` did not do itself.
/// Empty when nothing did.
pub fn delta(conn: &Connection, scope: &ContextScope<'_>, since: i64, actor: &str) -> Result<String> {
    let repo = InterviewRepo::new(conn);
    let nodes = NodeRepo::new(conn);
    let obligations = ObligationRepo::new(conn);
    let chain = ancestor_chain(conn, scope.node_id)?;
    let changes = repo.changes_since(&chain, since, actor)?;

    // Group by entity, keeping first-seen order.
    let mut order: Vec<(String, Uuid)> = Vec::new();
    for change in &changes {
        let key = (change.entity.clone(), change.entity_id);
        if !order.contains(&key) {
            order.push(key);
        }
    }
    let ops_for = |entity: &str, id: Uuid| -> Vec<&ChangeRow> {
        changes
            .iter()
            .filter(|c| c.entity == entity && c.entity_id == id)
            .collect()
    };

    let mut obligation_lines = String::new();
    let mut content_lines = String::new();
    let mut question_lines = String::new();
    let mut memory_lines = String::new();
    let mut plan_step_lines = String::new();
    let plan_steps = PlanStepRepo::new(conn);

    for (entity, id) in order {
        let ops = ops_for(&entity, id);
        let inserted = ops.iter().any(|c| c.op == "insert");
        match entity.as_str() {
            ENTITY_OBLIGATION => match obligations.get(id)? {
                None if inserted => {}
                None => writeln!(obligation_lines, "- [{}]", short_id(id))?,
                Some(o) if !phase_visible(&o.phase, scope.phase) => {}
                // An ancestor's requirement obligations are spoken for by its
                // generated summary (see `render_inherited_context`) — surfacing
                // a live edit to one here would restate exactly what the
                // summary already exists to replace. Its constraints still
                // matter in full, and this node's own obligations always do.
                Some(o) if o.node_id != scope.node_id && o.kind != KIND_CONSTRAINT => {}
                Some(o) => {
                    let from = if o.node_id != scope.node_id {
                        format!(" (from \"{}\")", node_title(&nodes, o.node_id))
                    } else {
                        String::new()
                    };
                    let mark = if inserted { '+' } else { '~' };
                    writeln!(obligation_lines, "{mark} {}{from}", obligation_line(&o))?;
                }
            },
            ENTITY_CONTENT => {
                let row: Option<(Vec<u8>, String, String)> = conn
                    .query_row(
                        "SELECT node_id, content_type, body FROM node_extra_content WHERE id = ?1",
                        params![uuid_to_blob(id)],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                let Some((node_blob, ty, body)) = row else {
                    continue;
                };
                let node = tod_store::outline::blob_to_uuid(&node_blob)?;
                if node != scope.node_id {
                    if ty == EXTRA_CONTENT_GOAL {
                        writeln!(
                            content_lines,
                            "~ purpose of \"{}\": {}",
                            node_title(&nodes, node),
                            one_line(&body)
                        )?;
                    }
                    continue;
                }
                if !phase_content_types(scope.phase).contains(&ty.as_str()) {
                    continue;
                }
                let append_from = if inserted {
                    None
                } else {
                    ops.iter()
                        .map(|c| {
                            c.fields
                                .iter()
                                .find_map(|f| f.strip_prefix("append:"))
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .collect::<Option<Vec<_>>>()
                        .and_then(|offsets| offsets.into_iter().min())
                };
                match append_from {
                    Some(offset) if offset <= body.len() && body.is_char_boundary(offset) => {
                        writeln!(content_lines, "+ {ty} (appended):\n{}", indent(&body[offset..], "  "))?
                    }
                    _ => writeln!(
                        content_lines,
                        "{} {ty}:\n{}",
                        if inserted { '+' } else { '~' },
                        indent(&body, "  ")
                    )?,
                }
            }
            ENTITY_QUESTION => {
                let Some(q) = repo.get_question_by_id(id)? else {
                    continue;
                };
                if inserted {
                    if q.status == STATUS_OPEN {
                        writeln!(question_lines, "+ {}", question_full(&q, scope.role))?;
                    } else if q.status == STATUS_ANSWERED {
                        writeln!(question_lines, "+ {}", answered_line(&q))?;
                    }
                    continue;
                }
                let fields: Vec<&str> = ops
                    .iter()
                    .flat_map(|c| c.fields.iter().map(String::as_str))
                    .collect();
                if fields.contains(&"status") || fields.contains(&"answer") {
                    let line = match q.status.as_str() {
                        STATUS_ANSWERED => format!("{} answered: {}", q.label(), answer_detail(&q)),
                        STATUS_DEFERRED => format!("{} deferred", q.label()),
                        STATUS_WITHDRAWN => format!(
                            "{} withdrawn by {}: {}",
                            q.label(),
                            q.withdrawn_by.as_deref().unwrap_or("the app"),
                            one_line(q.withdrawn_reason.as_deref().unwrap_or(""))
                        ),
                        _ => format!("{} reopened", q.label()),
                    };
                    writeln!(question_lines, "{line}")?;
                }
                if fields.contains(&"processed") {
                    writeln!(
                        question_lines,
                        "{} processed: {}",
                        q.label(),
                        one_line(q.processed_summary.as_deref().unwrap_or(""))
                    )?;
                }
            }
            ENTITY_MEMORY => {
                let Some(note) = repo.get_memory_by_id(id)? else {
                    continue;
                };
                if !memory_visible(scope, &note) {
                    continue;
                }
                if inserted {
                    if note.status == MEMORY_OPEN {
                        writeln!(memory_lines, "+ {}", memory_line(&note))?;
                    }
                    continue;
                }
                let fields: Vec<&str> = ops
                    .iter()
                    .flat_map(|c| c.fields.iter().map(String::as_str))
                    .collect();
                if note.status == MEMORY_DONE {
                    writeln!(memory_lines, "{} done", note.label())?;
                } else if fields.contains(&"body") {
                    writeln!(memory_lines, "~ {}: {}", note.label(), one_line(&note.body))?;
                }
            }
            ENTITY_PLAN_STEP if scope.phase == PHASE_PLANNING => match plan_steps.get(id)? {
                None if inserted => {}
                None => writeln!(plan_step_lines, "- [{}] deleted", short_id(id))?,
                Some(step) => {
                    let deps = plan_steps.list_dependencies(step.id)?;
                    let obligations = plan_steps.list_obligations(step.id)?;
                    let mark = if inserted { '+' } else { '~' };
                    writeln!(
                        plan_step_lines,
                        "{mark} {}",
                        plan_step_line(&step, &deps, &obligations)
                    )?;
                }
            },
            ENTITY_PLAN_STEP_DEP | ENTITY_PLAN_STEP_OBLIGATION if scope.phase == PHASE_PLANNING => {
                let Some(step) = plan_steps.get(id)? else {
                    continue;
                };
                let deps = plan_steps.list_dependencies(step.id)?;
                let obligations = plan_steps.list_obligations(step.id)?;
                writeln!(
                    plan_step_lines,
                    "~ {}",
                    plan_step_line(&step, &deps, &obligations)
                )?;
            }
            _ => {}
        }
    }

    let mut out = String::new();
    for (heading, body) in [
        ("Obligations", obligation_lines),
        ("Content", content_lines),
        ("Plan steps", plan_step_lines),
        ("Questions", question_lines),
        ("Memory", memory_lines),
    ] {
        if !body.is_empty() {
            write!(out, "\n## {heading}\n\n{body}")?;
        }
    }
    if out.is_empty() {
        return Ok(out);
    }
    Ok(format!("# Changes{out}"))
}

fn memory_visible(scope: &ContextScope<'_>, note: &MemoryNote) -> bool {
    scope.role.sees_memory(&note.kind)
        && !(note.kind == MEMORY_PLAN && note.phase.as_deref() != Some(scope.phase))
}

fn memory_line(note: &MemoryNote) -> String {
    let mut tags = Vec::new();
    if let Some(phase) = note.phase.as_deref().filter(|_| note.kind == MEMORY_PARKED) {
        tags.push(phase.to_string());
    }
    if let Some(seq) = note.question_seq {
        tags.push(format!("from q-{seq}"));
    }
    let tags = if tags.is_empty() {
        String::new()
    } else {
        format!(" ({})", tags.join(", "))
    };
    if note.body.contains('\n') {
        format!("{} {}{tags}:\n{}", note.label(), note.kind, indent(&note.body, "  "))
    } else {
        format!("{} {}{tags}: {}", note.label(), note.kind, note.body)
    }
}

fn question_full(q: &InterviewQuestion, role: Role) -> String {
    let mut out = format!(
        "{} ({}): {}",
        q.label(),
        q.author,
        one_line(q.question.as_deref().unwrap_or("(freeform text from the user)"))
    );
    if let Some(context) = q.context.as_deref().filter(|c| !c.trim().is_empty()) {
        let _ = write!(out, "\n  context: {}", indent(context, "    ").trim_start());
    }
    if !q.options.is_empty() {
        let options: Vec<String> = q
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| format!("{}. {}", i + 1, one_line(o)))
            .collect();
        let _ = write!(out, "\n  options: {}", options.join(" | "));
    }
    if let Some(recommend) = q.recommend.as_deref() {
        let _ = write!(out, "\n  recommend: {}", one_line(recommend));
    }
    if let Some(p) = &q.proposal {
        let _ = write!(out, "\n  proposal: {}", proposal_line(p));
    }
    if let Some(intent) = q.intent.as_deref() {
        if role == Role::AnswerProcessor || q.author == role.as_str() {
            let _ = write!(out, "\n  intent: {}", one_line(intent));
        }
    }
    if !q.covers.is_empty() {
        let _ = write!(out, "\n  covers: {}", q.covers.join(", "));
    }
    out
}

fn proposal_line(p: &Proposal) -> String {
    let id = |raw: &str| {
        Uuid::parse_str(raw)
            .map(short_id)
            .unwrap_or_else(|_| raw.to_string())
    };
    let text = p.text.as_deref().map(one_line).unwrap_or_default();
    let mut line = match p.op {
        ProposalOp::Add => format!(
            "add {}{}: \"{text}\"",
            p.kind.as_deref().unwrap_or(""),
            p.section
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default()
        ),
        ProposalOp::Update => format!("update [{}]: \"{text}\"", id(p.id.as_deref().unwrap_or(""))),
        ProposalOp::Delete => format!("delete [{}]", id(p.id.as_deref().unwrap_or(""))),
        ProposalOp::Content => format!(
            "{} {}: \"{text}\"",
            if p.append { "append to" } else { "set" },
            p.content_type.as_deref().unwrap_or("")
        ),
    };
    if !p.replaces.is_empty() {
        let replaced: Vec<String> = p.replaces.iter().map(|r| id(r)).collect();
        let _ = write!(line, " replacing [{}]", replaced.join(", "));
    }
    line
}

fn answered_line(q: &InterviewQuestion) -> String {
    match q.question.as_deref() {
        Some(question) => format!("{}: {} → {}", q.label(), one_line(question), answer_detail(q)),
        None => format!(
            "{} freeform: {}",
            q.label(),
            one_line(q.answer_text.as_deref().unwrap_or(""))
        ),
    }
}

fn answer_detail(q: &InterviewQuestion) -> String {
    let mut parts = Vec::new();
    if let Some(option) = q.answer_option {
        let label = q
            .options
            .get((option - 1).max(0) as usize)
            .map(|l| format!(" {}", one_line(l)))
            .unwrap_or_default();
        parts.push(format!("{option}{label}"));
    }
    if let Some(edited) = q.answer_edited_text.as_deref() {
        parts.push(format!("edited text: \"{}\"", one_line(edited)));
    }
    if let Some(text) = q.answer_text.as_deref() {
        if q.question.is_some() {
            parts.push(format!("notes: \"{}\"", one_line(text)));
        } else {
            parts.push(format!("\"{}\"", one_line(text)));
        }
    }
    if let Some(applied) = &q.applied {
        if let Some(error) = applied.get("error").and_then(|e| e.as_str()) {
            parts.push(format!("apply failed: {error}"));
        } else if let Some(ops) = applied.get("ops").and_then(|o| o.as_array()) {
            let ops: Vec<String> = ops
                .iter()
                .map(|op| {
                    let kind = op.get("op").and_then(|v| v.as_str()).unwrap_or("");
                    let target = op
                        .get("id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .map(short_id)
                        .or_else(|| op.get("type").and_then(|v| v.as_str()).map(str::to_string))
                        .unwrap_or_default();
                    match kind {
                        "add" => format!("+{target}"),
                        "update" => format!("~{target}"),
                        "delete" => format!("-{target}"),
                        _ => format!("{kind} {target}"),
                    }
                })
                .collect();
            parts.push(format!("applied {}", ops.join(" ")));
        }
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use crate::interview::test_support::{draft, fixture};
    use tod_store::interview::*;
    use tod_store::outline::OutlineMutation;

    const AGENT: &str = "agent-session";

    #[test]
    fn snapshot_shows_each_role_what_it_needs() {
        let fx = fixture();
        let obligation = fx.obligation("Sessions persist.");
        for (kind, body) in [(MEMORY_PLAN, "The plan so far."), (MEMORY_CONTEXT, "Users are admins.")] {
            fx.user(InterviewCommand::AddMemory {
                node_id: fx.node,
                kind: kind.into(),
                phase: Some(PHASE_REQUIREMENTS.into()),
                body: body.into(),
                question_seq: None,
            });
        }
        let mut question = draft("Should sessions survive restarts?");
        question.intent = Some("Option 2 means no.".into());
        question.proposal = Some(Proposal {
            op: ProposalOp::Update,
            kind: None,
            section: None,
            node: None,
            id: Some(short_id(obligation)),
            content_type: None,
            text: Some("Sessions persist across restarts.".into()),
            append: false,
            replaces: Vec::new(),
        });
        fx.question(ACTOR_USER, question);

        let qm = fx.snapshot(Role::QuestionMaker, PHASE_REQUIREMENTS);
        assert!(qm.starts_with("# Interview state"), "{qm}");
        assert!(qm.contains(&format!("- [{}] Sessions persist.", short_id(obligation))), "{qm}");
        assert!(qm.contains(&format!("proposal: update [{}]", short_id(obligation))), "{qm}");
        assert!(qm.contains("Users are admins."), "{qm}");
        assert!(qm.contains("The plan so far."), "{qm}");
        // The question maker sees intent only on questions it wrote.
        assert!(!qm.contains("Option 2 means no."), "{qm}");

        let ap = fx.snapshot(Role::AnswerProcessor, PHASE_REQUIREMENTS);
        assert!(ap.contains("intent: Option 2 means no."), "{ap}");
        assert!(ap.contains("Users are admins."), "{ap}");
        assert!(!ap.contains("The plan so far."), "{ap}");
    }

    #[test]
    fn delta_carries_only_what_others_changed_and_each_thing_once() {
        let fx = fixture();
        let base = fx.head();
        fx.question(AGENT, draft("Own question?"));
        fx.question(ACTOR_USER, draft("Someone else's question?"));

        let changes = fx.delta(Role::QuestionMaker, PHASE_REQUIREMENTS, base, AGENT);
        assert!(changes.starts_with("# Changes"), "{changes}");
        assert!(changes.contains("+ q-2 (user): Someone else's question?"), "{changes}");
        assert!(!changes.contains("Own question?"), "{changes}");

        let synced = fx.head();
        assert_eq!(fx.delta(Role::QuestionMaker, PHASE_REQUIREMENTS, synced, AGENT), "");

        fx.answer(1, None, Some("Because reasons"));
        let changes = fx.delta(Role::QuestionMaker, PHASE_REQUIREMENTS, synced, AGENT);
        assert!(changes.contains("q-1 answered: notes: \"Because reasons\""), "{changes}");
        // Already known: the question text is not sent again.
        assert!(!changes.contains("Own question?"), "{changes}");
        assert!(!changes.contains("Someone else's question?"), "{changes}");
    }

    #[test]
    fn delta_marks_removed_obligations_and_skips_short_lived_ones() {
        let fx = fixture();
        let removed = fx.obligation("Was here before.");
        let base = fx.head();
        fx.outline(OutlineMutation::DeleteObligation {
            obligation_id: removed,
        });
        let short_lived = fx.obligation("Came and went.");
        fx.outline(OutlineMutation::DeleteObligation {
            obligation_id: short_lived,
        });
        let added = fx.obligation("Newly required.");

        let changes = fx.delta(Role::AnswerProcessor, PHASE_REQUIREMENTS, base, AGENT);
        assert!(changes.contains(&format!("- [{}]", short_id(removed))), "{changes}");
        assert!(!changes.contains(&short_id(short_lived)), "{changes}");
        assert!(
            changes.contains(&format!("+ [{}] requirement: Newly required.", short_id(added))),
            "{changes}"
        );
    }

    #[test]
    fn appended_content_arrives_as_only_the_new_text() {
        let fx = fixture();
        let set = |body: &str| {
            fx.outline(OutlineMutation::SetExtraContent {
                node_id: fx.node,
                content_type: "goal".into(),
                body: body.into(),
            })
        };
        set("First decision.");
        let base = fx.head();
        set("First decision.\n\nSecond decision.");

        let changes = fx.delta(Role::AnswerProcessor, PHASE_REQUIREMENTS, base, AGENT);
        assert!(changes.contains("+ goal (appended):"), "{changes}");
        assert!(changes.contains("Second decision."), "{changes}");
        assert!(!changes.contains("First decision."), "{changes}");
    }

    #[test]
    fn planning_snapshot_carries_both_requirements_and_design_obligations() {
        let fx = fixture();
        let req = fx.obligation_with_phase("Must support offline mode.", PHASE_REQUIREMENTS);
        let design = fx.obligation_with_phase("Use SQLite for local cache.", PHASE_DESIGN);

        let planning = fx.snapshot(Role::AnswerProcessor, PHASE_PLANNING);
        assert!(planning.contains(&short_id(req)), "{planning}");
        assert!(planning.contains(&short_id(design)), "{planning}");
        assert!(planning.contains("Must support offline mode."), "{planning}");
        assert!(planning.contains("Use SQLite for local cache."), "{planning}");
    }

    #[test]
    fn design_phase_obligations_reach_design_and_planning_not_requirements() {
        let fx = fixture();
        let base = fx.head();
        let decision = fx.obligation_with_phase("Use Postgres for storage.", PHASE_DESIGN);

        let design_changes = fx.delta(Role::AnswerProcessor, PHASE_DESIGN, base, AGENT);
        assert!(design_changes.contains("Use Postgres for storage."), "{design_changes}");

        let planning_changes = fx.delta(Role::AnswerProcessor, PHASE_PLANNING, base, AGENT);
        assert!(planning_changes.contains("Use Postgres for storage."), "{planning_changes}");

        // A design-phase obligation is not visible back in the requirements phase.
        let requirements_changes = fx.delta(Role::AnswerProcessor, PHASE_REQUIREMENTS, base, AGENT);
        assert!(!requirements_changes.contains("Use Postgres for storage."), "{requirements_changes}");
        let _ = decision;
    }

    #[test]
    fn handoffs_reach_only_the_question_maker() {
        let fx = fixture();
        let base = fx.head();
        fx.user(InterviewCommand::AddMemory {
            node_id: fx.node,
            kind: MEMORY_HANDOFF.into(),
            phase: None,
            body: "Ask about exports.".into(),
            question_seq: None,
        });
        let qm = fx.delta(Role::QuestionMaker, PHASE_REQUIREMENTS, base, AGENT);
        assert!(qm.contains("+ m-1 handoff: Ask about exports."), "{qm}");
        assert_eq!(fx.delta(Role::AnswerProcessor, PHASE_REQUIREMENTS, base, AGENT), "");

        let synced = fx.head();
        fx.user(InterviewCommand::UpdateMemory {
            node_id: fx.node,
            seq: 1,
            body: None,
            status: Some(MEMORY_DONE.into()),
        });
        let qm = fx.delta(Role::QuestionMaker, PHASE_REQUIREMENTS, synced, AGENT);
        assert!(qm.contains("m-1 done"), "{qm}");
    }

    #[test]
    fn withdrawals_by_the_user_and_the_app_say_who_withdrew_them() {
        let fx = fixture();
        let target = fx.obligation("Old wording.");
        let mut stale = draft("Reword it?");
        stale.proposal = Some(Proposal {
            op: ProposalOp::Update,
            kind: None,
            section: None,
            node: None,
            id: Some(target.to_string()),
            content_type: None,
            text: Some("New wording.".into()),
            append: false,
            replaces: Vec::new(),
        });
        fx.question(ACTOR_USER, stale);
        fx.question(ACTOR_USER, draft("Useful at all?"));
        let base = fx.head();

        fx.outline(OutlineMutation::DeleteObligation {
            obligation_id: target,
        });
        fx.user(InterviewCommand::WithdrawStaleProposals { node_id: fx.node });
        fx.user(InterviewCommand::WithdrawQuestion {
            node_id: fx.node,
            seq: 2,
            reason: "Not useful.".into(),
        });

        let changes = fx.delta(Role::QuestionMaker, PHASE_REQUIREMENTS, base, AGENT);
        assert!(
            changes.contains(&format!("q-1 withdrawn by the app: {STALE_PROPOSAL_REASON}")),
            "{changes}"
        );
        assert!(changes.contains("q-2 withdrawn by user: Not useful."), "{changes}");

        let snapshot = fx.snapshot(Role::QuestionMaker, PHASE_REQUIREMENTS);
        assert!(
            snapshot.contains(&format!("- q-1 (by the app): Reword it? — {STALE_PROPOSAL_REASON}")),
            "{snapshot}"
        );
        assert!(
            snapshot.contains("- q-2 (by the user): Useful at all? — Not useful."),
            "{snapshot}"
        );
    }
}
