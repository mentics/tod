//! tod's own code review in `pr_draft`, when the node's lifecycle config
//! includes the review steps there (`tod_store::lifecycle_config::resolve_pr_draft_review`).
//!
//! The point is to get each change exactly right before a person is troubled
//! with it again. So when the pull request has feedback to act on, the
//! autopilot (`Autopilot::babysit`) splits the pull request agent's work in
//! two: first it makes the change and commits it, without pushing or
//! answering anything; then, if the branch's change against its base differs
//! from before ([`branch_change`]: merging the base in alone does not count),
//! tod's own Review → Fix → Review loop runs on the local head ([`change_step`])
//! until a review records nothing that stands, with the same round limit as
//! `review`; only then is the agent told to push and answer the threads. The
//! review's report records the head it cleared ([`heads_report`], with
//! [`local_heads`]), which is the head the push makes current, so [`owed`]
//! finds nothing more to do once it is pushed.
//!
//! The reviewer records findings exactly as it does in `review`, through
//! `tod-cli review`; it never posts anything itself. A head that reaches the
//! pull request without that loop (pushed from outside, or the agent's first
//! turn in the stay) is still reviewed once nothing but a person is left on
//! it ([`Owed::Review`]), and what that review records is posted on the pull
//! request as one review ([`post`]): each finding on a line of the change goes
//! on that line, one on a file the change touches but not on a changed line
//! goes on the file, and the rest in the review's text. Each finding records
//! the thread it became (`ReviewFinding::pr_thread`). From there an open
//! thread is feedback like any other, and answering it (`tod-cli pr threads
//! answer`) answers its finding. A finding no open thread carries is sent to
//! the agent by name ([`Owed::Answer`]). A thread a person resolves without an
//! answer leaves its finding declined ([`sync_resolved`]).
//!
//! Like `review`, only a review can clear the change: [`Owed::Clear`] once a
//! review of the current head recorded nothing that stands. The `pr_draft →
//! pr_review` gate checks the same thing (`pr-pr_review.own-review`).

use crate::pr_readiness::{LivePr, PrFeed, ReviewPost, pr_label};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tod_store::conversation::{Conversation, ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::{FleetStore, Workdir};
use tod_store::interview::InterviewCommand;
use tod_store::phase::PhaseRepo;
use tod_store::review::{FINDING_DECLINED, FINDING_OUT_OF_SCOPE, ReviewFinding, ReviewRepo};
use uuid::Uuid;

/// The lifecycle state this review runs in.
pub const STATE: &str = "pr_draft";

/// The key of the report the app adds to a finished review: the head of each
/// linked pull request it reviewed, by [`pr_label`].
pub const HEADS_KEY: &str = "pr_heads";

/// The key of the report the app adds to a finished review round in
/// `review`: the head of each repository in the node's checkout (the
/// superproject's and every submodule's), by lowercase `owner/repo`
/// ([`repo_heads`]). No pull request exists yet; once one does, the round
/// covers its first head when that head is the change it reviewed
/// ([`latest_review`]).
pub const REPO_HEADS_KEY: &str = "repo_heads";

/// The lifecycle state the code review runs in before the pull request opens.
const REVIEW_STATE: &str = "review";

/// Who the app's own writes here are attributed to.
const ACTOR: &str = "autopilot";

/// The states with the pull request open in which, with the review steps on,
/// a change the pull request's agent makes is reviewed before it is pushed:
/// `pr_draft`, and `pr_review` too, so a head pushed after a person's review
/// was still reviewed by tod first.
pub const STATES: [&str; 2] = [STATE, "pr_review"];

/// Whether a review run in lifecycle state `state` is one of these: on the
/// pull request's local head, by `surface/review-on-pr`.
pub fn reviews_on_pr(state: &str) -> bool {
    STATES.contains(&state)
}

/// Whether `pr_draft` (and so `pr_review`) includes the review steps for
/// `node`.
pub fn enabled(conn: &Connection, node: Uuid) -> Result<bool> {
    Ok(tod_store::lifecycle_config::resolve_pr_draft_review(conn, node)?.enabled)
}

/// A review conversation run with the pull request open ([`STATES`]), in the
/// node's current stay when `since` says when it began.
fn is_own(conversation: &Conversation, since: Option<i64>) -> bool {
    conversation.protocol == ProtocolKind::Review
        && conversation.from_state.as_deref().is_some_and(reviews_on_pr)
        && since.is_none_or(|since| conversation.created_at >= since)
}

/// The latest review (its first group's session) run in `pr_draft` in this
/// stay. With none there yet, in `pr_draft`: the `review` phase's round that
/// brought the node here, when it is the node's latest review of all, came
/// back clean ([`crate::conversation::review::review_passed`]) and recorded the
/// heads it reviewed ([`REPO_HEADS_KEY`]). It reviewed exactly the change the
/// pull request opened with, and a second full review of the same change
/// before anything moved is waste; a head that differs from it by more than a
/// merge of the base is still owed a review ([`owed`]).
pub fn latest_review(conn: &Connection, node: Uuid) -> Result<Option<Conversation>> {
    let phases = PhaseRepo::new(conn);
    let since = phases.stay_started_at(node)?;
    let Some(latest) = ConversationRepo::new(conn).latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)? else {
        return Ok(None);
    };
    if is_own(&latest, since) {
        return Ok(Some(latest));
    }
    let carried = latest.from_state.as_deref() == Some(REVIEW_STATE)
        && phases.current_state(node)?.as_deref() == Some(STATE)
        && crate::conversation::review::review_passed(conn, node)?
        && ConversationRepo::new(conn)
            .latest_report(latest.id)?
            .is_some_and(|r| r.get(REPO_HEADS_KEY).is_some() || r.get(HEADS_KEY).is_some());
    Ok(carried.then_some(latest))
}

/// The consecutive reviews in this stay that did not come back clean, counted
/// back from the latest and stopping at the most recent clean one: the rounds
/// counted against `REVIEW_ROUND_LIMIT`. A clean review (finished, and
/// recording nothing that stands, as [`round_clean`]) resets the count, since
/// a review is owed every time the head changes (a merge of the base, a CI
/// fix, an answer to a bot) and clean ones prove convergence, not the
/// opposite. A review's round is its findings up to the next review. Only a
/// review that finished (recorded done) is a round: one whose agent died is not.
pub fn unconverged_rounds(conn: &Connection, node: Uuid) -> Result<u32> {
    unconverged_rounds_since(conn, node, None)
}

