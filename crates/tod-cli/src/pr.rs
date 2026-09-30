//! `tod-cli pr` — opening and driving a node's pull request, from the `pr`
//! lifecycle state's conversation.
//!
//! Inside a `pr` conversation the app sets `TOD_IMPLEMENT_NODE` and
//! `TOD_IMPLEMENT_CONVERSATION`, so `--node` defaults to the node the
//! conversation is running on. `mergeable`/`merged` are how the app learns
//! the protocol's job is done — `pr → approved` and `approved → merged` are
//! app-checked gates, not agent-decided, so the agent's job ends at
//! `mergeable`. `list` works anywhere: it shows the pull requests of the
//! node's repository and its submodules.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use tod_core::conversation::pr::{blocked_report, merged_report, mergeable_report};
use tod_core::pull_requests::{
    NodePulls, PullScope, RepoPulls, RepoSection, load_node_pulls, read_target,
};
use tod_store::conversation::ConversationRepo;
use tod_store::credentials::{CredentialStore, resolve_github_auth};
use tod_store::github::{Github, NodePrRepo, PrLinks, parse_pr_link};
use tod_store::interview::{InterviewCommand, short_id};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli pr — opening and driving a node's pull request

Inside a `pr` conversation --node defaults to the node under work.

COMMANDS:
    list                              [--node <UUID>] [--all-open]
    open                              [--node <UUID>] --owner <OWNER> --repo <REPO> --head <BRANCH> --base <BRANCH> --title <TEXT> [--body <TEXT>]
    status                            [--node <UUID>]
    comment reply <COMMENT_ID> <TEXT> [--node <UUID>] [--pr <LINK>]
    threads                           [--node <UUID>] [--pr <LINK>] [--all]
    threads answer <THREAD_ID>        (--fixed | --rejected) --reply <TEXT> [--node <UUID>] [--pr <LINK>]
    mergeable                         [--note <TEXT>]
    merged                            [--note <TEXT>]
    blocked                           --why <TEXT>

