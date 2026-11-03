//! The orchestrator answers a GitHub wait's deadline itself
//! (`doc/cloud-sandboxes/orchestrator.md`, "Waiting on GitHub").
//!
//! A node in `pr_draft`, `pr_review`, `approved` or `merged` waits for a pull request to be merged or
//! released, which a webhook usually announces (`crate::webhooks`). The wait
//! also has a deadline, the next scheduled look, in case a webhook was lost.
//! Poking the node's sandbox for that look would wake it only to find
//! nothing; instead a due wake passes this gate ([`GithubGate`], a
//! [`WakeGate`]), which runs the node's own check
//! (`tod_core::autopilot::check_github`) against the user's copy of the
//! database, with the orchestrator's GitHub access (a proxy rule, `TOD_GITHUB_AUTH=proxy`).
//!
//! - Still waiting: the check has recorded the next look; the wake moves to
//!   it and the sandbox stays asleep, or does not exist. This includes a
//!   `pr_review` node whose pull request waits only for a person's review:
//!   the look is at the review cadence (`tod_core::wait_cadence`: minutes in
//!   the working day, hours outside it), never a sandbox wake. A bot's
//!   review, running checks, and work for the agent still poke.
//! - Clear, or nothing to wait on: poke, the node goes on.
//! - Not readable (no GitHub access here, a failed read): poke, as before,
//!   and say why on stderr; the sandbox's own check then reads GitHub with
//!   the user's token.
//!
//! Only a wake whose id is a pending `github:` event wait is looked at;
//! every other wake is poked as it always was.

use crate::users::Users;
use crate::wakes::{Verdict, Wake, WakeGate};
use anyhow::{Context, Result};
use std::sync::Arc;
use tod_core::autopilot::{GithubCheck, check_github};
use tod_store::waits::WaitRepo;
use uuid::Uuid;

pub struct GithubGate {
    users: Arc<Users>,
}

impl GithubGate {
    pub fn new(users: Arc<Users>) -> Self {
        Self { users }
    }

    fn look(&self, wake: &Wake) -> Result<Verdict> {
        let (Ok(wait_id), Ok(node)) = (Uuid::parse_str(&wake.id), Uuid::parse_str(&wake.node)) else {
            return Ok(Verdict::Poke);
        };
        let data = self.users.get(&wake.user)?;
        let Some(wait) = data.store.read(|conn| WaitRepo::new(conn).get(wait_id))? else {
            return Ok(Verdict::Poke);
        };
        if wait.kind != "event" || wait.state != tod_store::waits::WAIT_PENDING || !wait.match_spec.starts_with("github:") {
            return Ok(Verdict::Poke);
        }
        let lifecycle = data
            .store
            .get_node(&node.to_string())?
            .map(|n| n.lifecycle)
            .context("the node is not in the user's database")?;
        // A wait of a family the node's state has no use for (it left the
        // state that recorded it) is ended here, not poked a sandbox for.
        // The same for a wait recorded in a stay the node has left, even in
        // a state that takes its spec family (the node went back and forward).
        let stay_ended = match (wait.stay, data.store.read(|conn| tod_store::waits::current_stay(conn, node))?) {
            (Some(recorded), Some(current)) => recorded != current,
            _ => false,
        };
        if stay_ended
            || tod_store::waits::spec_states(&wait.match_spec).is_some_and(|allowed| !allowed.contains(&lifecycle.as_str()))
        {
            data.store
                .interview(
                    tod_store::waits::LIFECYCLE_ACTOR,
                    tod_store::interview::InterviewCommand::SweepWaits { expire_grace_ms: None },
                )
                .map_err(|e| anyhow::anyhow!("end the wait that no longer belongs to {lifecycle}: {e}"))?;
            eprintln!(
                "tod-orchestrator: {}'s node {node}: ended wait `{}` that does not belong to {lifecycle} or its stay",
                wake.user, wait.match_spec
            );
            return Ok(Verdict::Drop);
        }
        match check_github(&data.store, node, &data.root, &lifecycle)? {
            GithubCheck::Waiting { due_at_ms, what } => {
                let next = data
                    .store
                    .read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?
                    .into_iter()
                    .find(|w| w.kind == "event" && w.match_spec.starts_with("github:"))
                    .context("the check recorded no wait")?;
                eprintln!("tod-orchestrator: {}'s node {node} still waits on GitHub ({what}); next look {due_at_ms}", wake.user);
                Ok(Verdict::Later { id: next.id.to_string(), at: due_at_ms })
            }
            GithubCheck::Clear | GithubCheck::NothingToWaitOn => Ok(Verdict::Poke),
            // Feedback on the pull request is the node's agent's work: wake it.
            GithubCheck::Feedback { what } => {
                eprintln!("tod-orchestrator: {}'s node {node} has feedback to address ({what}); waking its sandbox", wake.user);
                Ok(Verdict::Poke)
            }
            GithubCheck::Unreadable(why) => {
                eprintln!("tod-orchestrator: cannot look at GitHub for {}'s node {node}: {why}; waking its sandbox", wake.user);
                Ok(Verdict::Poke)
            }
        }
    }
}

impl WakeGate for GithubGate {
    fn check(&self, wake: &Wake) -> Verdict {
        self.look(wake).unwrap_or_else(|err| {
            eprintln!("tod-orchestrator: GitHub gate for wake {}: {err:#}; waking its sandbox", wake.id);
            Verdict::Poke
        })
    }
}
