//! Fix escalation (`doc/lifecycle/find-it-first.md`, items 7 and 9): the
//! autopilot sends a task whose review is not converging back to `design`
//! itself, instead of letting the fixer patch the same problem round after
//! round. The fixer is told to send a redesign back on its own
//! (`surface/fix.md`); this is the app's backstop for when it does not.
//!
//! Two triggers, both read from the finished review rounds of the node's
//! current stay ([`crate::review_progress::rounds`]), each round being both
//! reviewers' findings:
//!
//! - **One root cause, many rounds.** Findings that share a root cause
//!   (`tod_store::review::root_cause_key`) were recorded in
//!   [`ROOT_CAUSE_ROUNDS`] or more rounds: each fix answered the instance and
//!   the cause kept producing more.
//! - **Fixing makes more.** A fix round added more findings than it closed
//!   ([`GROWING_FIX_ROUNDS`] times in a row): the round after it recorded
//!   more findings than the fix answered (`fixed` or `rejected`) of the
//!   round before.
//!
//! A finding the user dismissed (out of scope, declined) does not count for
//! either. The thresholds are here and nowhere else.
//!
//! [`check`] decides; [`send_back`] makes the move through
//! `tod_store::send_back` (the loop guard applies), with the autopilot as
//! actor, the latest round's main review conversation as the session that
//! sends it, the findings as the reason and as evidence.

use crate::review_progress::Round;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use tod_store::decisions::EvidenceRef;
use tod_store::fleet::FleetStore;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::review::{FINDING_DECLINED, FINDING_FIXED, FINDING_OUT_OF_SCOPE, FINDING_REJECTED, ReviewFinding};
use uuid::Uuid;

/// Rounds of one stay that recorded findings with the same root cause before
/// the task goes back to `design`.
pub const ROOT_CAUSE_ROUNDS: usize = 3;

/// Consecutive fix rounds that each added more findings than they closed
/// before the task goes back to `design`.
pub const GROWING_FIX_ROUNDS: usize = 2;

/// Where an escalation sends the task.
pub const TARGET: &str = "design";

/// The actor an escalation is made as.
pub const ACTOR: &str = "autopilot";

/// A review that is not converging, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    /// The send-back's reason, the findings named in it.
    pub reason: String,
    /// The findings it rests on.
    pub findings: Vec<Uuid>,
    /// The latest round's main review conversation: the session the
    /// send-back is made in.
    pub conversation_id: Uuid,
}

fn stands(finding: &ReviewFinding) -> bool {
    !matches!(finding.status.as_str(), FINDING_OUT_OF_SCOPE | FINDING_DECLINED)
}

fn closed(finding: &ReviewFinding) -> bool {
    matches!(finding.status.as_str(), FINDING_FIXED | FINDING_REJECTED)
}

fn line(finding: &ReviewFinding) -> String {
    let at = match (&finding.file, finding.line) {
        (Some(file), Some(line)) => format!(" ({file}:{line})"),
        (Some(file), None) => format!(" ({file})"),
        _ => String::new(),
    };
    format!("[{}] {}{at}", short_id(finding.id), finding.summary)
}

/// Whether the rounds (oldest first) call for sending the task back to
/// `design`. The root-cause trigger is checked first.
pub fn check(rounds: &[Round]) -> Option<Escalation> {
    let latest = rounds.last()?.lead.id;

    // One root cause across many rounds.
    let mut causes: BTreeMap<String, (String, BTreeSet<usize>, Vec<&ReviewFinding>)> = BTreeMap::new();
    for (index, round) in rounds.iter().enumerate() {
        for finding in round.findings.iter().filter(|f| stands(f)) {
            let Some(cause) = finding.root_cause.as_deref().filter(|c| !c.trim().is_empty()) else {
                continue;
            };
            let entry = causes
                .entry(tod_store::review::root_cause_key(cause))
                .or_insert_with(|| (cause.trim().to_string(), BTreeSet::new(), Vec::new()));
            entry.1.insert(index);
            entry.2.push(finding);
        }
    }
    if let Some((cause, in_rounds, findings)) = causes
        .into_values()
        .filter(|(_, in_rounds, _)| in_rounds.len() >= ROOT_CAUSE_ROUNDS)
        .max_by_key(|(_, in_rounds, _)| in_rounds.len())
    {
        let reason = format!(
            "tod's autopilot sends this back to design: the review findings in this stay share the root cause \
             \"{cause}\" across {} review rounds, so fixing them one at a time is not converging and the design \
             has to change. Findings:\n{}",
            in_rounds.len(),
            findings.iter().map(|f| format!("- {}", line(f))).collect::<Vec<_>>().join("\n")
        );
        return Some(Escalation {
            reason,
            findings: findings.iter().map(|f| f.id).collect(),
            conversation_id: latest,
        });
    }

    // A fix round between round i and round i + 1 closed what it answered of
    // round i, and round i + 1 added what it recorded.
    let mut streak: Vec<(usize, usize, usize)> = Vec::new();
    for (index, pair) in rounds.windows(2).enumerate() {
        let closed_count = pair[0].findings.iter().filter(|f| stands(f) && closed(f)).count();
        let added = pair[1].findings.iter().filter(|f| stands(f)).count();
        if added > closed_count {
            streak.push((index + 1, closed_count, added));
        } else {
            streak.clear();
        }
    }
    if streak.len() >= GROWING_FIX_ROUNDS {
        let growing = &streak[streak.len() - GROWING_FIX_ROUNDS..];
        let findings: Vec<&ReviewFinding> = growing
            .iter()
            .flat_map(|(after, _, _)| rounds[*after].findings.iter().filter(|f| stands(f)))
            .collect();
        let counts = growing
            .iter()
            .map(|(after, closed_count, added)| {
                format!("the fix before round {} closed {closed_count} and that round found {added}", after + 1)
            })
            .collect::<Vec<_>>()
            .join("; ");
        let reason = format!(
            "tod's autopilot sends this back to design: {GROWING_FIX_ROUNDS} fix rounds in a row added more review \
             findings than they closed ({counts}), so the fixes are making more problems than they solve and the \
             design has to change. Findings:\n{}",
            findings.iter().map(|f| format!("- {}", line(f))).collect::<Vec<_>>().join("\n")
        );
        return Some(Escalation {
            reason,
            findings: findings.iter().map(|f| f.id).collect(),
            conversation_id: latest,
        });
    }
    None
}

