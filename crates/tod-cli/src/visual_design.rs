//! `tod-cli visual-design` — save and view the UI mockup associated with one
//! obligation.
//!
//! Each obligation has at most one visual-design mockup: a single
//! self-contained HTML+CSS file (no external JS or network) written under
//! the data root and linked from `node_obligations.visual_design_path`.
//! Saving again for the same obligation overwrites the previous mockup file
//! and its link — there is deliberately no way to attach more than one, so a
//! node that needs several mockups gets several design-phase obligations,
//! each explaining what its own mockup covers. See
//! `assets/process/agents/tools/visual-design.md`.

use crate::Invocation;
use crate::args::Args;
use tod_store::interview::{InterviewRepo, short_id};
use tod_store::outline::NodeObligation;
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::ObligationRepo;

pub(crate) const USAGE: &str = "\
tod-cli visual-design — the UI mockup associated with one obligation

COMMANDS:
    save --obligation <UUID> --html-file <PATH>
    show --obligation <UUID>
    clear --obligation <UUID>

`save` writes the HTML file under the data root and links it from the given
obligation, replacing any mockup already linked there. The HTML must be
self-contained: no <script> tags, no external network resources.
";

fn require_obligation(inv: &Invocation, args: &Args) -> anyhow::Result<NodeObligation> {
    let raw = args.require("--obligation")?.to_string();
    let obligation = inv.client().read(move |conn| {
        let id = InterviewRepo::new(conn).resolve_obligation_id(&raw)?;
        ObligationRepo::new(conn)
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("obligation {id} not found"))
    })?;
    Ok(obligation)
}

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "save" => save(&inv, &args),
        "show" => show(&inv, &args),
        "clear" => clear(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn save(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let obligation = require_obligation(inv, args)?;
    let html_path = args.require("--html-file")?;
    let html_path = std::path::Path::new(html_path);
    if !html_path.is_file() {
        anyhow::bail!("--html-file {} does not exist", html_path.display());
    }
    let html = std::fs::read_to_string(html_path)
        .map_err(|err| anyhow::anyhow!("failed to read {}: {err}", html_path.display()))?;
    if html.to_ascii_lowercase().contains("<script") {
        anyhow::bail!(
            "visual design mockups must not contain <script> tags (self-contained HTML+CSS only)"
        );
    }

    // Mirrors `TodPaths::visual_design_dir` (tod-store), built directly from
    // `inv.data_root` since tod-cli doesn't go through the `TodPaths`
    // global-override machinery. One file per obligation, named by its id, so
    // saving again for the same obligation deterministically overwrites it.
    let dir = inv
        .data_root
        .join("visual-design")
        .join(obligation.node_id.to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyhow::anyhow!("failed to create {}: {err}", dir.display()))?;
    let dest = dir.join(format!("{}.html", obligation.id));
    std::fs::write(&dest, &html)
        .map_err(|err| anyhow::anyhow!("failed to write {}: {err}", dest.display()))?;
    let dest = tod_store::path_util::canonicalize_if_possible(&dest);
    let dest_str = dest.display().to_string();

    inv.client().interview(tod_store::interview::InterviewCommand::Outline {
        mutation: OutlineMutation::UpdateObligationVisualDesign {
            obligation_id: obligation.id,
            path: Some(dest_str.clone()),
        },
        target: None,
    })?;

    if inv.json {
        Ok(serde_json::json!({
            "obligation_id": obligation.id.to_string(),
            "path": dest_str,
        })
        .to_string())
    } else {
        Ok(format!("ok {} -> {}", short_id(obligation.id), dest_str))
    }
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let obligation = require_obligation(inv, args)?;
    match &obligation.visual_design_path {
        Some(path) if inv.json => Ok(serde_json::json!({
            "obligation_id": obligation.id.to_string(),
            "path": path,
        })
        .to_string()),
        Some(path) => Ok(path.clone()),
        None if inv.json => Ok(serde_json::json!({
            "obligation_id": obligation.id.to_string(),
            "path": null,
        })
        .to_string()),
        None => Ok("(none)".to_string()),
    }
}

fn clear(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let obligation = require_obligation(inv, args)?;
    inv.client().interview(tod_store::interview::InterviewCommand::Outline {
        mutation: OutlineMutation::UpdateObligationVisualDesign {
            obligation_id: obligation.id,
            path: None,
        },
        target: None,
    })?;
    Ok(format!("ok {} (cleared)", short_id(obligation.id)))
}
