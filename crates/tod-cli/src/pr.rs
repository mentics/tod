//! `tod-cli pr` — opening and driving a node's pull request, from the `pr`
//! lifecycle state's conversation.
//!
//! Inside a `pr` conversation the app sets `TOD_IMPLEMENT_NODE` and
//! `TOD_IMPLEMENT_CONVERSATION`, so `--node` defaults to the node the
//! conversation is running on. `mergeable`/`merged` are how the app learns
//! the protocol's job is done — `pr → approved` and `approved → merged` are
//! app-checked gates, not agent-decided, so the agent's job ends at
//! `mergeable`.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_core::conversation::pr::{blocked_report, merged_report, mergeable_report};
use tod_store::conversation::ConversationRepo;
use tod_store::credentials::{CredentialStore, resolve_github_token};
use tod_store::github::{self, NodePrRepo};
use tod_store::interview::{InterviewCommand, short_id};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli pr — opening and driving a node's pull request

Inside a `pr` conversation --node defaults to the node under work.

COMMANDS:
    open                              [--node <UUID>] --owner <OWNER> --repo <REPO> --head <BRANCH> --base <BRANCH> --title <TEXT> [--body <TEXT>]
    status                            [--node <UUID>]
    comment reply <COMMENT_ID> <TEXT> [--node <UUID>]
    mergeable                         [--note <TEXT>]
    merged                            [--note <TEXT>]
    blocked                           --why <TEXT>

`open` creates the pull request via the GitHub API and records it on the node
(owner/repo/number/url); it only works once per node — call `status` first if
you are not sure one already exists.
`status` fetches the PR's live mergeable flag, combined check status, and
merged flag.
`comment reply` posts a reply to a review comment thread by its numeric id
(shown by GitHub, not tod's short ids).
`mergeable` records that the PR is ready for the `pr → approved` gate to
check (checks green, reviews satisfied) — it does not itself approve
anything. `blocked` records that you cannot make further progress without the
user (an unresolvable conflict, a requested change you cannot judge) and
hands back with why.
`merged` records the terminal state when it was merged out of
band. Both only work inside a `pr` conversation.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    match command.as_str() {
        "open" => {
            let args = Args::parse(&rest)?;
            open(&inv, &args)
        }
        "status" => {
            let args = Args::parse(&rest)?;
            status(&inv, &args)
        }
        "comment" => comment(&inv, &rest),
        "mergeable" => {
            let args = Args::parse(&rest)?;
            mergeable(&inv, &args)
        }
        "merged" => {
            let args = Args::parse(&rest)?;
            merged(&inv, &args)
        }
        "blocked" => {
            let args = Args::parse(&rest)?;
            blocked(&inv, &args)
        }
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn node(args: &Args) -> anyhow::Result<Uuid> {
    if let Some(node) = args.uuid("--node")? {
        return Ok(node);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required outside a pr conversation"))
}

fn env_uuid(name: &str) -> anyhow::Result<Option<Uuid>> {
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => Uuid::parse_str(raw.trim())
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{name} is not a UUID (`{raw}`)")),
        _ => Ok(None),
    }
}

fn token(inv: &Invocation) -> anyhow::Result<String> {
    let store = CredentialStore::from_data_root(&inv.data_root);
    resolve_github_token(&store)
        .ok_or_else(|| anyhow::anyhow!("no GitHub token configured — see `tod-cli secrets`"))
}

fn open(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    if inv.client().read(|conn| NodePrRepo::new(conn).get(node))?.is_some() {
        anyhow::bail!("node {node} already has a pull request on record — run `pr status`");
    }
    let owner = args.require("--owner")?.to_string();
    let repo = args.require("--repo")?.to_string();
    let head = args.require("--head")?.to_string();
    let base = args.require("--base")?.to_string();
    let title = args.require("--title")?.to_string();
    let body = args.get("--body").unwrap_or_default().to_string();
    let token = token(inv)?;
    let pr = github::create_pr(&token, &owner, &repo, &head, &base, &title, &body)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?;
    inv.client().interview(InterviewCommand::RecordNodePr {
        node_id: node,
        owner,
        repo,
        pr_number: pr.number,
        url: pr.url.clone(),
    })?;
    Ok(format!("ok {}", pr.url))
}

fn status(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let pr = inv
        .client()
        .read(|conn| NodePrRepo::new(conn).get(node))?
        .ok_or_else(|| anyhow::anyhow!("no pull request on record for node {node} — run `pr open`"))?;
    let token = token(inv)?;
    let status = github::get_pr_status(&token, &pr.owner, &pr.repo, pr.pr_number)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?;
    if inv.json {
        return Ok(serde_json::to_string(&serde_json::json!({
            "url": pr.url,
            "mergeable": status.mergeable,
            "mergeable_state": status.mergeable_state,
            "merged": status.merged,
            "checks": status.checks,
        }))?);
    }
    Ok(format!(
        "{} mergeable={:?} mergeable_state={:?} merged={} checks={:?}",
        pr.url, status.mergeable, status.mergeable_state, status.merged, status.checks
    ))
}

fn comment(inv: &Invocation, rest: &[String]) -> anyhow::Result<String> {
    if rest.first().map(String::as_str) != Some("reply") {
        anyhow::bail!("unknown `pr comment` command — expected `reply`\n\n{}", USAGE.trim_end());
    }
    let rest = &rest[1..];
    if rest.len() < 2 {
        anyhow::bail!("usage: pr comment reply <COMMENT_ID> <TEXT> [--node <UUID>]");
    }
    let comment_id: i64 = rest[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("comment id must be a number (`{}`)", rest[0]))?;
    let args = Args::parse(&rest[2..])?;
    let text = rest[1].clone();
    let node = node(&args)?;
    let pr = inv
        .client()
        .read(|conn| NodePrRepo::new(conn).get(node))?
        .ok_or_else(|| anyhow::anyhow!("no pull request on record for node {node}"))?;
    let token = token(inv)?;
    github::reply_to_comment(&token, &pr.owner, &pr.repo, pr.pr_number, comment_id, &text)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?;
    Ok("ok".to_string())
}

fn conversation(inv: &Invocation) -> anyhow::Result<Uuid> {
    let conversation = env_uuid(IMPLEMENT_CONVERSATION_ENV)?.ok_or_else(|| {
        anyhow::anyhow!(
            "this only works inside a pr conversation: {IMPLEMENT_CONVERSATION_ENV} is not set"
        )
    })?;
    inv.client().read(|conn| {
        ConversationRepo::new(conn)
            .get(conversation)?
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("conversation {conversation} not found"))
    })?;
    Ok(conversation)
}

fn mergeable(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let conversation = conversation(inv)?;
    let note = args.get("--note").map(str::to_string);
    inv.client()
        .interview(InterviewCommand::RecordConversationReport {
            conversation_id: conversation,
            body: mergeable_report(note),
        })?;
    Ok(format!("ok mergeable ({})", short_id(conversation)))
}

fn merged(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let conversation = conversation(inv)?;
    let note = args.get("--note").map(str::to_string);
    inv.client()
        .interview(InterviewCommand::RecordConversationReport {
            conversation_id: conversation,
            body: merged_report(note),
        })?;
    Ok(format!("ok recorded ({})", short_id(conversation)))
}

fn blocked(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let conversation = conversation(inv)?;
    let why = args.require("--why")?.to_string();
    inv.client()
        .interview(InterviewCommand::RecordConversationReport {
            conversation_id: conversation,
            body: blocked_report(why),
        })?;
    Ok(format!("ok blocked ({})", short_id(conversation)))
}
