//! Which step moves a node along its lifecycle next: the one the lifecycle
//! panel and the conversation view both show as the primary button.
//!
//! Decided from what is stored — plan-step statuses, verification's verdicts,
//! review findings — never from what the UI remembers doing. A change to the
//! code after verification (a fix, a reimplementation) reopens the verdicts it
//! invalidates (`PlanStepRepo::reopen_verification`), so "verify again" shows
//! up here as unchecked work, the same as never having verified.
//!
//! The gate check is only ever the recommendation once nothing earlier is
//! owed: running it before then only reports what this already knows.

use crate::conversation::review::review_recorded_done;
use anyhow::Result;
use rusqlite::Connection;
use tod_store::outline::repos::PlanStepRepo;
use tod_store::outline::repos::plan_steps::{
    STATUS_FAILED, STATUS_IMPLEMENTED, STATUS_VERIFIED, needs_user,
};
use tod_store::review::ReviewRepo;
use tod_store::verification::VerdictRepo;
use uuid::Uuid;

/// Where a node's work stands, as far as choosing its next step goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Standing {
    pub lifecycle: String,
    pub plan_steps: usize,
    /// Plan steps implementation has not closed yet (not `implemented`,
    /// `verified`, or handed back to the user).
    pub steps_open: usize,
    /// Plan steps handed back to the user (`partial` / `blocked`).
    pub steps_need_user: usize,
    /// Plan steps verification has not ruled on: neither `verified` nor
    /// `failed` (including ones reopened since).
    pub steps_unchecked: usize,
    pub steps_failed: usize,
    /// Own obligations with no current verdict (never checked, or reopened).
    pub obligations_unchecked: usize,
    pub obligations_failed: usize,
    /// In `review`: the review recorded itself done.
    pub review_done: bool,
    /// Review findings nobody has answered.
    pub open_findings: usize,
}

/// The recommended next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    /// Implementation has open plan steps.
    Implement,
    /// Verification owes a verdict: something was never checked, or the code
    /// changed since it was.
    Verify,
    /// Verification is complete and failed: back to `active` to fix it.
    FixFailed,
    /// The code review has not finished.
    Review,
    /// The review's findings need answers.
    Fix,
    /// Nothing earlier is owed: check the gate (or advance, once it passed).
    GateCheck,
}

impl Standing {
    /// Read `node_id`'s standing from the store. `lifecycle` is its current
    /// state.
    pub fn load(conn: &Connection, node_id: Uuid, lifecycle: &str) -> Result<Self> {
        let steps = PlanStepRepo::new(conn).list_for_node(node_id)?;
        let standings = VerdictRepo::new(conn).standings(node_id)?;
        let reviewing = lifecycle == "review";
        let open_findings = if reviewing {
            ReviewRepo::new(conn)
                .list_for_node(node_id)?
                .iter()
                .filter(|f| f.is_open())
                .count()
        } else {
            0
        };
        Ok(Self {
            lifecycle: lifecycle.to_string(),
            plan_steps: steps.len(),
            steps_open: steps
                .iter()
                .filter(|s| {
                    !matches!(s.status.as_str(), STATUS_IMPLEMENTED | STATUS_VERIFIED)
                        && !needs_user(&s.status)
                })
                .count(),
            steps_need_user: steps.iter().filter(|s| needs_user(&s.status)).count(),
            steps_unchecked: steps
                .iter()
                .filter(|s| s.status != STATUS_VERIFIED && s.status != STATUS_FAILED)
                .count(),
            steps_failed: steps.iter().filter(|s| s.status == STATUS_FAILED).count(),
            obligations_unchecked: standings.iter().filter(|s| s.is_unchecked()).count(),
            obligations_failed: standings.iter().filter(|s| s.is_failed()).count(),
            review_done: reviewing && review_recorded_done(conn, node_id)?,
            open_findings,
        })
    }

    /// What verification still owes a verdict on.
    pub fn unverified(&self) -> usize {
        self.steps_unchecked + self.obligations_unchecked
    }

    /// Whether verification has to run (again) before the gate can pass.
    pub fn verification_due(&self) -> bool {
        self.lifecycle == "verifying"
            && (self.unverified() > 0
                // A failed obligation with no failed step to carry it back
                // is verification's to finish, not implementation's.
                || (self.obligations_failed > 0 && self.steps_failed == 0))
    }
}

