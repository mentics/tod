//! `tod-cli review` — code review findings on a node: what a review
//! conversation's agent found, and the response each one gets.
//!
//! Inside a review or fix conversation the app sets `TOD_IMPLEMENT_NODE` and
//! `TOD_IMPLEMENT_CONVERSATION`, so `--node` defaults to the node under
//! review and each finding is filed under the conversation. `coverage` and
//! `done` only work in a review conversation: `coverage` records what the
//! review checked, lens by lens, and `done` is how the app learns the review
//! is finished, refused until that record is complete
//! (`tod_store::review::coverage`). A fix conversation's agent may only
//! respond `fixed` or `rejected`; the other answers are the user's.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_store::conversation::{ConversationRepo, ProtocolKind};
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::review::{
    CoveragePass, CoverageResult, FINDING_OPEN, FINDING_STATUSES, FIX_AGENT_STATUSES, LENSES,
    NewCoverage, NewFinding, ReviewFinding, ReviewRepo, normalize,
};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli review — code review findings on a node, and what each review checked

Finding ids may be given in full or as the 8-character prefix shown in listings.
Inside a review or fix conversation --node defaults to the node under review.

COMMANDS:
    list      [--node <UUID>] [--open]
    show      <ID>
    add       [--node <UUID>] --severity high|medium|low --summary <TEXT> [--file <PATH>] [--line <N>] [--detail <TEXT>] [--root-cause <TEXT>]
    respond   <ID> --status open|fixed|out_of_scope|declined|rejected [--response <TEXT>]
    coverage  --lens <LENS> | --pass saturation  --checked <TEXT> --result none|n/a|finding:<ID> [--reason <TEXT>] [--file <PATH>]... [--read <PATH>]...
    done

`add` records an open finding: --summary is the defect in a sentence, --detail
the inputs or state that go wrong and how (use `--detail -` and a heredoc for
anything long), --file a repo-relative path and --line its 1-based line,
--root-cause the assumption or pattern that made it possible, in the same words
for every finding that shares it.
`respond` answers one: fixed (the response points at the change), out_of_scope,
declined (not critical, beyond the requirements, or not worth the cost), or
rejected (not a problem after all; the response says why).
Every status but open needs --response; open clears it. Inside a fix
conversation only fixed and rejected are allowed.
`coverage` records one entry of this review's coverage: a lens it swept, or
its saturation pass. --result is none, n/a (with --reason, for a lens that does
not apply), or finding:<ID>, repeated for each finding. --file names a changed
file the entry covers and --read a file read for it; both repeat. Lenses:
obligations, data-assumptions, external-systems, persisted-results,
interactions, reach, data-writes, concurrency, error-paths, tests,
verification-evidence.
`done` records that this review is finished. It is refused, naming what is
missing, until every lens has an entry and a saturation pass that found
nothing was recorded after this review's last finding. Both only work inside
a review conversation (TOD_IMPLEMENT_CONVERSATION=<UUID>).
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
        "coverage" => coverage(&inv, &args),
        "done" => done(&inv),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// `--node`, else the node a review conversation is running on.
fn node(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = crate::node::flag(inv, args, "--node")? {
        return Ok(node);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required outside a review conversation"))
}

pub(crate) fn env_uuid(name: &str) -> anyhow::Result<Option<Uuid>> {
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
        "root_cause": f.root_cause,
        "reviewer": f.reviewer.map(|r| r.as_str()),
    })
}

fn finding_line(f: &ReviewFinding) -> String {
    let location = f.location().map(|l| format!(" {l}")).unwrap_or_default();
    let root_cause = match &f.root_cause {
        Some(cause) => format!("\n    root cause: {cause}"),
        None => String::new(),
    };
    let reviewer = match f.reviewer {
        Some(reviewer) => format!("\n    reviewer: {}", reviewer.title()),
        None => String::new(),
    };
    let response = match &f.response {
        Some(response) => format!("\n    response: {response}"),
        None => String::new(),
    };
    format!(
        "[{}] {} {}{location}: {}{root_cause}{reviewer}{response}",
        short_id(f.id),
        f.status,
        f.severity,
        f.summary
    )
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
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
    let node = node(inv, args)?;
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
        root_cause: args.get("--root-cause").map(str::to_string),
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
    if in_fix_conversation(inv)? {
        anyhow::ensure!(
            FIX_AGENT_STATUSES.contains(&status),
            "a fix or pull request conversation may only respond {}: out_of_scope and declined \
             are the user's to give, and open is where the finding already is",
            FIX_AGENT_STATUSES.join(" or ")
        );
    }
    inv.client()
        .interview(InterviewCommand::RespondReviewFinding {
            finding_id: id,
            status: status.to_string(),
            response: response.map(str::to_string),
        })?;
    Ok(format!("ok {}", short_id(id)))
}