/// [`unconverged_rounds`] over only the reviews started at or after
/// `since_ms` (the review of one change, [`reviews_since`]), counted the same
/// way so the two cannot drift.
pub fn unconverged_rounds_since(conn: &Connection, node: Uuid, since_ms: Option<i64>) -> Result<u32> {
    let since = PhaseRepo::new(conn).stay_started_at(node)?;
    let repo = ConversationRepo::new(conn);
    let mut reviews: Vec<Conversation> = repo
        .conversations_for_focus(Focus::Node(node))?
        .into_iter()
        .filter(|c| c.skill_group == 0 && !c.is_risk_reviewer() && is_own(c, since) && since_ms.is_none_or(|ms| c.created_at >= ms))
        .collect();
    reviews.sort_by_key(|c| c.created_at);
    let findings = own_findings(conn, node)?;
    let mut count = 0;
    let mut next_start = i64::MAX;
    for review in reviews.iter().rev() {
        // Both reviewers of the round finished (`tod_store::review::round`).
        let done = tod_store::review::round::done(conn, review.id)?;
        let stands = findings
            .iter()
            .filter(|f| f.created_at >= review.created_at && f.created_at < next_start)
            .any(|f| !matches!(f.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED));
        next_start = review.created_at;
        // A review the agent never finished (killed, crashed) is not a round.
        if !done {
            continue;
        }
        if !stands {
            break;
        }
        count += 1;
    }
    Ok(count)
}

/// What the latest review in this stay left standing, in words, for
/// [`crate::autopilot::NeedsHuman::ReviewNotConverging`]: short id, summary
/// and status of each finding not dismissed. Empty when nothing stands.
pub fn stay_standing(conn: &Connection, node: Uuid) -> Result<Vec<String>> {
    let latest = latest_review(conn, node)?;
    Ok(own_findings(conn, node)?
        .iter()
        .filter(|f| latest.as_ref().is_some_and(|l| f.created_at >= l.created_at))
        .filter(|f| !matches!(f.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED))
        .map(|f| format!("[{}] {} ({})", tod_store::interview::short_id(f.id), f.summary, f.status))
        .collect())
}

/// The findings of tod's own reviews in `pr_draft`, oldest first: every one
/// recorded in this stay, and any still open from an earlier stay (a rerun
/// or a move back starts a new stay, but a finding nobody answered is still
/// owed a post and a fix). Only the stay's own reviews say the head was
/// reviewed ([`latest_review`], [`round_clean`]).
pub fn own_findings(conn: &Connection, node: Uuid) -> Result<Vec<ReviewFinding>> {
    let since = PhaseRepo::new(conn).stay_started_at(node)?;
    let conversations = ConversationRepo::new(conn);
    // Per conversation: (a review run in `pr_draft`, run in this stay).
    let mut own: BTreeMap<Uuid, (bool, bool)> = BTreeMap::new();
    let mut out = Vec::new();
    for finding in ReviewRepo::new(conn).list_for_node(node)? {
        let Some(id) = finding.conversation_id else { continue };
        let (ever, this_stay) = match own.get(&id) {
            Some(known) => *known,
            None => {
                let known = conversations
                    .get(id)?
                    .map_or((false, false), |c| (is_own(&c, None), is_own(&c, since)));
                own.insert(id, known);
                known
            }
        };
        if this_stay || (ever && finding.is_open()) {
            out.push(finding);
        }
    }
    Ok(out)
}

/// The head of each linked pull request, by [`pr_label`].
pub fn current_heads(live: &[LivePr]) -> BTreeMap<String, String> {
    live.iter()
        .filter_map(|p| Some((pr_label(&p.pr), p.head_sha.clone()?)))
        .collect()
}

/// The report the app records on a finished review: the review's own `done`
/// and the heads it reviewed.
pub fn heads_report(heads: &BTreeMap<String, String>) -> Value {
    json!({ "review": "done", HEADS_KEY: heads })
}

/// The heads the review `conversation` recorded reviewing; `None` until the
/// app recorded them (or when the review round, both of its reviewers, is not
/// finished).
pub fn reviewed_heads(conn: &Connection, conversation: Uuid) -> Result<Option<BTreeMap<String, String>>> {
    if !tod_store::review::round::done(conn, conversation)? {
        return Ok(None);
    }
    let report = ConversationRepo::new(conn).latest_report(conversation)?;
    Ok(report
        .filter(crate::conversation::review::is_done_report)
        .and_then(|r| r.get(HEADS_KEY).cloned())
        .and_then(|heads| serde_json::from_value(heads).ok()))
}

/// The head of each of `live`'s pull requests that the review `conversation`
/// covered, by [`pr_label`]: the heads it recorded ([`reviewed_heads`]), or,
/// for a round in `review` (run before any pull request), each pull
/// request's repository's head it recorded ([`REPO_HEADS_KEY`]). `None` when
/// it recorded neither, or not one of these pull requests' repositories.
fn heads_reviewed(conn: &Connection, conversation: &Conversation, live: &[LivePr]) -> Result<Option<BTreeMap<String, String>>> {
    if let Some(heads) = reviewed_heads(conn, conversation.id)? {
        return Ok(Some(heads));
    }
    if conversation.from_state.as_deref() != Some(REVIEW_STATE) || !tod_store::review::round::done(conn, conversation.id)? {
        return Ok(None);
    }
    let Some(repos) = ConversationRepo::new(conn)
        .latest_report(conversation.id)?
        .and_then(|r| r.get(REPO_HEADS_KEY).cloned())
        .and_then(|heads| serde_json::from_value::<BTreeMap<String, String>>(heads).ok())
    else {
        return Ok(None);
    };
    let mut out = BTreeMap::new();
    for p in live {
        let key = format!("{}/{}", p.pr.owner, p.pr.repo).to_ascii_lowercase();
        // One repository and no other is the pull request's, whatever its
        // `origin` is called ([`repo_dir`]).
        let head = repos.get(&key).or_else(|| (repos.len() == 1).then(|| repos.values().next()).flatten());
        let Some(head) = head else { return Ok(None) };
        out.insert(pr_label(&p.pr), head.clone());
    }
    Ok(Some(out))
}

