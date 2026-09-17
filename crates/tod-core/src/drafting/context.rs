//! What a drafter is told when its session starts. Later turns reuse the
//! interview's change delta (`interview::context::delta`) plus the turn's
//! dumps and choices.

use crate::drafting::DraftingMode;
use crate::interview::context::ContextScope;
use crate::node_context::{
    one_line, render_inherited_context, write_obligations_by_kind, write_snapshot_header,
};
use anyhow::Result;
use rusqlite::Connection;
use std::fmt::Write as _;
use tod_store::drafting::*;
use tod_store::interview::short_id;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{EXTRA_CONTENT_DETAILS, phase_visible};

/// One obligation as the drafter sees it: id, kind, provenance, and (for
/// `agent` ones) attention with its reason. Its section is the heading it is
/// listed under (see `write_obligations_by_kind`).
pub fn marked_line(m: &MarkedObligation) -> String {
    let o = &m.obligation;
    let mark = if m.mark.is_agent() {
        match (&m.mark.attention, &m.mark.attention_why) {
            (Some(level), Some(why)) => format!("agent, {level}: {}", one_line(why)),
            (Some(level), None) => format!("agent, {level}"),
            _ => "agent".to_string(),
        }
    } else {
        "user".to_string()
    };
    let visual = if o.visual_design_path.is_some() {
        " [mockup attached]"
    } else {
        ""
    };
    format!(
        "[{}] {} <{mark}>: {}{visual}",
        short_id(o.id),
        o.kind,
        one_line(&o.body)
    )
}

pub fn choice_line(c: &DraftingChoice) -> String {
    let options: Vec<String> = c
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}. {}", i + 1, one_line(&o.label)))
        .collect();
    format!(
        "{}: {} — {}",
        c.label(),
        one_line(&c.question),
        options.join(" | ")
    )
}

pub fn snapshot(conn: &Connection, scope: &ContextScope<'_>, mode: DraftingMode) -> Result<String> {
    let nodes = NodeRepo::new(conn);
    let drafting = DraftingRepo::new(conn);
    let mut out = String::from("# Drafting state\n\n");
    write_snapshot_header(
        &mut out,
        &nodes,
        scope.data_root,
        scope.tod_cli,
        scope.node_id,
    )?;
    writeln!(
        out,
        "Mode: {}",
        match mode {
            DraftingMode::Capture => "capture",
            DraftingMode::Drafting => "drafting",
        }
    )?;
    writeln!(out, "Phase: {}", scope.phase)?;

    out.push_str("\n## Details\n\n");
    match nodes
        .get_extra_content(scope.node_id, EXTRA_CONTENT_DETAILS)?
        .filter(|d| !d.trim().is_empty())
    {
        Some(details) => writeln!(out, "{}", details.trim())?,
        None => out.push_str("(none yet)\n"),
    }

    let local: Vec<MarkedObligation> = drafting
        .marked_obligations(scope.node_id)?
        .into_iter()
        .filter(|m| phase_visible(&m.obligation.phase, scope.phase))
        .collect();
    write_obligations_by_kind(&mut out, &local, |m| &m.obligation, marked_line)?;
    let pre_v3 = local.iter().filter(|m| m.mark.is_pre_v3()).count();
    if pre_v3 > 0 {
        writeln!(
            out,
            "\n{pre_v3} of these were written before drafting v3 (reason \"{PRE_V3_ATTENTION_WHY}\")."
        )?;
    }

    out.push_str(&render_inherited_context(
        conn,
        &nodes,
        scope.node_id,
        Some(scope.phase),
    )?);

    let open = drafting.list_choices(scope.node_id, &[CHOICE_OPEN])?;
    if !open.is_empty() {
        out.push_str("\n## Open choices\n\n");
        for c in &open {
            writeln!(out, "- {}", choice_line(c))?;
        }
    }

    if mode == DraftingMode::Drafting {
        out.push_str("\n## Buildable\n\n");
        match drafting.buildable(scope.node_id)? {
            Some(eval) => writeln!(
                out,
                "{}{}",
                eval.outcome,
                eval.detail
                    .as_deref()
                    .map(|d| format!(" — {}", one_line(d)))
                    .unwrap_or_default()
            )?,
            None => out.push_str("not recorded yet\n"),
        }
    }
    Ok(out)
}
