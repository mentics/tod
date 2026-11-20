//! The findings made on a node's pull request outside tod, for the `learn`
//! retrospective (`doc/lifecycle/find-it-first.md`, section 11).
//!
//! A review thread someone else opened, or a review a person or a bot
//! submitted, is a defect tod's own phases did not find first. The `learn`
//! agent is given each one, and records which phase should have caught it and
//! why that phase missed it. The counts per pull request (findings made
//! outside tod, findings per review round, rounds until a clean review) go
//! with them.
//!
//! This reads GitHub, so it runs only where an agent's opening is assembled
//! (off the UI thread), and never inside a store read. GitHub failing, or the
//! account tod acts as being unknown, is an error the run surfaces, never a
//! section that quietly comes back empty.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use chrono::DateTime;
use tod_store::fleet::FleetStore;
use tod_store::github::{NodePr, PrSnapshot};
use uuid::Uuid;

use crate::pr_readiness::PrFeed;

/// How many characters of a finding's text the retrospective is shown.
const SHOWN: usize = 400;

/// What the retrospective's section holds for `node`: the findings on its
/// linked pull requests that did not come from tod, and the counts. Empty
/// when the node has no linked pull request. `Err` when a pull request or the
/// account tod acts as cannot be read.
pub fn for_node(fleet: &FleetStore, feed: &dyn PrFeed, node: Uuid) -> Result<String> {
    let (links, since, rounds) = fleet.read(|conn| {
        let links = tod_store::github::NodePrRepo::new(conn).read(node)?;
        let since = tod_store::learn::LearnRepo::new(conn).current_pass_start(node)?;
        Ok((links, since, review_rounds(conn, node, since)?))
    })?;
    if links.prs.is_empty() {
        return Ok(String::new());
    }
    let own_login = feed
        .viewer_login()
        .map_err(|err| anyhow::anyhow!("could not learn which GitHub account tod posts as: {err}"))?;
    let mut snapshots = Vec::new();
    for pr in &links.prs {
        let snapshot = feed
            .snapshot(pr)
            .map_err(|err| anyhow::anyhow!("could not read {} for the findings made outside tod: {err}", pr.url))?;
        snapshots.push((pr, snapshot));
    }
    Ok(render(&own_login, &snapshots, &rounds, since))
}

/// [`for_node`] with the app's own GitHub credentials: the feed the data
/// root resolves. A node with linked pull requests and no credentials is an
/// error, not an empty section.
pub fn for_node_in(fleet: &FleetStore, data_root: &std::path::Path, node: Uuid) -> Result<String> {
    let linked = fleet.read(|conn| Ok(!tod_store::github::NodePrRepo::new(conn).read(node)?.prs.is_empty()))?;
    if !linked {
        return Ok(String::new());
    }
    let feed = crate::pr_readiness::feed_for(data_root)
        .context("no GitHub token configured (see `tod-cli secrets`), so the findings made outside tod on the node's pull request cannot be read")?;
    for_node(fleet, feed.as_ref(), node)
}

/// One of the node's own code-review rounds: how many findings it recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub findings: usize,
}

/// The node's review rounds in the current pass, oldest first: one per
/// round (its main reviewer's conversation, with the risk reviewer's folded
/// in: `tod_store::review::round`), with the findings its reviewers recorded.
fn review_rounds(conn: &rusqlite::Connection, node: Uuid, since: Option<i64>) -> Result<Vec<Round>> {
    use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
    let findings = tod_store::review::ReviewRepo::new(conn).list_for_node(node)?;
    let all: Vec<_> = ConversationRepo::new(conn)
        .list_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)?
        .into_iter()
        .map(|summary| summary.conversation)
        .collect();
    let mut leads: Vec<_> = all
        .iter()
        .filter(|c| !c.is_risk_reviewer() && since.is_none_or(|since| c.created_at > since))
        .collect();
    leads.sort_by_key(|c| c.created_at);
    Ok(leads
        .iter()
        .map(|lead| Round {
            findings: findings
                .iter()
                .filter(|f| {
                    f.conversation_id.is_some_and(|id| {
                        all.iter().any(|c| c.id == id && tod_store::review::round::lead_of(c) == lead.id)
                    })
                })
                .count(),
        })
        .collect())
}

/// A finding made outside tod.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outside {
    kind: &'static str,
    author: String,
    place: Option<String>,
    resolved: Option<bool>,
    text: String,
}

/// Whether an RFC 3339 `time` is in the pass that began at `since`. A time
/// that cannot be read counts as in the pass: nothing is hidden.
fn in_pass(time: &str, since: Option<i64>) -> bool {
    let Some(since) = since else { return true };
    DateTime::parse_from_rfc3339(time).map_or(true, |t| t.timestamp_millis() > since)
}

