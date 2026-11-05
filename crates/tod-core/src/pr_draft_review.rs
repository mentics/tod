//! tod's own code review in `pr_draft`, when the node's lifecycle config
//! includes the review steps there (`tod_store::lifecycle_config::resolve_pr_draft_review`).
//!
//! The review bots run on the draft, the pull request's agent fixes what they
//! say, and the bots may be satisfied while the project's own review would
//! not be. So once nothing is left on the pull request but a person, the
//! autopilot runs the `review` phase's code review (the Review protocol, with
//! the `review` phase's skills, in every group) on the current head. The
//! reviewer records findings exactly as it does in `review`, through
//! `tod-cli review`; it never posts anything itself. The app then posts them
//! on the pull request as one review ([`post`]): each finding on a line of the
//! change goes on that line, one on a file the change touches but not on a
//! changed line goes on the file, and the rest in the review's text. Each
//! finding records the thread it became (`ReviewFinding::pr_thread`).
//!
//! From there the pull request's own loop does the fixing: an open thread is
//! work for its agent, which also has the `fix` phase's skills while the
//! review steps are on, and answering a thread (`tod-cli pr threads answer`)
//! answers its finding. A finding no open thread carries (put in the review's
//! text, or its thread went outdated) is sent to the agent by name
//! ([`Owed::Answer`]). A thread a person resolves without an answer leaves
//! its finding declined ([`sync_resolved`]).
//!
//! Like `review`, only a review can clear the change: once its findings are
//! answered, the next time nothing else is left the head is reviewed again,
//! until a review of the current head records nothing that stands
//! ([`Owed::Clear`]). The `pr_draft → pr_review` gate checks the same thing
//! (`pr-pr_review.own-review`).

use crate::pr_readiness::{LivePr, PrFeed, ReviewPost, pr_label};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tod_store::conversation::{Conversation, ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
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

/// How many reviews (first groups' sessions) have run in `pr_draft` in this
/// stay: the rounds counted against `REVIEW_ROUND_LIMIT`.
pub fn reviews_this_stay(conn: &Connection, node: Uuid) -> Result<u32> {
    let since = PhaseRepo::new(conn).stay_started_at(node)?;
    Ok(ConversationRepo::new(conn)
        .conversations_for_focus(Focus::Node(node))?
        .iter()
        .filter(|c| c.skill_group == 0 && is_own(c, since))
        .count() as u32)
}

/// The findings recorded by the reviews run in `pr_draft` in this stay, any
/// group's, oldest first.
pub fn own_findings(conn: &Connection, node: Uuid) -> Result<Vec<ReviewFinding>> {
    let since = PhaseRepo::new(conn).stay_started_at(node)?;
    let conversations = ConversationRepo::new(conn);
    let mut own = BTreeMap::new();
    let mut out = Vec::new();
    for finding in ReviewRepo::new(conn).list_for_node(node)? {
        let Some(id) = finding.conversation_id else { continue };
        let mine = match own.get(&id) {
            Some(mine) => *mine,
            None => {
                let mine = conversations.get(id)?.is_some_and(|c| is_own(&c, since));
                own.insert(id, mine);
                mine
            }
        };
        if mine {
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

fn place(finding: &ReviewFinding, lines: &BTreeMap<String, BTreeSet<i64>>) -> Place {
    let Some(path) = finding.file.as_deref() else { return Place::Text };
    let Some(file_lines) = lines.get(path) else { return Place::Text };
    match finding.line {
        Some(line) if file_lines.contains(&line) => Place::Line { path: path.to_string(), line },
        _ => Place::File { path: path.to_string() },
    }
}

/// What marks a comment as the one a finding was posted as.
fn marker(finding: &ReviewFinding) -> String {
    format!("<!-- tod-finding:{} -->", finding.id)
}

fn comment_body(finding: &ReviewFinding, place: &Place) -> String {
    let mut out = format!("**[{}]** {}", finding.severity, finding.summary.trim());
    if let (Place::File { .. }, Some(line)) = (place, finding.line) {
        out = format!("**[{}]** (line {line}) {}", finding.severity, finding.summary.trim());
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
        assert_eq!(place(&finding(Some("src/other.rs"), Some(1)), &lines), Place::Text);
        assert_eq!(place(&finding(None, None), &lines), Place::Text);
    }

    #[test]
    fn a_comment_carries_the_marker_its_thread_is_found_by() {
        let f = finding(Some("src/a.rs"), Some(9));
        let body = comment_body(&f, &Place::File { path: "src/a.rs".into() });
        assert!(body.contains(&marker(&f)), "{body}");
        assert!(body.starts_with("**[high]** (line 9) Wrong"), "{body}");
    }
}