A node's pull requests are the links in its Ticket capability
(`capabilities set <NODE> ticket --pr <URL>`), which the user can also edit;
the `pr → approved` and `approved → merged` gates check every one.
`list` shows every pull request, in any state, from the node's branch in each
repository its work spans: the one its Files capability names and every
submodule in it (all on the same branch), plus every one linked; with
--all-open, every open pull request in those repositories instead,
whichever branch it is from.
`open` creates the pull request via the GitHub API (or finds the one already
open from --head) and links it to the node, enabling Ticket if it is off; it
refuses when the node already links a pull request in that repository — call
`status` first if you are not sure one already exists.
`status` fetches each linked PR's live mergeable flag, combined check status,
and merged flag.
`comment reply` posts a reply to a review comment thread by its numeric id
(shown by GitHub, not tod's short ids), on the linked PR --pr names (its URL
or <OWNER>/<REPO>#<NUMBER>) — needed only when the node links more than one.
`threads` lists the review threads still open (human and bot alike, with the
file and line, every comment, and roughly how many rounds it has had),
or all of them with --all. `threads answer` answers one by its id: it
posts --reply (which must say what you did, or why you did not) and resolves
the thread, all as the user. --fixed says you changed
code for it (push first, so the reply can name the commit); --rejected says
you did not, and why. If you cannot decide a thread, ask the user with
`decisions ask` instead.
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
        "list" => {
            let args = Args::parse(&rest)?;
            list(&inv, &args)
        }
        "open" => {
            let args = Args::parse(&rest)?;
            open(&inv, &args)
        }
        "status" => {
            let args = Args::parse(&rest)?;
            status(&inv, &args)
        }
        "comment" => comment(&inv, &rest),
        "threads" => threads(&inv, &rest),
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

fn github(inv: &Invocation) -> anyhow::Result<Github> {
    let store = CredentialStore::from_data_root(&inv.data_root);
    resolve_github_auth(&store)
        .map(Github::new)
        .ok_or_else(|| anyhow::anyhow!("no GitHub token configured — see `tod-cli secrets`"))
}

fn list(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let scope = if args.has("--all-open") {
        PullScope::AllOpen
    } else {
        PullScope::Branch
    };
    let target = inv.client().read(|conn| read_target(conn, node))?;
    let pulls =
        load_node_pulls(target, scope, &inv.data_root).map_err(|err| anyhow::anyhow!(err))?;
    if inv.json {
        return Ok(serde_json::to_string(&pulls_json(&pulls))?);
    }
    Ok(pulls_text(&pulls))
}

fn section_label(section: &RepoSection) -> String {
    match (&section.github, section.path.is_empty()) {
        (Some(repo), true) => repo.to_string(),
        (Some(repo), false) => format!("{} ({repo})", section.path),
        (None, true) => "(superproject)".to_string(),
        (None, false) => section.path.clone(),
    }
}

fn pulls_text(pulls: &NodePulls) -> String {
    let branch = pulls.branch.as_deref().unwrap_or("(no branch)");
    let mut lines = vec![match pulls.scope {
        PullScope::Branch => format!("branch {branch}"),
        PullScope::AllOpen => "every open pull request".to_string(),
    }];
    for section in &pulls.sections {
        lines.push(String::new());
        lines.push(section_label(section));
        match &section.pulls {
            RepoPulls::Listed(list) if list.is_empty() => lines.push(match pulls.scope {
                PullScope::Branch => format!("  no pull request from {branch}"),
                PullScope::AllOpen => "  no open pull request".to_string(),
            }),
            RepoPulls::Listed(list) => {
                for pull in list {
                    lines.push(format!(
                        "  #{} {} {} → {}  {}",
                        pull.number,
                        pull.state.as_str(),
                        pull.head,
                        pull.base,
                        pull.title
                    ));
                    lines.push(format!("     {}", pull.url));
                }
            }
            RepoPulls::NotGithub { remote: Some(url) } => {
                lines.push(format!("  not on GitHub: {url}"));
            }
            RepoPulls::NotGithub { remote: None } => lines.push("  no remote".to_string()),
            RepoPulls::NoBranch => lines.push("  not on a branch".to_string()),
            RepoPulls::Failed(err) => lines.push(format!("  failed: {err}")),
        }
    }
    for warning in &pulls.warnings {
        lines.push(format!("warning: {warning}"));
    }
    lines.join("\n")
}

fn pulls_json(pulls: &NodePulls) -> serde_json::Value {
    let sections: Vec<_> = pulls
        .sections
        .iter()
        .map(|section| {
            let (status, detail, list) = match &section.pulls {
                RepoPulls::Listed(list) => ("listed", None, list.as_slice()),
                RepoPulls::NotGithub { remote } => ("not_github", remote.clone(), &[][..]),
                RepoPulls::NoBranch => ("no_branch", None, &[][..]),
                RepoPulls::Failed(err) => ("failed", Some(err.clone()), &[][..]),
            };
            let pulls: Vec<_> = list
                .iter()
                .map(|pull| {
                    serde_json::json!({
                        "number": pull.number,
                        "title": pull.title,
                        "url": pull.url,
                        "state": pull.state.as_str(),
                        "author": pull.author,
                        "head": pull.head,
                        "base": pull.base,
                        "updated_at": pull.updated_at,
                    })
                })
                .collect();
            serde_json::json!({
                "path": section.path,
                "github": section.github.as_ref().map(ToString::to_string),
                "status": status,
                "detail": detail,
                "pulls": pulls,
            })
        })
        .collect();
    serde_json::json!({
        "scope": match pulls.scope {
            PullScope::Branch => "branch",
            PullScope::AllOpen => "all_open",
        },
        "branch": pulls.branch,
        "sections": sections,
        "warnings": pulls.warnings,
    })
}

/// The pull requests linked to `node`, refusing when there are none.
fn linked(inv: &Invocation, node: Uuid) -> anyhow::Result<PrLinks> {
    let links = inv.client().read(|conn| NodePrRepo::new(conn).read(node))?;
    if links.prs.is_empty() {
        match links.unrecognized.first() {
            Some(link) => anyhow::bail!(
                "node {node} links `{link}`, which does not say which pull request it is — \
                 give its URL or <OWNER>/<REPO>#<NUMBER>"
            ),
            None => anyhow::bail!("no pull request is linked to node {node} — run `pr open`"),
        }
    }
    Ok(links)
}

fn open(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let owner = args.require("--owner")?.to_string();
    let repo = args.require("--repo")?.to_string();
    let head = args.require("--head")?.to_string();
    let base = args.require("--base")?.to_string();
    let title = args.require("--title")?.to_string();
    let body = args.get("--body").unwrap_or_default().to_string();
    let links = inv.client().read(|conn| NodePrRepo::new(conn).read(node))?;
    if let Some(pr) = links.prs.iter().find(|pr| {
        pr.owner.eq_ignore_ascii_case(&owner) && pr.repo.eq_ignore_ascii_case(&repo)
    }) {
        anyhow::bail!(
            "node {node} already links {} in {owner}/{repo} — run `pr status`",
            pr.url
        );
    }
    let github = github(inv)?;
    // GitHub itself is the source of truth for whether one already exists —
    // not just the node's links checked above — so a retry after the local
    // write below failed (the PR now orphaned from tod's perspective) finds
    // the existing PR instead of opening a duplicate.
    let pr = match github.find_open_pr(&owner, &repo, &head)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?
    {
        Some(pr) => pr,
        None => github.create_pr(&owner, &repo, &head, &base, &title, &body)
            .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?,
    };
    inv.client()
        .interview(InterviewCommand::RecordNodePr {
            node_id: node,
            owner,
            repo,
            pr_number: pr.number,
            url: pr.url.clone(),
        })
        .map_err(|err| {
            anyhow::anyhow!(
                "{} is open on GitHub but could not be linked to the node: {err}. \
                 Re-run `pr open` once fixed — it will find this PR rather than opening \
                 a duplicate.",
                pr.url
            )
        })?;
    Ok(format!("ok {}", pr.url))
}

fn status(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(args)?;
    let links = linked(inv, node)?;
    let github = github(inv)?;
    let mut lines = Vec::new();
    let mut pulls = Vec::new();
    for pr in &links.prs {
        let status = github
            .get_pr_status(&pr.owner, &pr.repo, pr.pr_number)
            .map_err(|err| anyhow::anyhow!("GitHub ({}): {err}", pr.url))?;
        lines.push(format!(
            "{} mergeable={:?} mergeable_state={:?} merged={} checks={:?}",
            pr.url, status.mergeable, status.mergeable_state, status.merged, status.checks
        ));
        pulls.push(serde_json::json!({
            "url": pr.url,
            "mergeable": status.mergeable,
            "mergeable_state": status.mergeable_state,
            "merged": status.merged,
            "checks": status.checks,
        }));
    }
    if inv.json {
        return Ok(serde_json::to_string(&serde_json::json!({
            "pulls": pulls,
            "unrecognized_links": links.unrecognized,
        }))?);
    }
    for link in &links.unrecognized {
        lines.push(format!(
            "warning: the link `{link}` does not say which pull request it is"
        ));
    }
    Ok(lines.join("\n"))
}

fn comment(inv: &Invocation, rest: &[String]) -> anyhow::Result<String> {
    if rest.first().map(String::as_str) != Some("reply") {
        anyhow::bail!("unknown `pr comment` command — expected `reply`\n\n{}", USAGE.trim_end());
    }
    let rest = &rest[1..];
    if rest.len() < 2 {
        anyhow::bail!("usage: pr comment reply <COMMENT_ID> <TEXT> [--node <UUID>] [--pr <LINK>]");
    }
    let comment_id: i64 = rest[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("comment id must be a number (`{}`)", rest[0]))?;
    let args = Args::parse(&rest[2..])?;
    let text = rest[1].clone();
    let node = node(&args)?;
    let mut prs = linked(inv, node)?.prs;
    let pr = match args.get("--pr") {
        Some(link) => {
            let wanted = parse_pr_link(link).ok_or_else(|| {
                anyhow::anyhow!("--pr `{link}` is not a pull request URL or <OWNER>/<REPO>#<NUMBER>")
            })?;
            prs.into_iter()
                .find(|pr| pr.same_as(&wanted))
                .ok_or_else(|| anyhow::anyhow!("node {node} does not link {}", wanted.url))?
        }
        None if prs.len() == 1 => prs.remove(0),
        None => anyhow::bail!(
            "node {node} links {} pull requests — say which with --pr <LINK>",
            prs.len()
        ),
    };
    github(inv)?
        .reply_to_comment(&pr.owner, &pr.repo, pr.pr_number, comment_id, &text)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?;
    Ok("ok".to_string())
}

/// The one linked pull request `--pr` names, or the only one linked.
fn pick_pr(
    mut prs: Vec<tod_store::github::NodePr>,
    args: &Args,
    node: Uuid,
) -> anyhow::Result<tod_store::github::NodePr> {
    match args.get("--pr") {
        Some(link) => {
            let wanted = parse_pr_link(link).ok_or_else(|| {
                anyhow::anyhow!("--pr `{link}` is not a pull request URL or <OWNER>/<REPO>#<NUMBER>")
            })?;
            prs.into_iter()
                .find(|pr| pr.same_as(&wanted))
                .ok_or_else(|| anyhow::anyhow!("node {node} does not link {}", wanted.url))
        }
        None if prs.len() == 1 => Ok(prs.remove(0)),
        None => anyhow::bail!("node {node} links {} pull requests — say which with --pr <LINK>", prs.len()),
    }
}

fn threads(inv: &Invocation, rest: &[String]) -> anyhow::Result<String> {
    if rest.first().map(String::as_str) == Some("answer") {
        return answer_thread(inv, &rest[1..]);
    }
    let args = Args::parse(rest)?;
    let node = node(&args)?;
    let pr = pick_pr(linked(inv, node)?.prs, &args, node)?;
    let all = args.has("--all");
    let found = github(inv)?
        .list_review_threads(&pr.owner, &pr.repo, pr.pr_number)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?;
    let shown: Vec<_> = found.iter().filter(|t| all || (!t.resolved && !t.outdated)).collect();
    if inv.json {
        let list: Vec<_> = shown
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "resolved": t.resolved,
                    "outdated": t.outdated,
                    "path": t.path,
                    "line": t.line,
                    "rounds": tod_core::pr_readiness::rounds(t),
                    "comments": t.comments.iter().map(|c| serde_json::json!({
                        "author": c.author,
                        "body": c.body,
                        "created_at": c.created_at,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        return Ok(serde_json::to_string(&list)?);
    }
    if shown.is_empty() {
        return Ok(format!("no open review threads on {}", pr.url));
    }
    let mut out = Vec::new();
    for t in shown {
        let state = if t.resolved { "resolved" } else if t.outdated { "outdated" } else { "open" };
        out.push(format!(
            "thread {}  {}  {}:{}  answered {}x",
            t.id,
            state,
            t.path.as_deref().unwrap_or("(no file)"),
            t.line.map_or_else(|| "?".to_string(), |l| l.to_string()),
            tod_core::pr_readiness::rounds(t)
        ));
        if let Some(hunk) = t.comments.first().and_then(|c| c.diff_hunk.as_deref()) {
            out.push(format!("  code:\n{}", indent(hunk, "    ")));
        }
        for c in &t.comments {
            out.push(format!(
                "  {}:\n{}",
                c.author.as_deref().unwrap_or("?"),
                indent(c.body.trim(), "    ")
            ));
        }
    }
    Ok(out.join("\n"))
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines().map(|l| format!("{prefix}{l}")).collect::<Vec<_>>().join("\n")
}

fn answer_thread(inv: &Invocation, rest: &[String]) -> anyhow::Result<String> {
    let Some(thread_id) = rest.first().filter(|a| !a.starts_with("--")) else {
        anyhow::bail!("usage: pr threads answer <THREAD_ID> (--fixed | --rejected) --reply <TEXT>");
    };
    let args = Args::parse(&rest[1..])?;
    let fixed = args.has("--fixed");
    if fixed == args.has("--rejected") {
        anyhow::bail!("say which: --fixed (you changed code) or --rejected (you did not)");
    }
    let reply = args.require("--reply")?.trim().to_string();
    if reply.is_empty() {
        anyhow::bail!("--reply must say what you did, or why you did not");
    }
    let node = node(&args)?;
    let pr = pick_pr(linked(inv, node)?.prs, &args, node)?;
    let github = github(inv)?;
    let found = github
        .list_review_threads(&pr.owner, &pr.repo, pr.pr_number)
        .map_err(|err| anyhow::anyhow!("GitHub: {err}"))?;
    let thread = found
        .iter()
        .find(|t| &t.id == thread_id)
        .ok_or_else(|| anyhow::anyhow!("{} has no review thread {thread_id}", pr.url))?;
    if thread.resolved {
        return Ok(format!("thread {thread_id} is already resolved"));
    }
    github
        .reply_to_thread(thread_id, reply)
        .map_err(|err| anyhow::anyhow!("GitHub (reply): {err}"))?;
    github
        .resolve_thread(thread_id)
        .map_err(|err| anyhow::anyhow!("replied, but could not resolve the thread: GitHub: {err}"))?;
    Ok(format!("ok {} ({})", thread_id, if fixed { "fixed" } else { "rejected" }))
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