/// Records on `node`'s latest review round in `review`, once it is done, the
/// head of each repository in the node's checkout `cwd` ([`REPO_HEADS_KEY`]),
/// so that round can count for the pull request's first head
/// ([`latest_review`]). Nothing when the review steps are off in `pr_draft`
/// or the round is not done.
pub fn record_review_phase_heads(fleet: &FleetStore, node: Uuid, cwd: &Workdir) -> Result<()> {
    let latest = fleet.read(|conn| {
        if !enabled(conn, node)? {
            return Ok(None);
        }
        let Some(latest) = ConversationRepo::new(conn).latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)? else {
            return Ok(None);
        };
        let done = latest.from_state.as_deref() == Some(REVIEW_STATE) && tod_store::review::round::done(conn, latest.id)?;
        Ok(done.then_some(latest.id))
    })?;
    let Some(conversation_id) = latest else { return Ok(()) };
    let heads = repo_heads(cwd).map_err(|err| err.context(format!("recording the heads tod's review in `review` covered, in {cwd}")))?;
    fleet.interview(
        ACTOR,
        InterviewCommand::RecordConversationReport {
            conversation_id,
            body: json!({ "review": "done", REPO_HEADS_KEY: heads }),
        },
    )?;
    Ok(())
}

/// The head of each repository in the checkout `cwd`, the superproject's and
/// every submodule's at any depth, by lowercase `owner/repo` from its
/// `origin` ([`repo_of`]). A repository with no `origin` it can read one
/// from is left out.
pub fn repo_heads(cwd: &Workdir) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut dirs = vec![cwd.clone()];
    let paths = cwd.git(&["submodule", "foreach", "--quiet", "--recursive", "echo \"$displaypath\""])?;
    dirs.extend(paths.lines().map(str::trim).filter(|p| !p.is_empty()).map(|p| cwd.join(p)));
    for dir in dirs {
        let origin = dir.git(&["remote", "get-url", "origin"]).unwrap_or_default();
        let Some(repo) = repo_of(&origin) else { continue };
        out.insert(repo, local_head(&dir)?);
    }
    Ok(out)
}

/// `owner/repo`, lowercase, from a remote's URL (`git@host:owner/repo.git`,
/// `https://host/owner/repo`, ...); `None` when it does not have both.
fn repo_of(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let mut parts = url.rsplit(['/', ':']).filter(|p| !p.is_empty());
    let repo = parts.next()?;
    let owner = parts.next()?;
    Some(format!("{owner}/{repo}").to_ascii_lowercase())
}

/// Whether the findings of the review round that `latest` began (its own and
/// its later groups') all stand dismissed: none recorded, or each one the
/// user answered out of scope or declined. One fixed or rejected still
/// counts, as in `review`: only another review can say the change is right.
fn round_clean(findings: &[ReviewFinding], latest: &Conversation) -> bool {
    findings
        .iter()
        .filter(|f| f.created_at >= latest.created_at)
        .all(|f| matches!(f.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED))
}

/// What tod's own review owes the pull request now that nothing else is left
/// on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owed {
    /// Nothing: the review steps are off, or the latest review of the
    /// current head recorded nothing that stands.
    Clear,
    /// Findings recorded and not yet posted on the pull request.
    Post(Vec<ReviewFinding>),
    /// Open findings no open review thread carries (posted in the review's
    /// text, or their thread went outdated): the pull request's agent answers
    /// them by name.
    Answer(Vec<ReviewFinding>),
    /// A review of the current head.
    Review,
}

/// [`Owed`] for `node`, whose linked pull requests stand at `live`.
pub fn owed(conn: &Connection, node: Uuid, live: &[LivePr], feed: &dyn PrFeed) -> Result<Owed> {
    if !enabled(conn, node)? {
        return Ok(Owed::Clear);
    }
    let findings = own_findings(conn, node)?;
    let unposted: Vec<ReviewFinding> =
        findings.iter().filter(|f| f.is_open() && f.pr_posted_at.is_none()).cloned().collect();
    if !unposted.is_empty() {
        return Ok(Owed::Post(unposted));
    }
    let open_threads: BTreeSet<&str> = live
        .iter()
        .flat_map(|p| p.threads.iter().filter(|t| !t.resolved && !t.outdated).map(|t| t.id.as_str()))
        .collect();
    let unasked: Vec<ReviewFinding> = findings
        .iter()
        .filter(|f| f.is_open() && f.pr_thread.as_deref().is_none_or(|t| !open_threads.contains(t)))
        .cloned()
        .collect();
    if !unasked.is_empty() {
        return Ok(Owed::Answer(unasked));
    }
    let Some(latest) = latest_review(conn, node)? else {
        return Ok(Owed::Review);
    };
    let current = current_heads(live);
    match heads_reviewed(conn, &latest, live)? {
        None => return Ok(Owed::Review),
        Some(reviewed) if reviewed != current => {
            // A head that only merges the base in is the change already
            // reviewed: base moves every few minutes on a busy repository, and
            // a full review per merge never ends. Any other difference (a
            // commit of the branch's own, a reviewed head GitHub no longer
            // has) is a review owed. Not telling is an error, never a guess.
            if reviewed.keys().ne(current.keys()) {
                return Ok(Owed::Review);
            }
            for p in live {
                let label = pr_label(&p.pr);
                let (Some(was), Some(now)) = (reviewed.get(&label), current.get(&label)) else { continue };
                if was == now {
                    continue;
                }
                let base = p.base_ref.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("GitHub did not say which branch {} merges into, so a review of {was} cannot be compared with head {now}", p.pr.url)
                })?;
                // A head that moved only by tod's own fixer commits (a formatter,
                // a lint autofix: `tod_store::pr_pushes::VERDICT_MECHANICAL`) is
                // exempt from review.
                let same = tod_store::pr_pushes::PrPushRepo::new(conn).only_mechanical_since(node, &label, was)?
                    || feed
                        .same_change(&p.pr, base, was, now)
                        .map_err(|err| anyhow::anyhow!("{}: comparing tod's review of {was} with head {now}: {err}", p.pr.url))?;
                if !same {
                    return Ok(Owed::Review);
                }
            }
        }
        Some(_) => {}
    }
    Ok(if round_clean(&findings, &latest) { Owed::Clear } else { Owed::Review })
}