/// The findings of `snapshot` that tod's own account did not make: threads
/// someone else opened, and reviews someone else submitted with something to
/// say (a body, or a request for changes).
fn outside(own_login: &str, snapshot: &PrSnapshot, since: Option<i64>) -> Vec<Outside> {
    let own = |author: &str| author.eq_ignore_ascii_case(own_login);
    let mut found = Vec::new();
    for thread in &snapshot.threads {
        let Some(first) = thread.comments.first() else { continue };
        let author = first.author.clone().unwrap_or_else(|| "unknown".to_string());
        if own(&author) || !in_pass(&first.created_at, since) {
            continue;
        }
        found.push(Outside {
            kind: "review thread",
            author,
            place: thread.path.as_ref().map(|p| match thread.line {
                Some(line) => format!("{p}:{line}"),
                None => p.clone(),
            }),
            resolved: Some(thread.resolved),
            text: first.body.clone(),
        });
    }
    for review in &snapshot.reviews {
        let author = review.author.clone().unwrap_or_else(|| "unknown".to_string());
        let says_something = !review.body.trim().is_empty() || review.state == "CHANGES_REQUESTED";
        if own(&author) || !says_something || review.state == "PENDING" || !in_pass(&review.submitted_at, since) {
            continue;
        }
        found.push(Outside {
            kind: "review",
            author,
            place: None,
            resolved: None,
            text: if review.body.trim().is_empty() { review.state.clone() } else { review.body.clone() },
        });
    }
    found
}

fn render(own_login: &str, snapshots: &[(&NodePr, PrSnapshot)], rounds: &[Round], since: Option<i64>) -> String {
    let mut out = String::from("\n### Findings made outside tod\n\n");
    out.push_str(
        "Review threads and reviews on the pull request that tod's own review did not make: \
         a defect no phase found first. For each, say which phase should have caught it and why \
         that phase missed it.\n",
    );
    for (pr, snapshot) in snapshots {
        let found = outside(own_login, snapshot, since);
        let threads = found.iter().filter(|f| f.kind == "review thread").count();
        let _ = writeln!(
            out,
            "\n{}: {} finding(s) made outside tod ({threads} review thread(s), {} review(s)).",
            pr.url,
            found.len(),
            found.len() - threads
        );
        for f in &found {
            let place = f.place.as_deref().map(|p| format!(" at {p}")).unwrap_or_default();
            let state = match f.resolved {
                Some(true) => ", resolved",
                Some(false) => ", open",
                None => "",
            };
            let _ = writeln!(out, "- {} by {}{place}{state}: {}", f.kind, f.author, shorten(&f.text));
        }
    }
    let counts: Vec<String> = rounds.iter().map(|r| r.findings.to_string()).collect();
    if rounds.is_empty() {
        out.push_str("\ntod's own code review: no round recorded in this pass.\n");
    } else {
        let clean = rounds.iter().position(|r| r.findings == 0).map(|i| i + 1);
        let _ = writeln!(
            out,
            "\ntod's own code review: {} round(s), findings per round: {}; rounds until a clean review: {}.",
            rounds.len(),
            counts.join(", "),
            clean.map_or("none was clean".to_string(), |n| n.to_string()),
        );
    }
    out
}