/// [`check`] on the node's finished review rounds of this stay since
/// `since_ms` (`on_pr`: the review with the pull request open).
pub fn for_node(conn: &Connection, node: Uuid, since_ms: i64, on_pr: bool) -> Result<Option<Escalation>> {
    Ok(check(&crate::review_progress::rounds(conn, node, since_ms, on_pr)?))
}

/// Send the task back to [`TARGET`] for `escalation`, as [`ACTOR`]. What
/// came of it (moved, or refused by the loop guard) is the send-back row the
/// caller reads (`Autopilot::sent_back_during`).
pub fn send_back(fleet: &FleetStore, escalation: &Escalation) -> Result<()> {
    fleet.interview(
        ACTOR,
        InterviewCommand::SendBack(tod_store::send_back::Request {
            conversation_id: escalation.conversation_id,
            to: TARGET.to_string(),
            reason: escalation.reason.clone(),
            evidence: escalation
                .findings
                .iter()
                .map(|id| EvidenceRef {
                    kind: "finding".to_string(),
                    id: *id,
                })
                .collect(),
            obligations: Vec::new(),
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::conversation::{Conversation, Focus, ProtocolKind};

    fn lead() -> Conversation {
        Conversation {
            id: Uuid::new_v4(),
            focus: Focus::Node(Uuid::nil()),
            protocol: ProtocolKind::Review,
            agent_run_id: None,
            agent_session_id: None,
            session_name: None,
            platform: None,
            model: None,
            effort: None,
            from_state: Some("review".into()),
            to_state: None,
            skill_group: 0,
            reviewer: Some(tod_store::review::Reviewer::Main),
            review_round: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn finding(summary: &str, root_cause: Option<&str>, status: &str) -> ReviewFinding {
        ReviewFinding {
            id: Uuid::new_v4(),
            node_id: Uuid::nil(),
            conversation_id: None,
            seq: 1,
            importance: "blocker".into(), impact: "high".into(), complexity: "simple".into(),
            file: Some("a.py".into()),
            line: Some(3),
            summary: summary.into(),
            detail: None,
            status: status.into(),
            response: None,
            created_at: 0,
            updated_at: 0,
            pr_thread: None,
            pr_posted_at: None,
            root_cause: root_cause.map(str::to_string),
            reviewer: None,
        }
    }

    fn round(findings: Vec<ReviewFinding>) -> Round {
        Round { lead: lead(), findings }
    }

    #[test]
    fn one_root_cause_in_three_rounds_goes_back_to_design() {
        let rounds = vec![
            round(vec![finding("a", Some("Trusts the cache"), "fixed")]),
            round(vec![finding("b", Some("trusts  the cache"), "fixed"), finding("c", None, "fixed")]),
            round(vec![finding("d", Some("TRUSTS THE CACHE"), "open")]),
        ];
        let escalation = check(&rounds).expect("three rounds share a root cause");
        assert_eq!(escalation.conversation_id, rounds[2].lead.id);
        assert_eq!(escalation.findings.len(), 3);
        assert!(escalation.reason.contains("root cause \"Trusts the cache\" across 3 review rounds"), "{}", escalation.reason);
        assert!(escalation.reason.contains("] d (a.py:3)"), "{}", escalation.reason);
    }

    #[test]
    fn a_fix_that_adds_more_findings_than_it_closes_twice_goes_back_to_design() {
        let rounds = vec![
            round(vec![finding("a", None, "fixed")]),
            round(vec![finding("b", None, "fixed"), finding("c", None, "rejected")]),
            round(vec![finding("d", None, "open"), finding("e", None, "open"), finding("f", None, "open")]),
        ];
        let escalation = check(&rounds).expect("two fix rounds in a row grew the findings");
        assert_eq!(escalation.findings.len(), 5);
        assert!(escalation.reason.contains("closed 1 and that round found 2"), "{}", escalation.reason);
        assert!(escalation.reason.contains("closed 2 and that round found 3"), "{}", escalation.reason);
    }

    #[test]
    fn a_converging_review_is_left_alone() {
        // Two rounds share a cause (below the threshold); fixes close as
        // many as the next round finds; a dismissed finding does not count.
        let rounds = vec![
            round(vec![finding("a", Some("cause"), "fixed"), finding("b", None, "fixed")]),
            round(vec![finding("c", Some("cause"), "fixed"), finding("x", None, "declined")]),
            round(vec![finding("d", None, "open"), finding("y", Some("cause"), "out_of_scope")]),
        ];
        assert_eq!(check(&rounds), None);
        // Growing once, then shrinking, then growing once: never twice in a row.
        let rounds = vec![
            round(vec![finding("a", None, "fixed")]),
            round(vec![finding("b", None, "fixed"), finding("c", None, "fixed")]),
            round(vec![finding("d", None, "fixed")]),
            round(vec![finding("e", None, "open"), finding("f", None, "open")]),
        ];
        assert_eq!(check(&rounds), None);
        assert_eq!(check(&[]), None);
    }
}