/// Carries the latest review's heads over to the pull request's current ones,
/// once [`owed`] has said that nothing is owed: the head moved only by merging
/// the base in, which is the change already reviewed. The
/// `pr-pr_review.own-review` gate ([`gate`]) compares heads exactly, so without
/// this the review would be called clear here and the gate would then refuse
/// it as "not reviewed the current head", with no review ever started.
pub fn carry_heads(fleet: &FleetStore, node: Uuid, live: &[LivePr]) -> Result<()> {
    let current = current_heads(live);
    let carried = fleet.read(|conn| {
        let Some(latest) = latest_review(conn, node)? else { return Ok(None) };
        // A round in `review` recorded no pull request's head: the one it
        // covers is recorded on it here, even when it is the very head it
        // reviewed.
        let recorded = reviewed_heads(conn, latest.id)?;
        Ok(match (recorded, heads_reviewed(conn, &latest, live)?) {
            (Some(reviewed), _) if reviewed == current => None,
            (_, Some(_)) => Some(latest.id),
            _ => None,
        })
    })?;
    if let Some(conversation_id) = carried {
        fleet.interview(
            ACTOR,
            InterviewCommand::RecordConversationReport { conversation_id, body: heads_report(&current) },
        )?;
    }
    Ok(())
}

/// The branch's own change against `base` (a branch of `origin`), as a
/// fingerprint: the changed lines (committed or not, against the merge base
/// of `origin/<base>` and `HEAD`) and the untracked files, without hunk
/// positions or blob ids. Merging the base in leaves it as it was, since the
/// merge base moves with it and the lines around the change are not counted;
/// any change to the branch's own lines changes it. `Err` when git fails.
pub fn branch_change(cwd: &Workdir, base: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let base_ref = format!("origin/{base}");
    let merge_base = cwd.git(&["merge-base", &base_ref, "HEAD"])?;
    let diff = cwd.git(&["diff", "--no-color", "--no-ext-diff", "--no-renames", "-U0", &merge_base])?;
    let untracked = cwd.git(&["ls-files", "--others", "--exclude-standard"])?;
    let mut hash = Sha256::new();
    for line in diff.lines().filter(|l| !l.starts_with("@@") && !l.starts_with("index ")) {
        hash.update(line.as_bytes());
        hash.update(b"\n");
    }
    hash.update(b"untracked\n");
    hash.update(untracked.as_bytes());
    Ok(format!("{:x}", hash.finalize()))
}

/// Whether `url` (a remote's URL: `git@host:owner/repo.git`,
/// `https://host/owner/repo`, ...) names `owner/repo`.
fn remote_is(url: &str, owner: &str, repo: &str) -> bool {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let want = format!("{owner}/{repo}").to_ascii_lowercase();
    let url = url.to_ascii_lowercase();
    url.strip_suffix(&want).is_some_and(|rest| rest.ends_with('/') || rest.ends_with(':'))
}

/// The checkout of `owner/repo` in `cwd`: `cwd` itself when its `origin` is
/// that repository, else the submodule (at any depth) whose `origin` is. A
/// pull request on a submodule's repository is that submodule's change; the
/// superproject sees it only as a moved `Subproject commit` line, which any
/// commit at all moves, a merge of the base included. A checkout with no
/// submodules is the one repository there is. `Err` when, among submodules,
/// none is.
pub fn repo_dir(cwd: &Workdir, owner: &str, repo: &str) -> Result<Workdir> {
    let origin = cwd.git(&["remote", "get-url", "origin"]).unwrap_or_default();
    if remote_is(&origin, owner, repo) {
        return Ok(cwd.clone());
    }
    let paths = cwd.git(&["submodule", "foreach", "--quiet", "--recursive", "echo \"$displaypath\""])?;
    if paths.trim().is_empty() {
        // One repository and no other: the pull request's, whatever its
        // `origin` is called (a local mirror, a proxy's URL).
        return Ok(cwd.clone());
    }
    let mut seen = Vec::new();
    for path in paths.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let dir = cwd.join(path);
        let origin = dir.git(&["remote", "get-url", "origin"])?;
        if remote_is(&origin, owner, repo) {
            return Ok(dir);
        }
        seen.push(format!("{path} ({origin})"));
    }
    anyhow::bail!(
        "no checkout of {owner}/{repo} in {cwd}: its origin is {} and its submodules are {}",
        if origin.is_empty() { "unset".to_string() } else { origin },
        if seen.is_empty() { "none".to_string() } else { seen.join(", ") }
    )
}

/// [`branch_change`] of every linked pull request, each in its own
/// repository's checkout ([`repo_dir`]) against its own base: what tells a
/// change the agent made from a merge of a base, whichever repository (the
/// superproject or a submodule) the pull request is on. `Err` when GitHub did
/// not say a pull request's base, or git fails.
pub fn pr_change(cwd: &Workdir, live: &[LivePr]) -> Result<String> {
    let prs: Vec<(&tod_store::github::NodePr, Option<&str>)> = live.iter().map(|p| (&p.pr, p.base_ref.as_deref())).collect();
    pr_change_of(cwd, &prs)
}

/// [`pr_change`] of `(pull request, its base)` pairs.
fn pr_change_of(cwd: &Workdir, prs: &[(&tod_store::github::NodePr, Option<&str>)]) -> Result<String> {
    let mut out = String::new();
    for (pr, base) in prs {
        let base = base.ok_or_else(|| {
            anyhow::anyhow!(
                "GitHub did not say which branch {} merges into, so its change cannot be told from a merge of its base",
                pr.url
            )
        })?;
        let dir = repo_dir(cwd, &pr.owner, &pr.repo)?;
        let change = branch_change(&dir, base)
            .map_err(|err| err.context(format!("reading {}'s change against origin/{base} in {dir}", pr.url)))?;
        out.push_str(&format!("{}={change}\n", pr_label(pr)));
    }
    Ok(out)
}

/// The commit checked out in `cwd`: the head about to be pushed.
pub fn local_head(cwd: &Workdir) -> Result<String> {
    cwd.git(&["rev-parse", "HEAD"])
}

/// The local head of each linked pull request, by [`pr_label`]: the commit
/// checked out in its own repository's checkout ([`repo_dir`]), which is what
/// its push makes the pull request's head. For a pull request on a submodule
/// that is the submodule's commit, never the superproject's, which no pull
/// request of it ever shows.
pub fn local_heads(cwd: &Workdir, live: &[LivePr]) -> Result<BTreeMap<String, String>> {
    local_heads_of(cwd, live.iter().map(|p| &p.pr))
}

/// [`local_heads`] of these pull requests.
fn local_heads_of<'a>(cwd: &Workdir, prs: impl IntoIterator<Item = &'a tod_store::github::NodePr>) -> Result<BTreeMap<String, String>> {
    prs.into_iter()
        .map(|pr| {
            let dir = repo_dir(cwd, &pr.owner, &pr.repo)?;
            let head = local_head(&dir).map_err(|err| err.context(format!("reading the head commit of {} in {dir}", pr.url)))?;
            Ok((pr_label(pr), head))
        })
        .collect()
}

