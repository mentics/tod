//! Whether a review that has used up its round limit is still getting
//! somewhere. A round limit that only counts rounds stops a review whose every
//! round finds one or two new, real problems the fixer fixes (ENG-2504: five
//! rounds, no finding repeated) exactly as it stops a fixer that keeps undoing
//! its own work. So reaching `lifecycle.review_round_limit` stops the run only
//! when the rounds repeat themselves: a finding of one round came back in
//! another (the fix did not hold, or the reviewer goes around in a circle).
//! While every finding of every round is new, the review may go on, up to
//! [`HARD_LIMIT_FACTOR`] times the limit, which always stops it.
//!
//! [`limit_reached`] is the one decision, used by every site that counts
//! review rounds (`review`, `pr_draft`'s review of a change, `pr_draft`'s
//! review of a pushed head), so they cannot drift.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeSet;
use tod_store::conversation::{Conversation, ConversationRepo, Focus, ProtocolKind};
use tod_store::phase::PhaseRepo;
use tod_store::review::{ReviewFinding, ReviewRepo};
use uuid::Uuid;

/// The most rounds a review gets while it keeps finding only new problems, as
/// a multiple of the configured round limit. A hard stop.
pub const HARD_LIMIT_FACTOR: u32 = 2;

/// What makes two findings the same problem: the file and the sentence,
/// compared without case, punctuation or spacing. The line is left out, as it
/// moves when code above it changes.
fn key(file: Option<&str>, summary: &str) -> String {
    let words: Vec<String> = summary
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    format!("{}|{}", file.unwrap_or("").to_lowercase(), words.join(" "))
}

/// Whether the rounds (each the keys of that round's findings, oldest first)
/// are all new: no finding of one round came back in another.
pub fn all_new(rounds: &[Vec<String>]) -> bool {
    let mut seen: BTreeSet<&String> = BTreeSet::new();
    for round in rounds {
        for k in round {
            if seen.contains(k) {
                return false;
            }
        }
        seen.extend(round.iter());
    }
    true
}

/// Whether a review that has had `rounds` rounds has used them up: always at
/// [`HARD_LIMIT_FACTOR`] times `limit`; from `limit` on only when a finding
/// repeated (`finding_rounds` from [`finding_rounds`]).
pub fn limit_reached(rounds: u32, limit: u32, finding_rounds: &[Vec<String>]) -> bool {
    let limit = limit.max(1);
    rounds >= limit.saturating_mul(HARD_LIMIT_FACTOR) || (rounds >= limit && !all_new(finding_rounds))
}

/// One finished review round: its main conversation (the round's lead,
/// `tod_store::review::round`) and every finding its reviewers recorded, in
/// the order recorded.
#[derive(Debug, Clone)]
pub struct Round {
    pub lead: Conversation,
    pub findings: Vec<ReviewFinding>,
}

/// Each finished review round in the node's current stay in the review state
/// (with the pull request open, [`crate::pr_draft_review::STATES`], when
/// `on_pr`, else tod's `review`), started at or after `since_ms`, oldest
/// first. A round whose reviewers did not both finish is skipped (it is no
/// round). Every site that reads a review's rounds reads them here.
pub fn rounds(conn: &Connection, node: Uuid, since_ms: i64, on_pr: bool) -> Result<Vec<Round>> {
    let stay = PhaseRepo::new(conn).stay_started_at(node)?;
    let all = ConversationRepo::new(conn).conversations_for_focus(Focus::Node(node))?;
    let mut leads: Vec<&Conversation> = all
        .iter()
        .filter(|c| {
            c.protocol == ProtocolKind::Review
                && c.skill_group == 0
                && !c.is_risk_reviewer()
                && c.from_state.as_deref().is_some_and(crate::pr_draft_review::reviews_on_pr) == on_pr
                && stay.is_none_or(|s| c.created_at >= s)
                && c.created_at >= since_ms
        })
        .collect();
    leads.sort_by_key(|c| c.created_at);
    let findings = ReviewRepo::new(conn).list_for_node(node)?;
    let mut out = Vec::new();
    for lead in leads {
        if !tod_store::review::round::done(conn, lead.id)? {
            continue;
        }
        let members = tod_store::review::round::conversations(conn, lead.id)?;
        out.push(Round {
            lead: lead.clone(),
            findings: findings
                .iter()
                .filter(|f| f.conversation_id.is_some_and(|c| members.contains(&c)))
                .cloned()
                .collect(),
        });
    }
    Ok(out)
}

/// The findings of each finished review round ([`rounds`]), as keys. A
/// finding that was dismissed (out of scope, declined) is left out: it does
/// not stand.
pub fn finding_rounds(conn: &Connection, node: Uuid, since_ms: i64, on_pr: bool) -> Result<Vec<Vec<String>>> {
    Ok(rounds(conn, node, since_ms, on_pr)?
        .into_iter()
        .map(|round| {
            round
                .findings
                .iter()
                .filter(|f| f.stands())
                .map(|f| key(f.file.as_deref(), &f.summary))
                .collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| key(None, s)).collect()
    }

    #[test]
    fn distinct_findings_in_every_round_are_all_new() {
        assert!(all_new(&[round(&["a"]), round(&["b", "c"]), round(&["d"])]));
    }

    #[test]
    fn a_finding_that_comes_back_is_not_new() {
        assert!(!all_new(&[round(&["Tag key is wrong."]), round(&["b"]), round(&["tag key is wrong"])]));
    }

    #[test]
    fn the_limit_holds_for_a_review_that_repeats_and_stretches_for_one_that_does_not() {
        let distinct: Vec<_> = (0..10).map(|n| round(&[&format!("d{n}")])).collect();
        assert!(!limit_reached(4, 5, &distinct[..4]), "below the limit");
        assert!(!limit_reached(5, 5, &distinct[..5]), "all new: goes on");
        assert!(!limit_reached(9, 5, &distinct[..9]), "all new: goes on");
        assert!(limit_reached(10, 5, &distinct), "the hard stop");
        let repeating = vec![round(&["x"]), round(&["y"]), round(&["x"])];
        assert!(limit_reached(5, 5, &repeating), "a repeat stops at the limit");
        assert!(!limit_reached(4, 5, &repeating), "but never before it");
    }
}
