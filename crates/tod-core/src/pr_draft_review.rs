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

/// Who the app's own writes here are attributed to.
const ACTOR: &str = "autopilot";

/// Whether `pr_draft` includes the review steps for `node`.
pub fn enabled(conn: &Connection, node: Uuid) -> Result<bool> {
    Ok(tod_store::lifecycle_config::resolve_pr_draft_review(conn, node)?.enabled)
}

/// A review conversation run in `pr_draft` in the node's current stay there.
fn is_own(conversation: &Conversation, since: Option<i64>) -> bool {
    conversation.protocol == ProtocolKind::Review
        && conversation.from_state.as_deref() == Some(STATE)
        && since.is_none_or(|since| conversation.created_at >= since)
}

/// The latest review (its first group's session) run in `pr_draft` in this stay.
pub fn latest_review(conn: &Connection, node: Uuid) -> Result<Option<Conversation>> {
    let since = PhaseRepo::new(conn).stay_started_at(node)?;
    Ok(ConversationRepo::new(conn)
        .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)?
        .filter(|c| is_own(c, since)))
}

/// The consecutive reviews in this stay that did not come back clean, counted
/// back from the latest and stopping at the most recent clean one: the rounds
/// counted against `REVIEW_ROUND_LIMIT`. A clean review (finished, and
/// recording nothing that stands, as [`round_clean`]) resets the count, since
/// a review is owed every time the head changes (a merge of the base, a CI
/// fix, an answer to a bot) and clean ones prove convergence, not the
/// opposite. A review's round is its findings up to the next review.
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
        .filter(|c| c.skill_group == 0 && is_own(c, since) && since_ms.is_none_or(|ms| c.created_at >= ms))
        .collect();
    reviews.sort_by_key(|c| c.created_at);
    let findings = own_findings(conn, node)?;
    let mut count = 0;
    let mut next_start = i64::MAX;
    for review in reviews.iter().rev() {
        let done = repo
            .latest_report(review.id)?
            .is_some_and(|r| crate::conversation::review::is_done_report(&r));
        let stands = findings
            .iter()
            .filter(|f| f.created_at >= review.created_at && f.created_at < next_start)
            .any(|f| !matches!(f.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED));
        if done && !stands {
            break;
        }
        count += 1;
        next_start = review.created_at;
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
/// app recorded them (or when the review is not finished).
pub fn reviewed_heads(conn: &Connection, conversation: Uuid) -> Result<Option<BTreeMap<String, String>>> {
    let report = ConversationRepo::new(conn).latest_report(conversation)?;
    Ok(report
        .filter(crate::conversation::review::is_done_report)
        .and_then(|r| r.get(HEADS_KEY).cloned())
        .and_then(|heads| serde_json::from_value(heads).ok()))
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
pub fn owed(conn: &Connection, node: Uuid, live: &[LivePr]) -> Result<Owed> {
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
    if reviewed_heads(conn, latest.id)?.is_none_or(|heads| heads != current_heads(live)) {
        return Ok(Owed::Review);
    }
    Ok(if round_clean(&findings, &latest) { Owed::Clear } else { Owed::Review })
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

/// The commit checked out in `cwd`: the head about to be pushed.
pub fn local_head(cwd: &Workdir) -> Result<String> {
    cwd.git(&["rev-parse", "HEAD"])
}

/// The heads a review of the local head `head` covers, by [`pr_label`]: every
/// linked pull request, since the node's branch is the head of each.
pub fn local_heads(live: &[LivePr], head: &str) -> BTreeMap<String, String> {
    live.iter().map(|p| (pr_label(&p.pr), head.to_string())).collect()
}

/// The reviews (first groups' sessions) run in `pr_draft` in this stay since
/// `since_ms`, oldest first: the rounds of the review of one change.
pub fn reviews_since(conn: &Connection, node: Uuid, since_ms: i64) -> Result<Vec<Conversation>> {
    let stay = PhaseRepo::new(conn).stay_started_at(node)?;
    let mut out: Vec<Conversation> = ConversationRepo::new(conn)
        .conversations_for_focus(Focus::Node(node))?
        .into_iter()
        .filter(|c| c.skill_group == 0 && is_own(c, stay) && c.created_at >= since_ms)
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
    let done = ConversationRepo::new(conn)
        .latest_report(latest.id)?
        .is_some_and(|r| crate::conversation::review::is_done_report(&r));
    if !done {
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
    if let Some(detail) = finding.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        out.push_str("\n\n  ");
        out.push_str(&detail.trim().replace('\n', "\n  "));
    }
    out
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
        if let Some(detail) = finding.detail.as_deref().filter(|d| !d.trim().is_empty()) {
            out.push_str(&format!("  {}\n", detail.trim().replace('\n', "\n  ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