/// `text` on one line, cut to [`SHOWN`] characters.
fn shorten(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SHOWN {
        return line;
    }
    let cut: String = line.chars().take(SHOWN).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tod_store::github::{PrReview, PrStatus, ReviewThread, ThreadComment};

    fn comment(author: &str, body: &str) -> ThreadComment {
        ThreadComment {
            id: 1,
            author: Some(author.into()),
            body: body.into(),
            created_at: "2026-11-18T10:00:00Z".into(),
            diff_hunk: None,
        }
    }

    fn thread(author: &str, body: &str, resolved: bool) -> ReviewThread {
        ReviewThread {
            id: format!("t-{author}-{body}"),
            resolved,
            outdated: false,
            path: Some("src/a.rs".into()),
            line: Some(7),
            comments: vec![comment(author, body)],
        }
    }

    fn review(author: &str, state: &str, body: &str) -> PrReview {
        PrReview {
            author: Some(author.into()),
            submitted_at: "2026-11-18T11:00:00Z".into(),
            state: state.into(),
            body: body.into(),
        }
    }

    fn snapshot() -> PrSnapshot {
        PrSnapshot {
            status: PrStatus::default(),
            threads: vec![
                thread("tod-user", "tod's own finding", false),
                thread("greptile-apps[bot]", "P1: the 404 is retried forever", true),
                thread("alice", "why not cache this?", false),
            ],
            comments: vec![],
            reviews: vec![
                review("tod-user", "COMMENTED", "own review body"),
                review("greptile-apps[bot]", "COMMENTED", "Confidence 3/5"),
                review("alice", "APPROVED", ""),
                review("bob", "CHANGES_REQUESTED", ""),
            ],
        }
    }

    #[test]
    fn only_findings_not_made_by_tods_account_are_listed_and_counted() {
        let pr = NodePr::new("o", "r", 1);
        let text = render(
            "TOD-user",
            &[(&pr, snapshot())],
            &[Round { findings: 3 }, Round { findings: 1 }, Round { findings: 0 }],
            None,
        );
        assert!(text.contains("4 finding(s) made outside tod (2 review thread(s), 2 review(s))"), "{text}");
        assert!(text.contains("P1: the 404 is retried forever") && text.contains("resolved"), "{text}");
        assert!(text.contains("why not cache this?"), "{text}");
        assert!(text.contains("Confidence 3/5"), "{text}");
        assert!(text.contains("review by bob: CHANGES_REQUESTED"), "{text}");
        assert!(!text.contains("tod's own finding") && !text.contains("own review body"), "{text}");
        assert!(!text.contains("review by alice"), "an approval with no words is no finding: {text}");
        assert!(text.contains("3 round(s), findings per round: 3, 1, 0; rounds until a clean review: 3"), "{text}");
    }

    #[test]
    fn an_earlier_pass_is_not_counted() {
        let pr = NodePr::new("o", "r", 1);
        let since = DateTime::parse_from_rfc3339("2026-11-18T10:30:00Z").unwrap().timestamp_millis();
        let text = render("tod-user", &[(&pr, snapshot())], &[], Some(since));
        // The threads are from 10:00 (before), the reviews from 11:00 (after).
        assert!(text.contains("2 finding(s) made outside tod (0 review thread(s), 2 review(s))"), "{text}");
        assert!(text.contains("no round recorded in this pass"), "{text}");
    }

    struct Feed {
        login: Result<String, String>,
        snapshot: Result<PrSnapshot, String>,
    }

    impl PrFeed for Feed {
        fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
            self.snapshot.clone()
        }
        fn viewer_login(&self) -> Result<String, String> {
            self.login.clone()
        }
    }

    fn linked_fixture() -> crate::interview::test_support::Fixture {
        use tod_store::fleet::FleetMutation;
        use tod_store::outline::{Capability, OutlineMutation};
        let fx = crate::interview::test_support::fixture();
        fx.fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: fx.node,
                capabilities: vec![Capability::Ticket],
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet
            .enqueue(FleetMutation::UpdateTaskLinkedPrs {
                id: fx.node.to_string(),
                linked_prs: vec!["https://github.com/o/r/pull/1".into()],
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet.reload_if_stale().ok();
        fx
    }

    #[test]
    fn a_node_with_no_pull_request_has_no_section() {
        let fx = crate::interview::test_support::fixture();
        let feed = Feed { login: Err("never asked".into()), snapshot: Err("never read".into()) };
        assert_eq!(for_node(&fx.fleet, &feed, fx.node).unwrap(), "");
    }

    #[test]
    fn the_feed_gives_the_section_and_its_failures_are_errors() {
        let fx = linked_fixture();
        let ok = Feed { login: Ok("tod-user".into()), snapshot: Ok(snapshot()) };
        let text = for_node(&fx.fleet, &ok, fx.node).unwrap();
        assert!(text.contains("https://github.com/o/r/pull/1: 4 finding(s)"), "{text}");

        let unreadable = Feed { login: Ok("tod-user".into()), snapshot: Err("HTTP 502".into()) };
        let err = for_node(&fx.fleet, &unreadable, fx.node).unwrap_err().to_string();
        assert!(err.contains("HTTP 502") && err.contains("pull/1"), "{err}");

        let no_login = Feed { login: Err("bad credentials".into()), snapshot: Ok(snapshot()) };
        let err = for_node(&fx.fleet, &no_login, fx.node).unwrap_err().to_string();
        assert!(err.contains("bad credentials"), "{err}");
    }

    #[test]
    fn the_learn_opening_carries_the_section_through_the_feed_override() {
        let fx = linked_fixture();
        crate::pr_readiness::set_feed_override(Some(Arc::new(Feed {
            login: Ok("tod-user".into()),
            snapshot: Ok(snapshot()),
        })));
        let text = for_node_in(&fx.fleet, &fx.root, fx.node);
        crate::pr_readiness::set_feed_override(None);
        assert!(text.unwrap().contains("### Findings made outside tod"));
    }
}
