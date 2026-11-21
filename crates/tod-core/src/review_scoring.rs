//! Scored review findings, and where each one goes (`doc/requests/design.md`
//! section 5.1; the scores are `tod_store::review`'s).
//!
//! **What blocks.** A review stands or falls on blockers and majors, plus
//! minors in the first `review.minor_rounds` rounds of a stay (default 2). A
//! nit never blocks. Every reader asks `ReviewFinding::stands`, so the rule
//! lives in one place: a finding that does not block is settled `left`, with
//! tod's reason, the moment its round is done ([`route`], then the
//! autopilot's `settle_review_round`).
//!
//! **Who fixes** ([`Lane`]):
//! - a finding that blocks and is `mechanical` or `simple` goes, with the
//!   others like it, to ONE quick fix session and the small-change review;
//! - a finding that blocks and is `involved` goes to Fix, as before;
//! - a `nit` is fixed only when it is `mechanical` (by the quick fix),
//!   else left at once;
//! - a `minor` past the blocking rounds is left.
//!
//! A quick fix that fails passes its blocking findings to Fix; the nits it
//! could not fix are left.
//!
//! **Check suggestions** ([`rule_suggestions`]). A `mechanical` finding may
//! cite the rule it breaks (`tod-cli review add --rule`). Two findings cite
//! the same rule when their wording is equal after lowercasing, collapsing
//! whitespace and trimming surrounding punctuation
//! (`tod_store::review::rule_key`): deterministic, no model. When three
//! different nodes have findings citing one rule, a suggestion to add a check
//! for it is filed, once (the first wording cited is the title).

use anyhow::Result;
use std::collections::BTreeMap;
use tod_store::review::{
    COMPLEXITY_INVOLVED, IMPORTANCE_BLOCKER, IMPORTANCE_MAJOR, IMPORTANCE_MINOR, IMPORTANCE_NIT, ReviewFinding, ReviewRepo, rule_key,
};
use uuid::Uuid;

/// How many different nodes must have a finding citing one rule before a
/// check is suggested for it.
pub const SUGGEST_AFTER_NODES: usize = 3;

/// Where a finding goes once its review round is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lane {
    /// The one quick fix session.
    Quick,
    /// The Fix protocol.
    Fix,
    /// Set aside as `left`, for this reason.
    Leave(String),
}

/// Whether a finding counts against its review in round `round` (1-based) of
/// a stay.
pub fn blocks(finding: &ReviewFinding, round: u32, minor_rounds: u32) -> bool {
    match finding.importance.as_str() {
        IMPORTANCE_BLOCKER | IMPORTANCE_MAJOR => true,
        IMPORTANCE_MINOR => round <= minor_rounds,
        _ => false,
    }
}

/// The lane of one finding.
pub fn lane(finding: &ReviewFinding, round: u32, minor_rounds: u32) -> Lane {
    if finding.importance == IMPORTANCE_NIT {
        return if finding.complexity == tod_store::review::COMPLEXITY_MECHANICAL {
            Lane::Quick
        } else {
            Lane::Leave("nit: left as it is".to_string())
        };
    }
    if !blocks(finding, round, minor_rounds) {
        debug_assert_eq!(finding.importance, IMPORTANCE_MINOR);
        return Lane::Leave(format!("minor after round {minor_rounds}: it no longer blocks, left as it is"));
    }
    if finding.complexity == COMPLEXITY_INVOLVED { Lane::Fix } else { Lane::Quick }
}

/// A round's open findings, divided by lane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Routing {
    pub quick: Vec<ReviewFinding>,
    pub fix: Vec<ReviewFinding>,
    pub leave: Vec<(ReviewFinding, String)>,
}

/// Divides `open` findings by lane.
pub fn route(open: &[ReviewFinding], round: u32, minor_rounds: u32) -> Routing {
    let mut routing = Routing::default();
    for finding in open {
        match lane(finding, round, minor_rounds) {
            Lane::Quick => routing.quick.push(finding.clone()),
            Lane::Fix => routing.fix.push(finding.clone()),
            Lane::Leave(reason) => routing.leave.push((finding.clone(), reason)),
        }
    }
    routing
}

