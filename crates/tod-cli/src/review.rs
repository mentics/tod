//! `tod-cli review` — code review findings on a node: what a review
//! conversation's agent found, and the response each one gets.
//!
//! Inside a review conversation the app sets `TOD_IMPLEMENT_NODE` and
//! `TOD_IMPLEMENT_CONVERSATION`, so `--node` defaults to the node under
//! review and each finding is filed under the conversation. `done` only
//! works there: it is how the app learns the review is finished.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_core::conversation::review::done_report;
use tod_store::conversation::ConversationRepo;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::review::{
    FINDING_OPEN, FINDING_STATUSES, NewFinding, ReviewFinding, ReviewRepo, normalize,
};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli review — code review findings on a node

Finding ids may be given in full or as the 8-character prefix shown in listings.
Inside a review conversation --node defaults to the node under review.

COMMANDS:
    list      [--node <UUID>] [--open]
    show      <ID>
    add       [--node <UUID>] --severity high|medium|low --summary <TEXT> [--file <PATH>] [--line <N>] [--detail <TEXT>]
    respond   <ID> --status open|fixed|out_of_scope|declined [--response <TEXT>]
    done

`add` records an open finding: --summary is the defect in a sentence, --detail
the inputs or state that go wrong and how (use `--detail -` and a heredoc for
anything long), --file a repo-relative path and --line its 1-based line.
`respond` answers one: fixed (the response points at the change), out_of_scope,
or declined (not critical, beyond the requirements, or not worth the cost).
Every status but open needs --response; open clears it.
`done` records that this review is finished; it only works inside a review
conversation (TOD_IMPLEMENT_CONVERSATION=<UUID>).
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "list" => list(&inv, &args),
        "show" => show(&inv, &args),
        "add" => add(&inv, &args),
        "respond" => respond(&inv, &args),
        "done" => done(&inv),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// `--node`, else the node a review conversation is running on.
fn node(args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = args.uuid("--node")? {
        return Ok(node);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required outside a review conversation"))
}

fn env_uuid(name: &str) -> anyhow::Result<Option<Uuid>> {
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => Uuid::parse_str(raw.trim())
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{name} is not a UUID (`{raw}`)")),
        _ => Ok(None),
    }
}

fn resolve(inv: &Invocation, raw: &str) -> anyhow::Result<Uuid> {
    inv.client().read(|conn| ReviewRepo::new(conn).resolve(raw))
}

fn finding_json(f: &ReviewFinding) -> serde_json::Value {
    serde_json::json!({
        "id": f.id.to_string(),
        "node_id": f.node_id.to_string(),
        "seq": f.seq,
        "severity": f.severity,
        "file": f.file,
        "line": f.line,
        "summary": f.summary,
        "detail": f.detail,
        "status": f.status,
        "response": f.response,
    })
}

fn finding_line(f: &ReviewFinding) -> String {
    let location = f.location().map(|l| format!(" {l}")).unwrap_or_default();
    let response = match &f.response {
        Some(response) => format!("\n    response: {response}"),
        None => String::new(),
    };
    format!(
        "[{}] {} {}{location}: {}{response}",
        short_id(f.id),
        f.status,
        f.severity,
        f.summary
    )
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let open_only = args.has("--open");
    let findings: Vec<ReviewFinding> = inv
        .client()
        .read(|conn| ReviewRepo::new(conn).list_for_node(node))?
        .into_iter()
        .filter(|f| !open_only || f.is_open())
        .collect();
    if inv.json {
        return Ok(serde_json::to_string(
            &findings.iter().map(finding_json).collect::<Vec<_>>(),
        )?);
    }
    if findings.is_empty() {
        return Ok(if open_only {
            "(no open findings)".to_string()
        } else {
            "(no findings)".to_string()
        });
    }
    Ok(findings
        .iter()
        .map(finding_line)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a finding id")?)?;
    let finding = inv
        .client()
        .read(|conn| ReviewRepo::new(conn).get(id))?
        .ok_or_else(|| anyhow::anyhow!("finding {id} not found"))?;
    if inv.json {
        return Ok(serde_json::to_string(&finding_json(&finding))?);
    }
    let mut out = finding_line(&finding);
    if let Some(detail) = &finding.detail {
        out.push_str(&format!("\n    detail: {detail}"));
    }
    Ok(out)
}

fn add(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let line = args
        .get("--line")
        .map(|raw| {
            raw.trim()
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    anyhow::anyhow!("--line must be a positive whole number (got `{raw}`)")
                })
        })
        .transpose()?;
    let finding = NewFinding {
        severity: args.require("--severity")?.to_string(),
        file: args.get("--file").map(str::to_string),
        line,
        summary: args.require("--summary")?.to_string(),
        detail: args.get("--detail").map(str::to_string),
    };
    let result = inv.client().interview(InterviewCommand::AddReviewFinding {
        node_id: node,
        conversation_id: env_uuid(IMPLEMENT_CONVERSATION_ENV)?,
        finding,
    })?;
    let id = result
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .ok_or_else(|| anyhow::anyhow!("the finding was recorded but no id came back"))?;
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok(format!("ok {}", short_id(id)))
}

fn respond(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let id = resolve(inv, args.target("a finding id")?)?;
    let status = normalize(args.require("--status")?, &FINDING_STATUSES, "status")?;
    let response = args
        .get("--response")
        .map(str::trim)
        .filter(|r| !r.is_empty());
    anyhow::ensure!(
        status == FINDING_OPEN || response.is_some(),
        "--status {status} needs --response: the fix, or why not"
    );
    inv.client()
        .interview(InterviewCommand::RespondReviewFinding {
            finding_id: id,
            status: status.to_string(),
            response: response.map(str::to_string),
        })?;
    Ok(format!("ok {}", short_id(id)))
}

/// Record the review finished, in the conversation the app named.
fn done(inv: &Invocation) -> anyhow::Result<String> {
    let conversation = env_uuid(IMPLEMENT_CONVERSATION_ENV)?.ok_or_else(|| {
        anyhow::anyhow!(
            "`review done` only works inside a review conversation: \
             {IMPLEMENT_CONVERSATION_ENV} is not set"
        )
    })?;
    inv.client().read(|conn| {
        ConversationRepo::new(conn)
            .get(conversation)?
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("conversation {conversation} not found"))
    })?;
    inv.client()
        .interview(InterviewCommand::RecordConversationReport {
            conversation_id: conversation,
            body: done_report(),
        })?;
    Ok("ok review done".to_string())
}