/// Whether this runs inside a fix conversation, or the pull request's (which
/// answers tod's own review in `pr_draft`), whose agent may only give the
/// statuses in [`FIX_AGENT_STATUSES`].
fn in_fix_conversation(inv: &Invocation) -> anyhow::Result<bool> {
    let Some(conversation) = env_uuid(IMPLEMENT_CONVERSATION_ENV)? else {
        return Ok(false);
    };
    inv.client().read(|conn| {
        Ok(ConversationRepo::new(conn)
            .get(conversation)?
            .is_some_and(|c| matches!(c.protocol, ProtocolKind::Fix | ProtocolKind::Pr)))
    })
}

/// The review conversation the app named; `verb` is what needs it.
fn review_conversation(verb: &str) -> anyhow::Result<Uuid> {
    env_uuid(IMPLEMENT_CONVERSATION_ENV)?.ok_or_else(|| {
        anyhow::anyhow!(
            "`review {verb}` only works inside a review conversation: \
             {IMPLEMENT_CONVERSATION_ENV} is not set"
        )
    })
}

/// Record one entry of the review's coverage: a lens, or the saturation pass.
fn coverage(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let conversation = review_conversation("coverage")?;
    let lenses = || LENSES.iter().map(|l| l.slug).collect::<Vec<_>>().join(", ");
    let pass = match (args.get("--lens"), args.get("--pass")) {
        (Some(lens), None) => CoveragePass::Lens(lens.to_string()),
        (None, Some(pass)) if pass.trim().eq_ignore_ascii_case("saturation") => {
            CoveragePass::Saturation
        }
        (None, Some(other)) => anyhow::bail!("unknown pass `{other}` (expected saturation)"),
        (Some(_), Some(_)) => anyhow::bail!("give --lens or --pass saturation, not both"),
        (None, None) => anyhow::bail!(
            "--lens <LENS> or --pass saturation is required (lenses: {})",
            lenses()
        ),
    };
    let mut findings = Vec::new();
    let mut plain = Vec::new();
    for raw in args.get_all("--result") {
        match raw.trim().strip_prefix("finding:") {
            Some(id) => findings.push(resolve(inv, id)?),
            None => plain.push(raw.trim().to_ascii_lowercase()),
        }
    }
    let reason = args.get("--reason").map(str::trim).filter(|r| !r.is_empty());
    let result = match (plain.as_slice(), findings.is_empty()) {
        ([], false) => CoverageResult::Findings(findings),
        ([one], true) if one == "none" => CoverageResult::None,
        ([one], true) if one == "n/a" => CoverageResult::NotApplicable(
            reason
                .ok_or_else(|| {
                    anyhow::anyhow!("--result n/a needs --reason: why the lens does not apply")
                })?
                .to_string(),
        ),
        ([], true) => anyhow::bail!("--result is required: none, n/a, or finding:<ID>"),
        _ => anyhow::bail!(
            "--result is none, n/a, or finding:<ID> (repeated for each finding), not a mix \
             (got {})",
            args.get_all("--result").join(", ")
        ),
    };
    if reason.is_some() && !matches!(result, CoverageResult::NotApplicable(_)) {
        anyhow::bail!("--reason goes with --result n/a only");
    }
    let entry = NewCoverage {
        pass,
        files: args.get_all("--file").into_iter().map(str::to_string).collect(),
        checked: args.get("--checked").map(str::to_string),
        result,
        read: args.get_all("--read").into_iter().map(str::to_string).collect(),
    };
    let label = match &entry.pass {
        CoveragePass::Lens(lens) => format!("lens {lens}"),
        CoveragePass::Saturation => "saturation pass".to_string(),
    };
    inv.client().interview(InterviewCommand::RecordReviewCoverage {
        conversation_id: conversation,
        entry,
    })?;
    Ok(format!("ok coverage {label}"))
}

/// Record the review finished, in the conversation the app named; refused,
/// with what is missing, until its coverage is complete.
fn done(inv: &Invocation) -> anyhow::Result<String> {
    let conversation = review_conversation("done")?;
    inv.client()
        .interview(InterviewCommand::RecordReviewDone { conversation_id: conversation })?;
    Ok("ok review done".to_string())
}
