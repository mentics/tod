//! `tod-cli lessons` — the improvement lessons the app records about its own
//! process (`tod_store::lessons`, `doc/lifecycle/self-improvement.md`): list,
//! show, the measures of the loop, the user's own lessons, declining one, and
//! the process review marking the ones its commits address.

use crate::Invocation;
use crate::args::Args;
use tod_store::interview::InterviewCommand;
use tod_store::lessons::{self, Lesson, Op};
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli lessons — what the app learned about its own process

COMMANDS:
    list     [--status <STATUS>] [--source <SOURCE>]
    show     <LESSON>
    stats    [--days <N>]
    add      --node <NODE> --text <TEXT> [--category <CATEGORY>] [--target <TEXT>]
    decline  <LESSON>
    apply    [<LESSON>] [--lesson <LESSON> (repeatable)] --ref <TEXT>

Lessons are recorded by the app (a request that should not have been asked, a
send-back or dispute, a quick fix that escalated, a broken tool) and by the
user. `list` shows them, `stats` the measures of the improvement loop. `add`
records a lesson in the user's words about a node; category is one of
asker_could_decide, app_should_handle, upstream_unclear, brief_unclear,
necessary, upstream_wrong. `decline` is the user's. `apply` is for the
process review's session: it marks the lessons its commits address, by the
commit in --ref.
";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn short(id: Uuid) -> String {
    id.simple().to_string()[..8].to_string()
}

fn one_line(text: &str, chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= chars {
        return flat;
    }
    let mut out: String = flat.chars().take(chars).collect();
    out.push_str(" ...");
    out
}

fn slug_of(inv: &Invocation, node: Uuid) -> anyhow::Result<String> {
    Ok(inv.client().read(move |conn| NodeRepo::new(conn).get(node))?.map(|n| n.slug).unwrap_or_else(|| node.simple().to_string()))
}

fn render_row(inv: &Invocation, lesson: &Lesson) -> anyhow::Result<String> {
    Ok(format!(
        "{}  {:<8} {:<9} {:<18} {}  {}",
        short(lesson.id),
        lesson.status,
        lesson.source,
        lesson.category,
        slug_of(inv, lesson.node_id)?,
        one_line(&lesson.summary, 140)
    ))
}

fn render_full(inv: &Invocation, lesson: &Lesson) -> anyhow::Result<String> {
    let mut out = format!(
        "lesson {}\n  status:   {}{}\n  source:   {}\n  category: {}\n  node:     {}\n  target:   {}\n  recorded: {}\n",
        lesson.id,
        lesson.status,
        if lesson.applied_ref.is_empty() { String::new() } else { format!(" (by {})", lesson.applied_ref) },
        lesson.source,
        lesson.category,
        slug_of(inv, lesson.node_id)?,
        if lesson.target.is_empty() { "(none)" } else { &lesson.target },
        chrono::DateTime::from_timestamp_millis(lesson.created_at).map(|t| t.to_rfc3339()).unwrap_or_default(),
    );
    if let Some(run) = lesson.run_node_id {
        out.push_str(&format!("  review:   grouped into {}\n", slug_of(inv, run)?));
    }
    for (label, id) in [
        ("request", lesson.decision_id),
        ("session", lesson.conversation_id),
        ("send-back", lesson.send_back_id),
        ("incident", lesson.incident_id),
    ] {
        if let Some(id) = id {
            out.push_str(&format!("  {label}: {id}\n"));
        }
    }
    out.push_str(&format!("\n{}\n", lesson.summary.trim()));
    if !lesson.user_quote.trim().is_empty() {
        out.push_str(&format!("\nThe user said: \"{}\"\n", lesson.user_quote.trim()));
    }
    Ok(out)
}

fn one(inv: &Invocation, raw: &str) -> anyhow::Result<Lesson> {
    let id = inv.client().read(|conn| lessons::resolve(conn, raw))?;
    inv.client()
        .read(move |conn| lessons::get(conn, id))?
        .ok_or_else(|| anyhow::anyhow!("lesson {id} not found"))
}

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "list" => {
            let status = args.get("--status").map(str::to_string);
            let source = args.get("--source").map(str::to_string);
            if let Some(s) = &status {
                anyhow::ensure!(lessons::STATUSES.contains(&s.as_str()), "--status is one of {}", lessons::STATUSES.join(", "));
            }
            if let Some(s) = &source {
                anyhow::ensure!(lessons::SOURCES.contains(&s.as_str()), "--source is one of {}", lessons::SOURCES.join(", "));
            }
            let found = inv.client().read(move |conn| lessons::list(conn, status.as_deref(), source.as_deref()))?;
            if inv.json {
                return Ok(serde_json::to_string(&found)?);
            }
            if found.is_empty() {
                return Ok("no lessons".to_string());
            }
            let rows: anyhow::Result<Vec<String>> = found.iter().map(|l| render_row(&inv, l)).collect();
            Ok(rows?.join("\n"))
        }
        "show" => {
            let lesson = one(&inv, args.target("a lesson id")?)?;
            if inv.json {
                return Ok(serde_json::to_string(&lesson)?);
            }
            Ok(render_full(&inv, &lesson)?.trim_end().to_string())
        }
        "stats" => {
            let since = args
                .get("--days")
                .map(|n| {
                    n.trim()
                        .parse::<u32>()
                        .map_err(|_| anyhow::anyhow!("--days must be a whole number of days (got `{n}`)"))
                })
                .transpose()?
                .map(|days| chrono::Utc::now().timestamp_millis() - i64::from(days) * DAY_MS);
            let stats = inv.client().read(move |conn| tod_store::lesson_stats::compute(conn, since))?;
            if inv.json {
                return Ok(serde_json::to_string(&stats)?);
            }
            Ok(stats.render().trim_end().to_string())
        }
        "add" => {
            let node = crate::node::resolve(&inv, args.require("--node")?)?;
            let op = Op::Add {
                node,
                text: args.require("--text")?.to_string(),
                category: args.get("--category").map(str::to_string),
                target: args.get("--target").unwrap_or_default().to_string(),
            };
            if let Op::Add { category: Some(c), .. } = &op {
                anyhow::ensure!(lessons::CATEGORIES.contains(&c.as_str()), "--category is one of {}", lessons::CATEGORIES.join(", "));
            }
            let result = inv.client().interview(InterviewCommand::Lessons(op))?;
            if inv.json {
                return Ok(serde_json::to_string(&result)?);
            }
            let id: Uuid = serde_json::from_value(result["id"].clone())?;
            Ok(format!("ok lesson {}", short(id)))
        }
        "decline" => {
            let lesson = one(&inv, args.target("a lesson id")?)?;
            let result = inv.client().interview(InterviewCommand::Lessons(Op::Decline { id: lesson.id }))?;
            if inv.json {
                return Ok(serde_json::to_string(&result)?);
            }
            Ok(format!("ok declined {}", short(lesson.id)))
        }
        "apply" => {
            let mut ids = Vec::new();
            for raw in args.target("a lesson id").ok().into_iter().chain(args.get_all("--lesson")) {
                ids.push(one(&inv, raw)?.id);
            }
            let result = inv.client().interview(InterviewCommand::Lessons(Op::Apply {
                ids,
                applied_ref: args.require("--ref")?.to_string(),
            }))?;
            if inv.json {
                return Ok(serde_json::to_string(&result)?);
            }
            Ok(format!("ok applied {}", result["applied"]))
        }
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}