/// The reviews (first groups' sessions) run in `pr_draft` in this stay since
/// `since_ms`, oldest first: the rounds of the review of one change.
pub fn reviews_since(conn: &Connection, node: Uuid, since_ms: i64) -> Result<Vec<Conversation>> {
    let stay = PhaseRepo::new(conn).stay_started_at(node)?;
    let mut out: Vec<Conversation> = ConversationRepo::new(conn)
        .conversations_for_focus(Focus::Node(node))?
        .into_iter()
        .filter(|c| c.skill_group == 0 && !c.is_risk_reviewer() && is_own(c, stay) && c.created_at >= since_ms)
        .collect();
    out.sort_by_key(|c| c.created_at);
    Ok(out)
}

/// What the review of a change made in `pr_draft` does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStep {
    /// Review the change (again).
    Review,
    /// Resolve the latest review's open findings.
    Fix,
    /// The latest review recorded nothing that stands.
    Clean,
}

/// [`ChangeStep`] for the change made at `since_ms`, decided as `review`
/// decides its own (`crate::lifecycle_next::next_step`): no finished review
/// of it yet, review; the latest left findings open, fix; it recorded
/// nothing that stands, clean; otherwise (its findings answered by the
/// fixer, whose claim is not the verdict) review again.
pub fn change_step(conn: &Connection, node: Uuid, since_ms: i64) -> Result<ChangeStep> {
    let Some(latest) = reviews_since(conn, node, since_ms)?.pop() else {
        return Ok(ChangeStep::Review);
    };
    if !tod_store::review::round::done(conn, latest.id)? {
        return Ok(ChangeStep::Review);
    }
    let findings = own_findings(conn, node)?;
    if findings.iter().any(|f| f.created_at >= latest.created_at && f.is_open()) {
        return Ok(ChangeStep::Fix);
    }
    Ok(if round_clean(&findings, &latest) { ChangeStep::Clean } else { ChangeStep::Review })
}

/// What the latest review of the change made at `since_ms` left standing, in
/// words, for [`crate::autopilot::NeedsHuman::ReviewNotConverging`].
pub fn change_standing(conn: &Connection, node: Uuid, since_ms: i64) -> Result<Vec<String>> {
    let Some(latest) = reviews_since(conn, node, since_ms)?.pop() else {
        return Ok(Vec::new());
    };
    Ok(own_findings(conn, node)?
        .iter()
        .filter(|f| f.created_at >= latest.created_at)
        .filter(|f| !matches!(f.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED))
        .map(|f| format!("[{}] {} ({})", tod_store::interview::short_id(f.id), f.summary, f.status))
        .collect())
}

/// Whether the latest review in this stay recorded `heads` and came back
/// clean: the head before a change was cleared, so a change that leaves the
/// branch's own lines as they were (a merge of the base) is cleared too.
pub fn cleared(conn: &Connection, node: Uuid, heads: &BTreeMap<String, String>) -> Result<Option<Conversation>> {
    let Some(latest) = latest_review(conn, node)? else { return Ok(None) };
    if reviewed_heads(conn, latest.id)?.is_none_or(|reviewed| &reviewed != heads) {
        return Ok(None);
    }
    let findings = own_findings(conn, node)?;
    let open = findings.iter().any(|f| f.created_at >= latest.created_at && f.is_open());
    Ok((!open && round_clean(&findings, &latest)).then_some(latest))
}

/// The heads (commit SHAs) the stay's latest own review covered and left
/// clean, as [`gate`] reads it: no open finding of any own review, and the
/// latest review's round recorded nothing that stands. Empty when the review
/// steps are off or no review has finished clean. What a review bot's ask and
/// fallback wait for (`tod_store::OwnReviewState`).
///
/// In `pr_review` (`in_review`) it is the heads the latest own review of any
/// stay covered: the one that cleared the `pr_draft` gate, or one run in
/// `pr_review` on a change the agent made there before pushing it. A head
/// pushed there without such a review is not cleared.
pub fn cleared_shas(conn: &Connection, node: Uuid, in_review: bool) -> Result<Vec<String>> {
    if !enabled(conn, node)? {
        return Ok(Vec::new());
    }
    let latest = if in_review {
        ConversationRepo::new(conn)
            .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)?
            .filter(|c| is_own(c, None))
    } else {
        latest_review(conn, node)?
    };
    let Some(latest) = latest else { return Ok(Vec::new()) };
    let Some(heads) = reviewed_heads(conn, latest.id)? else { return Ok(Vec::new()) };
    let findings = own_findings(conn, node)?;
    if findings.iter().any(|f| f.is_open()) || !round_clean(&findings, &latest) {
        return Ok(Vec::new());
    }
    Ok(heads.into_values().collect())
}

/// The `pr_draft → pr_review` gate's check (`pr-pr_review.own-review`), for
/// pull requests whose heads are `heads` (by [`pr_label`]): `Ok` says why it
/// passes, `Err` why not.
pub fn gate(conn: &Connection, node: Uuid, heads: &BTreeMap<String, String>) -> Result<std::result::Result<String, String>> {
    if !enabled(conn, node)? {
        return Ok(Ok("The lifecycle config does not include the review steps in `pr_draft`.".to_string()));
    }
    let findings = own_findings(conn, node)?;
    let open: Vec<String> = findings
        .iter()
        .filter(|f| f.is_open())
        .map(|f| tod_store::interview::short_id(f.id))
        .collect();
    if !open.is_empty() {
        return Ok(Err(format!("tod's code review has open findings: {}.", open.join(", "))));
    }
    let Some(latest) = latest_review(conn, node)? else {
        return Ok(Err("tod's code review has not reviewed the pull request in this stay.".to_string()));
    };
    if reviewed_heads(conn, latest.id)?.is_none_or(|reviewed| &reviewed != heads) {
        return Ok(Err("tod's code review has not reviewed the pull request's current head.".to_string()));
    }
    if !round_clean(&findings, &latest) {
        return Ok(Err(
            "tod's latest code review recorded findings; only another review of the fixed head clears them."
                .to_string(),
        ));
    }
    Ok(Ok("tod's code review of the current head recorded nothing that stands.".to_string()))
}

