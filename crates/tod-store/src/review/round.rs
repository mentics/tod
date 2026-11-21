//! A review round: two reviewers at once (`doc/lifecycle/find-it-first.md`
//! 6h). The **main** reviewer sweeps every lens; the **risk** reviewer
//! sweeps only the lenses where defects that survive review usually live
//! ([`super::RISK_LENSES`]). Each is a review conversation of its own, in a
//! fresh session, and both record into the node's one findings list.
//!
//! The main reviewer's conversation stands for the round: it is the one every
//! "latest review" lookup returns
//! (`ConversationRepo::latest_for_focus_with_protocol` passes over a risk
//! reviewer), and the risk reviewer's conversation names it
//! (`conversations.review_round`).
//!
//! **How "the round is done" is recorded.** Each reviewer records `review
//! done` on its own conversation, refused until its own lenses and a clean
//! saturation pass after its own last finding are recorded
//! (`coverage::record_done`). There is no separate round record: the round is
//! done when its main conversation and every partner have recorded done
//! ([`done`]). A main conversation with no partner is a round whose risk
//! reviewer has not run, so it is not done; a review from before there were
//! two (no reviewer recorded) is a round on its own.

use crate::conversation::{Conversation, ConversationRepo};
use anyhow::{Context, Result};
use rusqlite::Connection;
use uuid::Uuid;

use super::{Reviewer, is_done_report};

/// The conversations of the round `lead` leads: `lead` first, then its
/// partners.
pub fn conversations(conn: &Connection, lead: Uuid) -> Result<Vec<Uuid>> {
    let mut out = vec![lead];
    out.extend(ConversationRepo::new(conn).review_partners(lead)?.into_iter().map(|c| c.id));
    Ok(out)
}

/// Whether review conversation `conversation_id` last recorded itself done.
pub fn recorded_done(conn: &Connection, conversation_id: Uuid) -> Result<bool> {
    Ok(ConversationRepo::new(conn)
        .latest_report(conversation_id)?
        .is_some_and(|report| is_done_report(&report)))
}