/// Why a nit the quick fix could not fix is left.
pub const QUICK_FIX_FAILED_NIT: &str = "nit: the quick fix could not make it, left as it is";

/// The findings, as the quick fix session is given them: the score, the
/// place, the claim and the detail, one block each.
pub fn render_for_quick_fix(findings: &[ReviewFinding]) -> String {
    let mut out = String::new();
    for f in findings {
        out.push_str(&format!(
            "- [{}] {}/{}/{}{}: {}\n",
            tod_store::interview::short_id(f.id),
            f.importance,
            f.impact,
            f.complexity,
            f.location().map(|l| format!(" at {l}")).unwrap_or_default(),
            f.summary
        ));
        if let Some(detail) = f.detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            for line in detail.lines() {
                out.push_str(&format!("    {line}\n"));
            }
        }
        if let Some(rule) = f.rule.as_deref().filter(|r| !r.trim().is_empty()) {
            out.push_str(&format!("    Rule: {rule}\n"));
        }
    }
    out
}

/// The finding ids a review quick fix covers, as `QuickFix::command` holds
/// them (one per line).
pub fn ids_text(findings: &[ReviewFinding]) -> String {
    findings.iter().map(|f| f.id.to_string()).collect::<Vec<_>>().join("\n")
}

/// The ids in [`ids_text`].
pub fn ids_from(text: &str) -> Vec<Uuid> {
    text.lines().filter_map(|l| Uuid::parse_str(l.trim()).ok()).collect()
}

/// A rule three nodes' mechanical findings cite, for which a check is
/// suggested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSuggestion {
    /// The wording first cited.
    pub rule: String,
    pub nodes: Vec<Uuid>,
    pub findings: Vec<Uuid>,
}

impl RuleSuggestion {
    pub fn title(&self) -> String {
        format!("Add a check for: {}", self.rule)
    }

    pub fn body(&self) -> String {
        format!(
            "Reviewers have reported the same mechanical finding on {} different tasks ({} findings):\n\n> {}\n\n\
             A check that runs the rule (a lint, a formatter, a script) would catch it before any reviewer reads \
             the change, and the review would no longer have to spend its attention on it.",
            self.nodes.len(),
            self.findings.len(),
            self.rule
        )
    }
}