/// Open findings whose thread is resolved on GitHub with no answer recorded:
/// a person dismissed them there. They are declined, saying so. Returns how
/// many.
pub fn sync_resolved(fleet: &FleetStore, node: Uuid, live: &[LivePr]) -> Result<usize> {
    let resolved: BTreeSet<&str> =
        live.iter().flat_map(|p| p.threads.iter().filter(|t| t.resolved).map(|t| t.id.as_str())).collect();
    let findings = fleet.read(|conn| own_findings(conn, node))?;
    let mut count = 0;
    for finding in findings.iter().filter(|f| f.is_open()) {
        if finding.pr_thread.as_deref().is_some_and(|t| resolved.contains(t)) {
            fleet.interview(
                ACTOR,
                InterviewCommand::RespondReviewFinding {
                    finding_id: finding.id,
                    status: FINDING_DECLINED.to_string(),
                    response: Some("Its review thread was resolved on GitHub without an answer.".to_string()),
                },
            )?;
            count += 1;
        }
    }
    Ok(count)
}

/// Where a finding goes on the pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    Line { path: String, line: i64 },
    File { path: String },
    Text,
}

/// The lines of each changed file's new side that a review comment can be put
/// on: the added and unchanged lines of its patch's hunks.
pub fn commentable_lines(files: &[tod_store::github::PrFile]) -> BTreeMap<String, BTreeSet<i64>> {
    let mut out = BTreeMap::new();
    for file in files {
        let lines: &mut BTreeSet<i64> = out.entry(file.filename.clone()).or_default();
        let Some(patch) = file.patch.as_deref() else { continue };
        let mut next: Option<i64> = None;
        for text in patch.lines() {
            if let Some(header) = text.strip_prefix("@@") {
                // `@@ -a,b +c,d @@`: the new side starts at line c.
                next = header
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix('+'))
                    .and_then(|range| range.split(',').next())
                    .and_then(|start| start.parse().ok());
                continue;
            }
            let Some(line) = next else { continue };
            match text.chars().next() {
                Some('-') => {}
                Some('\\') => {}
                _ => {
                    lines.insert(line);
                    next = Some(line + 1);
                }
            }
        }
    }
    out
}

/// The changed file a finding's path names. Agents record paths from where
/// they stood (`flagship/flagship/settings.py` for a pull request on the
/// `flagship` repository, whose file is `flagship/settings.py`), so a path
/// matches a changed file when either ends with the other at a folder
/// boundary (the longest such file wins). A path that matches none still
/// gets a file, the changed one it shares the most trailing folders with
/// (else the first), so the finding is a thread that can be answered and
/// resolved, never loose text; its body names the location it is about.
fn resolve_file(path: &str, lines: &BTreeMap<String, BTreeSet<i64>>) -> Option<String> {
    if lines.contains_key(path) {
        return Some(path.to_string());
    }
    let ends = |long: &str, short: &str| long.strip_suffix(short).is_some_and(|rest| rest.ends_with('/'));
    if let Some(f) = lines
        .keys()
        .filter(|f| ends(path, f) || ends(f, path))
        .max_by_key(|f| f.len())
    {
        return Some(f.clone());
    }
    let shared = |f: &str| path.rsplit('/').zip(f.rsplit('/')).take_while(|(a, b)| a == b).count();
    let mut best: Option<(&String, usize)> = None;
    for f in lines.keys() {
        let n = shared(f);
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((f, n));
        }
    }
    best.map(|(f, _)| f.clone())
}

fn place(finding: &ReviewFinding, lines: &BTreeMap<String, BTreeSet<i64>>) -> Place {
    // A finding with no file goes on the file with the most changed lines: a
    // thread is the only thing that can be answered and resolved.
    let Some(path) = finding.file.as_deref() else {
        return match lines.iter().max_by_key(|(_, l)| l.len()) {
            Some((file, _)) => Place::File { path: file.clone() },
            None => Place::Text,
        };
    };
    let Some(file) = resolve_file(path, lines) else { return Place::Text };
    let exact = file == path || path.ends_with(&format!("/{file}"));
    match finding.line {
        Some(line) if exact && lines[&file].contains(&line) => Place::Line { path: file, line },
        _ => Place::File { path: file },
    }
}

/// What marks a comment as the one a finding was posted as.
fn marker(finding: &ReviewFinding) -> String {
    format!("<!-- tod-finding:{} -->", finding.id)
}

fn comment_body(finding: &ReviewFinding, place: &Place) -> String {
    let mut out = format!("**[{}]** {}", finding.severity, finding.summary.trim());
    if let (Place::File { path }, Some(location)) = (place, finding.location()) {
        // Say where it is about when the thread could not sit on that line.
        let about = match finding.line {
            Some(line) if location == format!("{path}:{line}") || finding.file.as_deref() == Some(path) => {
                format!("line {line}")
            }
            _ => format!("`{location}`"),
        };
        out = format!("**[{}]** ({about}) {}", finding.severity, finding.summary.trim());
    }
    if let Some(cause) = root_cause(finding) {
        out.push_str(&format!("\n\nRoot cause: {cause}"));
    }
    if let Some(detail) = finding.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        out.push_str("\n\n");
        out.push_str(detail.trim());
    }
    out.push_str(&format!(
        "\n\n<sub>tod code review, finding `{}`</sub>\n{}",
        tod_store::interview::short_id(finding.id),
        marker(finding)
    ));
    out
}

fn text_item(finding: &ReviewFinding) -> String {
    let location = finding.location().map(|l| format!(" `{l}`")).unwrap_or_default();
    let mut out = format!(
        "- **[{}]**{location} {} (finding `{}`)",
        finding.severity,
        finding.summary.trim(),
        tod_store::interview::short_id(finding.id)
    );
    if let Some(cause) = root_cause(finding) {
        out.push_str(&format!("\n\n  Root cause: {cause}"));
    }
    if let Some(detail) = finding.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        out.push_str("\n\n  ");
        out.push_str(&detail.trim().replace('\n', "\n  "));
    }
    out
}

/// The finding's root cause, when the reviewer gave one.
fn root_cause(finding: &ReviewFinding) -> Option<&str> {
    finding.root_cause.as_deref().map(str::trim).filter(|c| !c.is_empty())
}

/// Which of `live` a finding belongs to: the one whose change has its file,
/// else the first.
fn pr_for<'a>(
    finding: &ReviewFinding,
    live: &'a [LivePr],
    lines: &[BTreeMap<String, BTreeSet<i64>>],
) -> usize {
    finding
        .file
        .as_deref()
        .and_then(|path| (0..live.len()).find(|&i| lines[i].contains_key(path)))
        .unwrap_or(0)
}