/// Whether the round `lead` leads is done: each of its reviewers recorded
/// done (see the module comment).
pub fn done(conn: &Connection, lead: Uuid) -> Result<bool> {
    let repo = ConversationRepo::new(conn);
    let lead_row = repo
        .get(lead)?
        .with_context(|| format!("review conversation {lead} not found"))?;
    if !recorded_done(conn, lead)? {
        return Ok(false);
    }
    let partners = repo.review_partners(lead)?;
    if lead_row.reviewer == Some(Reviewer::Main) && partners.is_empty() {
        return Ok(false);
    }
    for partner in &partners {
        if !recorded_done(conn, partner.id)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The round's lead for review conversation `conversation`: itself, or the
/// main conversation a risk reviewer names.
pub fn lead_of(conversation: &Conversation) -> Uuid {
    match (conversation.reviewer, conversation.review_round) {
        (Some(Reviewer::Risk), Some(lead)) => lead,
        _ => conversation.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{Focus, ProtocolKind};
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;
    use crate::review::coverage::{self, CoveragePass, CoverageResult, NewCoverage};

    fn setup() -> (Connection, Uuid) {
        let dir = tod_test_util::scratch_dir("tod-review-round");
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "reviewed", "Reviewed").unwrap();
        (conn, node)
    }

    fn review(conn: &Connection, node: Uuid, reviewer: Option<Reviewer>, round: Option<Uuid>) -> Uuid {
        let repo = ConversationRepo::new(conn);
        let id = repo
            .create(Focus::Node(node), ProtocolKind::Review, None, None, None)
            .unwrap()
            .id;
        if let Some(reviewer) = reviewer {
            repo.set_reviewer(id, reviewer, round).unwrap();
        }
        id
    }

    fn sweep(conn: &Connection, conversation: Uuid, slugs: &[&str]) {
        for slug in slugs {
            coverage::add(
                conn,
                conversation,
                &NewCoverage {
                    pass: CoveragePass::Lens(slug.to_string()),
                    files: vec![],
                    checked: Some("read it".into()),
                    result: CoverageResult::None,
                    read: vec![],
                },
            )
            .unwrap();
        }
        coverage::add(
            conn,
            conversation,
            &NewCoverage {
                pass: CoveragePass::Saturation,
                files: vec![],
                checked: Some("again".into()),
                result: CoverageResult::None,
                read: vec![],
            },
        )
        .unwrap();
    }

    #[test]
    fn the_risk_reviewer_owes_only_its_lenses_and_the_round_needs_both_done() {
        let (conn, node) = setup();
        let main = review(&conn, node, Some(Reviewer::Main), None);
        let risk = review(&conn, node, Some(Reviewer::Risk), Some(main));

        let required: Vec<&str> =
            coverage::required_lenses(&conn, risk).unwrap().iter().map(|l| l.slug).collect();
        assert_eq!(required, super::super::RISK_LENSES.to_vec());
        assert_eq!(coverage::required_lenses(&conn, main).unwrap().len(), super::super::LENSES.len());

        // The risk reviewer is done with its six lenses; the main one is not
        // done, so neither is the round.
        sweep(&conn, risk, &super::super::RISK_LENSES);
        coverage::record_done(&conn, risk).unwrap();
        assert!(!done(&conn, main).unwrap());
        // The main reviewer cannot get by on the risk reviewer's six.
        sweep(&conn, main, &super::super::RISK_LENSES);
        let refused = coverage::record_done(&conn, main).unwrap_err().to_string();
        assert!(refused.contains("obligations"), "{refused}");
        let all: Vec<&str> = super::super::LENSES.iter().map(|l| l.slug).collect();
        sweep(&conn, main, &all);
        coverage::record_done(&conn, main).unwrap();
        assert!(done(&conn, main).unwrap());

        assert_eq!(conversations(&conn, main).unwrap(), vec![main, risk]);
        let risk_row = ConversationRepo::new(&conn).get(risk).unwrap().unwrap();
        assert_eq!(lead_of(&risk_row), main);
        // The round's main conversation stands for it.
        let latest = ConversationRepo::new(&conn)
            .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, main);
    }

    #[test]
    fn a_main_reviewer_alone_is_not_a_done_round_but_a_legacy_review_is() {
        let (conn, node) = setup();
        let all: Vec<&str> = super::super::LENSES.iter().map(|l| l.slug).collect();
        let main = review(&conn, node, Some(Reviewer::Main), None);
        sweep(&conn, main, &all);
        coverage::record_done(&conn, main).unwrap();
        assert!(!done(&conn, main).unwrap(), "the risk reviewer has not run");

        let legacy = review(&conn, node, None, None);
        sweep(&conn, legacy, &all);
        coverage::record_done(&conn, legacy).unwrap();
        assert!(done(&conn, legacy).unwrap());
    }

    #[test]
    fn a_finding_carries_the_reviewer_that_recorded_it() {
        let (conn, node) = setup();
        let main = review(&conn, node, Some(Reviewer::Main), None);
        let risk = review(&conn, node, Some(Reviewer::Risk), Some(main));
        let repo = crate::review::ReviewRepo::new(&conn);
        let finding = |conversation| crate::review::NewFinding {
            importance: "blocker".into(), impact: "high".into(), complexity: "simple".into(),
            summary: format!("from {conversation}"),
            ..Default::default()
        };
        let a = repo.add(node, Some(main), &finding(main)).unwrap();
        let b = repo.add(node, Some(risk), &finding(risk)).unwrap();
        let c = repo.add(node, None, &finding(Uuid::nil())).unwrap();
        assert_eq!(a.reviewer, Some(Reviewer::Main));
        assert_eq!(b.reviewer, Some(Reviewer::Risk));
        assert_eq!(c.reviewer, None);
    }
}