/// The step to recommend for `standing`. `None` when there is nothing the app
/// can recommend: a node with no plan to implement, or steps waiting on the
/// user.
pub fn next_step(standing: &Standing) -> Option<NextStep> {
    match standing.lifecycle.as_str() {
        "active" if standing.plan_steps == 0 => None,
        "active" if standing.steps_open > 0 => Some(NextStep::Implement),
        "active" if standing.steps_need_user > 0 => None,
        "verifying" if standing.plan_steps == 0 && standing.obligations_unchecked == 0 => {
            Some(NextStep::GateCheck)
        }
        "verifying" if standing.verification_due() => Some(NextStep::Verify),
        "verifying" if standing.steps_failed > 0 => Some(NextStep::FixFailed),
        "review" if !standing.review_done => Some(NextStep::Review),
        "review" if standing.open_findings > 0 => Some(NextStep::Fix),
        _ => Some(NextStep::GateCheck),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::schema;
    use tod_store::outline::repos::{NodeRepo, ObligationRepo};

    fn standing(lifecycle: &str) -> Standing {
        Standing {
            lifecycle: lifecycle.to_string(),
            plan_steps: 2,
            ..Standing::default()
        }
    }

    #[test]
    fn active_implements_until_the_plan_is_done() {
        let mut s = standing("active");
        s.steps_open = 1;
        assert_eq!(next_step(&s), Some(NextStep::Implement));
        s.steps_open = 0;
        assert_eq!(next_step(&s), Some(NextStep::GateCheck));
        s.steps_need_user = 1;
        assert_eq!(next_step(&s), None);
        assert_eq!(
            next_step(&Standing {
                plan_steps: 0,
                ..standing("active")
            }),
            None
        );
    }

    #[test]
    fn verifying_recommends_the_gate_only_once_everything_is_verified() {
        let mut s = standing("verifying");
        s.steps_unchecked = 2;
        s.obligations_unchecked = 1;
        assert_eq!(next_step(&s), Some(NextStep::Verify));
        s.steps_unchecked = 0;
        // One obligation still owed a verdict: verify, not the gate.
        assert_eq!(next_step(&s), Some(NextStep::Verify));
        s.obligations_unchecked = 0;
        assert_eq!(next_step(&s), Some(NextStep::GateCheck));
    }

    /// What the fix reported by users came down to: after a gate check's
    /// finding was fixed, the verdicts earned against the old code are
    /// reopened, and verification — not the gate check — comes next.
    #[test]
    fn a_reopened_verification_recommends_verify_over_the_gate() {
        let mut s = standing("verifying");
        assert_eq!(next_step(&s), Some(NextStep::GateCheck));
        // A fix reopened both steps and the obligation.
        s.steps_unchecked = 2;
        s.obligations_unchecked = 1;
        assert!(s.verification_due());
        assert_eq!(next_step(&s), Some(NextStep::Verify));
    }

    #[test]
    fn a_finished_verification_that_failed_goes_back_to_fix_it() {
        let mut s = standing("verifying");
        s.steps_failed = 1;
        s.obligations_failed = 1;
        assert_eq!(next_step(&s), Some(NextStep::FixFailed));
        // ...but not while anything is still unchecked.
        s.steps_unchecked = 1;
        assert_eq!(next_step(&s), Some(NextStep::Verify));
    }

    #[test]
    fn a_failed_obligation_with_no_failed_step_is_verifications_to_finish() {
        let mut s = standing("verifying");
        s.obligations_failed = 1;
        assert_eq!(next_step(&s), Some(NextStep::Verify));
    }

    #[test]
    fn a_node_without_a_plan_verifies_its_obligations_then_checks_the_gate() {
        let mut s = Standing {
            plan_steps: 0,
            ..standing("verifying")
        };
        assert_eq!(next_step(&s), Some(NextStep::GateCheck));
        s.obligations_unchecked = 1;
        assert_eq!(next_step(&s), Some(NextStep::Verify));
    }

    #[test]
    fn review_then_fix_then_the_gate() {
        let mut s = standing("review");
        assert_eq!(next_step(&s), Some(NextStep::Review));
        s.review_done = true;
        s.open_findings = 1;
        assert_eq!(next_step(&s), Some(NextStep::Fix));
        s.open_findings = 0;
        assert_eq!(next_step(&s), Some(NextStep::GateCheck));
    }

    /// The standing comes from the store: a fix reopening verification turns
    /// a node ready for its gate into one that needs verifying.
    #[test]
    fn loads_from_the_store_and_follows_a_reopened_verification() {
        let dir = std::env::temp_dir().join(format!("tod-lifecycle-next-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "fixed", "Fixed")
            .unwrap();
        let obligation = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(
                obligation,
                node,
                "requirement",
                usize::MAX,
                None,
                "Works",
                "requirements",
            )
            .unwrap();
        let steps = PlanStepRepo::new(&conn);
        let step = Uuid::new_v4();
        steps.insert_at(step, node, 0, "Build it").unwrap();
        steps
            .update_status(step, STATUS_VERIFIED, None, None)
            .unwrap();
        VerdictRepo::new(&conn)
            .record(node, obligation, None, "verified", "Saw it work.")
            .unwrap();

        let before = Standing::load(&conn, node, "verifying").unwrap();
        assert_eq!(before.unverified(), 0);
        assert_eq!(next_step(&before), Some(NextStep::GateCheck));

        steps
            .reopen_verification(node, "A fix changed it.")
            .unwrap();
        let after = Standing::load(&conn, node, "verifying").unwrap();
        assert_eq!((after.steps_unchecked, after.obligations_unchecked), (1, 1));
        assert_eq!(next_step(&after), Some(NextStep::Verify));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
