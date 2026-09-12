//! `tod-cli visual-design` — save and list UI mockup packages for a node.
//!
//! A "package" is a single self-contained HTML+CSS file (no external JS or
//! network) written under the data root and linked from a design-phase
//! obligation body, so the existing gate-check machinery (which reads
//! obligation bodies, not a dedicated table) can judge whether visual design
//! is done. See `assets/process/agents/tools/visual-design.md`.

use crate::Invocation;
use crate::args::Args;
use tod_store::interview::{InterviewCommand, PHASE_DESIGN, short_id};
use tod_store::outline::repos::ObligationRepo;
use tod_store::outline::slug::slugify;
use tod_store::outline::{KIND_REQUIREMENT, OutlineMutation};
use uuid::Uuid;

const USAGE: &str = "\
tod-cli visual-design — UI mockup packages for a node

COMMANDS:
    save --node <UUID> --title <TEXT> --html-file <PATH> [--section <NAME>]
    list --node <UUID>

`save` writes the HTML file under the data root and creates a design-phase
obligation whose body links to it, so the design-planning gate check can find
it. The HTML must be self-contained: no <script> tags, no external network
resources.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "save" => save(&inv, &args),
        "list" => list(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn save(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.node()?;
    let title = args.require("--title")?.to_string();
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
    // global-override machinery.
    let dir = inv.data_root.join("visual-design").join(node.to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyhow::anyhow!("failed to create {}: {err}", dir.display()))?;

    let mut slug = slugify(&title);
    if slug.is_empty() {
        slug = Uuid::new_v4().to_string();
    }
    let mut dest = dir.join(format!("{slug}.html"));
    if dest.exists() {
        dest = dir.join(format!("{slug}-{}.html", &Uuid::new_v4().to_string()[..8]));
    }
    std::fs::write(&dest, &html)
        .map_err(|err| anyhow::anyhow!("failed to write {}: {err}", dest.display()))?;
    let dest = tod_store::path_util::canonicalize_if_possible(&dest);

    let body = format!(
        "Visual design package: **{title}**\n\n[{title}]({})",
        dest.display()
    );
    let obligation_id = Uuid::new_v4();
    inv.client().interview(InterviewCommand::Outline {
        mutation: OutlineMutation::CreateObligation {
            obligation_id: Some(obligation_id),
            node_id: node,
            kind: KIND_REQUIREMENT.to_string(),
            after_id: None,
            before: false,
            section: args
                .get("--section")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            body,
            phase: PHASE_DESIGN.to_string(),
        },
        target: None,
    })?;

    if inv.json {
        Ok(serde_json::json!({
            "obligation_id": obligation_id.to_string(),
            "path": dest.display().to_string(),
        })
        .to_string())
    } else {
        Ok(format!(
            "ok {} -> {}",
            short_id(obligation_id),
            dest.display()
        ))
    }
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = args.node()?;
    let rows = inv
        .client()
        .read(|conn| ObligationRepo::new(conn).list_for_node(node))?;
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|o| o.phase == PHASE_DESIGN && o.body.contains("Visual design package:"))
        .collect();
    if inv.json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|o| {
                serde_json::json!({
                    "id": o.id.to_string(),
                    "body": o.body,
                })
            })
            .collect();
        return Ok(serde_json::Value::Array(items).to_string());
    }
    if rows.is_empty() {
        return Ok("(none)".to_string());
    }
    Ok(rows
        .iter()
        .map(|o| format!("[{}] {}", short_id(o.id), o.body.replace('\n', " ")))
        .collect::<Vec<_>>()
        .join("\n"))
}