/// Post `findings` on the pull requests at `live` and record where each went.
/// `Err` says what could not be done; findings already recorded as posted
/// are not posted again.
pub fn post(
    fleet: &FleetStore,
    feed: &dyn PrFeed,
    live: &[LivePr],
    findings: &[ReviewFinding],
) -> std::result::Result<(), String> {
    if live.is_empty() {
        return Err("no pull request is linked to post the review on".to_string());
    }
    let mut lines = Vec::new();
    for p in live {
        let files = feed.changed_files(&p.pr).map_err(|err| format!("{}: {err}", p.pr.url))?;
        lines.push(commentable_lines(&files));
    }
    let mut by_pr: Vec<Vec<(&ReviewFinding, Place)>> = vec![Vec::new(); live.len()];
    for finding in findings {
        let i = pr_for(finding, live, &lines);
        by_pr[i].push((finding, place(finding, &lines[i])));
    }
    for (p, placed) in live.iter().zip(&by_pr) {
        if placed.is_empty() {
            continue;
        }
        let commit_id = p
            .head_sha
            .clone()
            .ok_or_else(|| format!("{}: GitHub did not say which commit is its head", p.pr.url))?;
        let mut post = ReviewPost { commit_id, ..Default::default() };
        let mut text = Vec::new();
        for (finding, place) in placed {
            match place {
                Place::Line { path, line } => post.lines.push(tod_store::github::LineComment {
                    path: path.clone(),
                    line: *line,
                    body: comment_body(finding, place),
                }),
                Place::File { path } => post.files.push((path.clone(), comment_body(finding, place))),
                Place::Text => text.push(text_item(finding)),
            }
        }
        if !text.is_empty() {
            post.body = format!(
                "tod's code review found these where no line of the change can hold them. \
                 Each is answered with the finding's id:\n\n{}",
                text.join("\n")
            );
        }
        feed.post_review(&p.pr, &post).map_err(|err| format!("{}: {err}", p.pr.url))?;
        // The next read of the pull request has to show the threads just
        // posted, not a snapshot from before them.
        crate::pr_readiness::forget_snapshot(&p.pr);
        let threads = feed.threads(&p.pr).map_err(|err| format!("{}: {err}", p.pr.url))?;
        let mut missing = Vec::new();
        for (finding, place) in placed {
            let thread = match place {
                Place::Text => None,
                _ => {
                    let mark = marker(finding);
                    match threads.iter().find(|t| t.comments.first().is_some_and(|c| c.body.contains(&mark))) {
                        Some(t) => Some(t.id.clone()),
                        None => {
                            missing.push(tod_store::interview::short_id(finding.id));
                            None
                        }
                    }
                }
            };
            fleet
                .interview(ACTOR, InterviewCommand::RecordFindingPosted { finding_id: finding.id, thread })
                .map_err(|err| format!("recording finding {} as posted: {err}", finding.id))?;
        }
        if !missing.is_empty() {
            return Err(format!(
                "{}: posted the review, but GitHub shows no thread for finding(s) {}",
                p.pr.url,
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

/// What the pull request's agent is sent for [`Owed::Answer`].
pub fn answer_message(findings: &[ReviewFinding]) -> String {
    let mut out = String::from(
        "# tod's code review: findings to answer\n\n\
         tod's own code review of this pull request recorded these findings, and no open \
         review thread carries them. For each one, fix it or decide it is not a defect, push, \
         then answer it through the `review` noun (fixed, or rejected with a note saying why \
         it is not a problem). Answer every one.\n\n",
    );
    for finding in findings {
        out.push_str(&format!(
            "- `{}` [{}]{} {}\n",
            finding.id,
            finding.severity,
            finding.location().map(|l| format!(" {l}")).unwrap_or_default(),
            finding.summary.trim()
        ));
        if let Some(cause) = root_cause(finding) {
            out.push_str(&format!("  Root cause: {cause}\n"));
        }
        if let Some(detail) = finding.detail.as_deref().filter(|d| !d.trim().is_empty()) {
            out.push_str(&format!("  {}\n", detail.trim().replace('\n', "\n  ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_names_its_repository_in_any_url_form() {
        for url in [
            "git@github.com:Cofactr/flagship.git",
            "https://github.com/Cofactr/flagship",
            "https://github.com/cofactr/flagship.git/",
            "ssh://git@github.com/Cofactr/flagship.git",
        ] {
            assert!(remote_is(url, "Cofactr", "flagship"), "{url}");
        }
        assert!(!remote_is("git@github.com:Cofactr/flagship-ui.git", "Cofactr", "flagship"));
        assert!(!remote_is("git@github.com:Other/xflagship.git", "Cofactr", "flagship"));
        assert!(!remote_is("", "Cofactr", "flagship"));
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "protocol.file.allow=always", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The pull request is on a submodule's repository. Merging that
    /// repository's moved base in changes the superproject's view of the
    /// change (its `Subproject commit` line) but not the pull request's own
    /// change, which is what decides whether a review is owed; a commit of
    /// the branch's own does change it.
    #[test]
    fn a_merge_of_a_submodules_base_is_not_a_change_of_its_pull_request() {
        let root = tod_test_util::scratch_dir("pr-change-submodule");
        let lib_origin = root.join("lib.git");
        let top_origin = root.join("top.git");
        // The submodule's repository, with a main branch.
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q"]);
        std::fs::write(seed.join("a.txt"), "one\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "lib"]);
        git(&root, &["clone", "-q", "--bare", seed.to_str().unwrap(), lib_origin.to_str().unwrap()]);
        // The superproject, with the submodule.
        let top_seed = root.join("top-seed");
        std::fs::create_dir_all(&top_seed).unwrap();
        git(&top_seed, &["init", "-q"]);
        git(&top_seed, &["submodule", "add", "-q", lib_origin.to_str().unwrap(), "lib"]);
        git(&top_seed, &["commit", "-qm", "top"]);
        git(&root, &["clone", "-q", "--bare", top_seed.to_str().unwrap(), top_origin.to_str().unwrap()]);
        let work = root.join("work");
        git(&root, &["clone", "-q", "--recurse-submodules", top_origin.to_str().unwrap(), work.to_str().unwrap()]);
        let lib = work.join("lib");
        git(&work, &["remote", "set-url", "origin", "git@github.com:acme/top.git"]);
        git(&lib, &["remote", "set-url", "origin", "git@github.com:acme/lib.git"]);
        // The node's branch in the submodule, with its own change.
        git(&lib, &["checkout", "-qb", "task/x"]);
        std::fs::write(lib.join("b.txt"), "mine\n").unwrap();
        git(&lib, &["add", "."]);
        git(&lib, &["commit", "-qm", "own change"]);
        git(&work, &["add", "lib"]);
        git(&work, &["commit", "-qm", "bump lib"]);

        let cwd = Workdir::host(&work);
        let pr = tod_store::github::NodePr::new("acme", "lib", 7);
        assert_eq!(repo_dir(&cwd, "acme", "lib").unwrap(), Workdir::host(&lib));
        assert_eq!(repo_dir(&cwd, "acme", "top").unwrap(), cwd);
        assert!(repo_dir(&cwd, "acme", "other").is_err());
        let before = pr_change_of(&cwd, &[(&pr, Some("main"))]).unwrap();
        let top_before = branch_change(&cwd, "main").unwrap();

        // The base moves upstream, and the agent merges it in.
        git(&seed, &["remote", "add", "up", lib_origin.to_str().unwrap()]);
        std::fs::write(seed.join("c.txt"), "theirs\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "base moved"]);
        git(&seed, &["push", "-q", "up", "HEAD:main"]);
        git(&lib, &["fetch", "-q", lib_origin.to_str().unwrap(), "main"]);
        git(&lib, &["update-ref", "refs/remotes/origin/main", "FETCH_HEAD"]);
        git(&lib, &["merge", "-q", "--no-edit", "origin/main"]);
        git(&work, &["add", "lib"]);
        git(&work, &["commit", "-qm", "bump lib again"]);

        assert_ne!(branch_change(&cwd, "main").unwrap(), top_before, "the superproject sees any submodule commit");
        assert_eq!(pr_change_of(&cwd, &[(&pr, Some("main"))]).unwrap(), before, "a merge of the base alone is no change");

        // A commit of the branch's own is.
        std::fs::write(lib.join("b.txt"), "mine, fixed\n").unwrap();
        git(&lib, &["commit", "-qam", "fix"]);
        assert_ne!(pr_change_of(&cwd, &[(&pr, Some("main"))]).unwrap(), before);
        // A pull request GitHub gave no base for cannot be told.
        assert!(pr_change_of(&cwd, &[(&pr, None)]).is_err());

        // What a clean review of the change records, and what tod's push
        // makes the pull request's head: the submodule's commit, never the
        // superproject's (ENG-3558: the superproject's was recorded, so the
        // pull request's head read as unreviewed and was reviewed again).
        let lib_head = git(&lib, &["rev-parse", "HEAD"]);
        let top_head = git(&work, &["rev-parse", "HEAD"]);
        assert_ne!(lib_head, top_head);
        assert_eq!(local_heads_of(&cwd, [&pr]).unwrap(), BTreeMap::from([("acme/lib#7".to_string(), lib_head.clone())]));
        // What a review in `review` records, before any pull request: every
        // repository's head.
        assert_eq!(
            repo_heads(&cwd).unwrap(),
            BTreeMap::from([("acme/lib".to_string(), lib_head), ("acme/top".to_string(), top_head)])
        );
    }

    #[test]
    fn a_remote_url_gives_its_owner_and_repository() {
        assert_eq!(repo_of("git@github.com:Cofactr/flagship.git").as_deref(), Some("cofactr/flagship"));
        assert_eq!(repo_of("https://github.com/Cofactr/flagship/").as_deref(), Some("cofactr/flagship"));
        assert_eq!(repo_of("ssh://git@github.com/Cofactr/flagship.git").as_deref(), Some("cofactr/flagship"));
        assert_eq!(repo_of("flagship"), None);
        assert_eq!(repo_of(""), None);
    }

    fn finding(file: Option<&str>, line: Option<i64>) -> ReviewFinding {
        ReviewFinding {
            id: Uuid::new_v4(),
            node_id: Uuid::nil(),
            conversation_id: None,
            seq: 1,
            severity: "high".into(),
            file: file.map(String::from),
            line,
            summary: "Wrong".into(),
            detail: Some("Because".into()),
            status: "open".into(),
            response: None,
            created_at: 0,
            updated_at: 0,
            pr_thread: None,
            pr_posted_at: None,
            root_cause: None,
            reviewer: None,
        }
    }

    #[test]
    fn commentable_lines_are_the_new_side_of_each_hunk() {
        let files = vec![
            tod_store::github::PrFile {
                filename: "src/a.rs".into(),
                patch: Some("@@ -1,3 +1,4 @@\n ctx\n-old\n+new\n+added\n ctx2\n@@ -20,2 +30,2 @@ fn x()\n a\n+b".into()),
            },
            tod_store::github::PrFile { filename: "bin.png".into(), patch: None },
        ];
        let lines = commentable_lines(&files);
        assert_eq!(lines["src/a.rs"].iter().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4, 30, 31]);
        assert!(lines["bin.png"].is_empty());
    }

    #[test]
    fn a_finding_goes_on_its_line_else_its_file_else_the_text() {
        let files = vec![tod_store::github::PrFile {
            filename: "src/a.rs".into(),
            patch: Some("@@ -1,1 +1,2 @@\n a\n+b".into()),
        }];
        let lines = commentable_lines(&files)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(place(&finding(Some("src/a.rs"), Some(2)), &lines), Place::Line { path: "src/a.rs".into(), line: 2 });
        assert_eq!(place(&finding(Some("src/a.rs"), Some(9)), &lines), Place::File { path: "src/a.rs".into() });
        assert_eq!(place(&finding(Some("src/a.rs"), None), &lines), Place::File { path: "src/a.rs".into() });
        // A path from a parent folder still names the changed file, and its line.
        assert_eq!(
            place(&finding(Some("repo/src/a.rs"), Some(2)), &lines),
            Place::Line { path: "src/a.rs".into(), line: 2 }
        );
        // A file the change does not touch is still a thread, on the changed file.
        assert_eq!(place(&finding(Some("src/other.rs"), Some(1)), &lines), Place::File { path: "src/a.rs".into() });
        assert_eq!(place(&finding(None, None), &lines), Place::File { path: "src/a.rs".into() });
        assert_eq!(place(&finding(None, None), &BTreeMap::new()), Place::Text);
    }

    #[test]
    fn a_comment_carries_the_marker_its_thread_is_found_by() {
        let f = finding(Some("src/a.rs"), Some(9));
        let body = comment_body(&f, &Place::File { path: "src/a.rs".into() });
        assert!(body.contains(&marker(&f)), "{body}");
        assert!(body.starts_with("**[high]** (line 9) Wrong"), "{body}");
    }
}
