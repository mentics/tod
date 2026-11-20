//! The process review inside the autopilot (`doc/lifecycle/self-improvement.md`):
//! a node the daemon made for a review run (`crate::process_review`) holds the
//! lessons grouped into it, and its run starts the review session once, ahead
//! of any phase agent. The session drafts changes in the node's own checkout;
//! the lifecycle that follows is how the user reviews and approves them.
//!
//! "Once" is the existence of a process-review conversation for the node: the
//! session's single turn is the whole review, so an ended session is not
//! started again, and one cut off mid-turn is resumed by the run's saved state.

use super::*;

impl Autopilot {
    /// Runs the review session when lessons were grouped into this node and it
    /// has not run. `Some` ends the run.
    pub(super) fn process_review_step<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
    ) -> Result<Option<Outcome>> {
        let node = self.node;
        if !fleet.read(|conn| crate::process_review::owed(conn, node))? {
            return Ok(None);
        }
        hook.working_on("reviewing the process lessons");
        self.converse_group(fleet, agent, hook, ProtocolKind::ProcessReview, None, 0)
    }
}