/// The rules cited by mechanical findings on at least [`SUGGEST_AFTER_NODES`]
/// nodes, of which `node` is one, and for which no suggestion with that title
/// exists on any node yet. Reads only.
pub fn rule_suggestions(conn: &rusqlite::Connection, node: Uuid) -> Result<Vec<RuleSuggestion>> {
    let mut by_key: BTreeMap<String, RuleSuggestion> = BTreeMap::new();
    for (finding_node, finding, rule) in ReviewRepo::new(conn).mechanical_rules()? {
        let key = rule_key(&rule);
        if key.is_empty() {
            continue;
        }
        let entry = by_key.entry(key).or_insert_with(|| RuleSuggestion {
            rule: rule.split_whitespace().collect::<Vec<_>>().join(" "),
            nodes: Vec::new(),
            findings: Vec::new(),
        });
        if !entry.nodes.contains(&finding_node) {
            entry.nodes.push(finding_node);
        }
        entry.findings.push(finding);
    }
    let mut out = Vec::new();
    for suggestion in by_key.into_values() {
        if suggestion.nodes.len() < SUGGEST_AFTER_NODES || !suggestion.nodes.contains(&node) {
            continue;
        }
        let filed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM node_suggestions WHERE lower(title) = lower(?1)",
            rusqlite::params![suggestion.title()],
            |r| r.get(0),
        )?;
        if filed == 0 {
            out.push(suggestion);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::review::{NewFinding, ReviewRepo};

    fn finding(importance: &str, complexity: &str) -> ReviewFinding {
        ReviewFinding {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            seq: 1,
            importance: importance.into(),
            impact: "low".into(),
            complexity: complexity.into(),
            file: Some("a.rs".into()),
            line: Some(3),
            summary: format!("{importance} {complexity}"),
            detail: None,
            status: "open".into(),
            response: None,
            created_at: 1,
            updated_at: 1,
            pr_thread: None,
            pr_posted_at: None,
            root_cause: None,
            reviewer: None,
            rule: None,
            quick_fix: None,
        }
    }

    #[test]
    fn blockers_and_majors_always_block_and_a_nit_never_does() {
        for round in [1, 2, 5] {
            assert!(blocks(&finding("blocker", "involved"), round, 2));
            assert!(blocks(&finding("major", "simple"), round, 2));
            assert!(!blocks(&finding("nit", "mechanical"), round, 2));
        }
    }

    #[test]
    fn a_minor_blocks_only_in_the_first_rounds_of_a_stay() {
        let minor = finding("minor", "simple");
        assert!(blocks(&minor, 1, 2) && blocks(&minor, 2, 2));
        assert!(!blocks(&minor, 3, 2));
        assert!(!blocks(&minor, 1, 0), "0 rounds: a minor never blocks");
    }

    #[test]
    fn findings_are_routed_quick_fix_or_fix_or_left() {
        let all = vec![
            finding("major", "mechanical"),
            finding("blocker", "simple"),
            finding("major", "involved"),
            finding("nit", "mechanical"),
            finding("nit", "simple"),
            finding("minor", "involved"),
        ];
        let early = route(&all, 1, 2);
        assert_eq!(early.quick.len(), 3, "mechanical and simple that block, plus the mechanical nit");
        assert_eq!(early.fix.len(), 2, "the involved major and the involved minor");
        assert_eq!(early.leave.len(), 1);
        assert!(early.leave[0].1.starts_with("nit"));
        let late = route(&all, 3, 2);
        assert_eq!(late.fix.len(), 1, "a minor is not Fix's after round 2");
        assert_eq!(late.leave.len(), 2);
        assert!(late.leave.iter().any(|(_, why)| why.starts_with("minor after round 2")));
    }

    #[test]
    fn the_quick_fix_is_given_each_finding_and_its_rule() {
        let mut f = finding("major", "mechanical");
        f.detail = Some("line one\nline two".into());
        f.rule = Some("docstrings are at most three lines".into());
        let text = render_for_quick_fix(&[f.clone()]);
        assert!(text.contains("major/low/mechanical at a.rs:3"), "{text}");
        assert!(text.contains("    line two") && text.contains("Rule: docstrings"));
        assert_eq!(ids_from(&ids_text(&[f.clone(), finding("nit", "mechanical")])).len(), 2);
    }

    fn db() -> (rusqlite::Connection, std::path::PathBuf) {
        let dir = tod_test_util::scratch_dir(format!("tod-review-scoring-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = tod_store::fleet::schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        (conn, dir)
    }

    fn cite(conn: &rusqlite::Connection, rule: &str) -> Uuid {
        let node = Uuid::new_v4();
        tod_store::outline::repos::NodeRepo::new(conn).create_with_id(node, &format!("n{node}"), "N").unwrap();
        let mut new = NewFinding::scored("minor", "low", "mechanical", "too long");
        new.rule = Some(rule.into());
        ReviewRepo::new(conn).add(node, None, &new).unwrap();
        node
    }

    #[test]
    fn a_check_is_suggested_when_three_nodes_cite_the_same_rule() {
        let (conn, _dir) = db();
        let a = cite(&conn, "Docstrings are at most three lines.");
        let b = cite(&conn, "docstrings   are at most three lines");
        assert!(rule_suggestions(&conn, b).unwrap().is_empty(), "two nodes are not enough");
        let other = cite(&conn, "a different rule");
        let c = cite(&conn, "DOCSTRINGS are at most three lines");
        let got = rule_suggestions(&conn, c).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rule, "Docstrings are at most three lines.");
        assert_eq!(got[0].nodes.len(), 3);
        assert_eq!(got[0].title(), "Add a check for: Docstrings are at most three lines.");
        assert!(rule_suggestions(&conn, other).unwrap().is_empty(), "the other rule has one node");
        assert_eq!(rule_suggestions(&conn, a).unwrap().len(), 1);
        // Once a suggestion with that title exists, no other node files it again.
        let mut new = tod_store::suggestions::NewSuggestion::follow_up_ticket(c, &got[0].title(), &got[0].body(), "app");
        new.duplicates_note = "x".into();
        tod_store::suggestions::file(&conn, &new).unwrap();
        assert!(rule_suggestions(&conn, a).unwrap().is_empty());
    }
}
