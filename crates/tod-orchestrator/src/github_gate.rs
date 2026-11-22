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
//! The look is `tod_core::autopilot::check_github_as` with `Look::Act`: it does
//! what tod can do through the API itself, with no sandbox: rerun a job lost
//! to infrastructure (a cancelled job, a lost runner), merge the base into a
//! branch that is behind, ask a silent review bot, link a pull request whose
//! link never arrived. Only **agent work** pokes ([`GithubCheck::Feedback`]: a
//! failing check that is not infrastructure, an unanswered thread or a person's
//! comment, a conflict, tod's own review of a head); checks still running are
//! waited for (a short poll), and the agent is woken once with everything
//! when they finish. A wake a webhook made (id `event-<node>`, see
//! `crate::webhooks`) passes the same gate, so a webhook for a cancelled job
//! or a branch update does not poke a sleeping sandbox either. A node a run
//! holds is poked as before (it is awake anyway).
//!
//! - Still waiting: the check has recorded the next look; the wake moves to
//!   it and the sandbox stays asleep, or does not exist. This includes a
//!   `pr_review` node whose pull request waits only for a person's review:
//!   the look is at the review cadence (`tod_core::wait_cadence`: minutes in
//!   the working day, hours outside it), never a sandbox wake. A bot's
//!   review, running checks, and work for the agent still poke. Being behind
//!   the base alone is not work for this: it waits like the rest, so a busy base
//!   does not wake the sandbox all day (it is merged in when anything else wakes it).
//! - Clear, or nothing to wait on: poke, the node goes on.
//! - Not readable (no GitHub access here, a missing pull request, a failed
//!   read): never a poke. The failure is recorded on the node
//!   (`look_failed_next_look`: shown, counted, on the user's list when
//!   retrying cannot help) and the wake looks again later.
//!
//! Only a wake whose id is a pending `github:` event wait is looked at;
//! every other wake is poked as it always was.

use crate::users::Users;
use crate::wakes::{Verdict, Wake, WakeGate, now_ms};
use anyhow::{Context, Result};
use std::sync::Arc;
use tod_core::autopilot::{GithubCheck, GithubLook, check_github_as, look_failed_next_look};
use tod_store::waits::WaitRepo;
use uuid::Uuid;

/// The id of the wake a GitHub webhook leaves for a node (`event-<node>`).
pub fn webhook_wake_id(node: &str) -> String {
    format!("event-{node}")
}

pub struct GithubGate {
    users: Arc<Users>,
}

impl GithubGate {
    pub fn new(users: Arc<Users>) -> Self {
        Self { users }
    }

    fn look(&self, wake: &Wake) -> Result<Verdict> {
        let Ok(node) = Uuid::parse_str(&wake.node) else {
            return Ok(Verdict::Poke);
        };
        let from_webhook = wake.id == webhook_wake_id(&wake.node);
        let data = self.users.get(&wake.user)?;
        let wait = if from_webhook {
            None
        } else {
            let Ok(wait_id) = Uuid::parse_str(&wake.id) else {
                return Ok(Verdict::Poke);
            };
            let Some(wait) = data.store.read(|conn| WaitRepo::new(conn).get(wait_id))? else {
                return Ok(Verdict::Poke);
            };
            if wait.kind != "event" || wait.state != tod_store::waits::WAIT_PENDING || !wait.match_spec.starts_with("github:") {
                return Ok(Verdict::Poke);
            }
            Some(wait)
        };
        let lifecycle = data
            .store
            .get_node(&node.to_string())?
            .map(|n| n.lifecycle)
            .context("the node is not in the user's database")?;
        // A wait of a family the node's state has no use for (it left the
        // state that recorded it) is ended here, not poked a sandbox for.
        // The same for a wait recorded in a stay the node has left, even in
        // a state that takes its spec family (the node went back and forward).
        // A webhook's wake is for a node in a state that waits on GitHub only.
        if from_webhook && !matches!(lifecycle.as_str(), "pr_draft" | "pr_review" | "approved" | "merged") {
            return Ok(Verdict::Poke);
        }
        // A node a run holds is awake: the supervisor does its own acting.
        let now = now_ms();
        let held = data
            .store
            .read(|conn| tod_store::activity::ActivityRepo::new(conn).list_for_nodes(&[node]))?
            .into_iter()
            .any(|lease| lease.kind == tod_store::activity::KIND_RUN && lease.is_live(now));
        if held {
            return Ok(Verdict::Poke);
        }
        let stay_ended = match (wait.as_ref().and_then(|w| w.stay), data.store.read(|conn| tod_store::waits::current_stay(conn, node))?) {
            (Some(recorded), Some(current)) => recorded != current,
            _ => false,
        };
        if let Some(wait) = wait.as_ref().filter(|w| {
            stay_ended || tod_store::waits::spec_states(&w.match_spec).is_some_and(|allowed| !allowed.contains(&lifecycle.as_str()))
        }) {
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
        match check_github_as(&data.store, node, &data.root, &lifecycle, GithubLook::Act)? {
            GithubCheck::Waiting { due_at_ms, what } => {
                let next: Option<tod_store::waits::Wait> = data
                    .store
                    .read(|conn| WaitRepo::new(conn).list_pending_for_node(node))?
                    .into_iter()
                    .find(|w| w.kind == "event" && w.match_spec.starts_with("github:"))
                    .map(Some)
                    .unwrap_or(None);
                // A webhook's wake with nothing recorded to look at later is done.
                let Some(next) = next else { return Ok(Verdict::Drop) };
                eprintln!("tod-orchestrator: {}'s node {node} still waits on GitHub ({what}); next look {due_at_ms}", wake.user);
                Ok(Verdict::Later { id: next.id.to_string(), at: due_at_ms })
            }
            GithubCheck::Clear | GithubCheck::NothingToWaitOn => Ok(Verdict::Poke),
            // Feedback on the pull request is the node's agent's work: wake it.
            GithubCheck::Feedback { what } => {
                eprintln!("tod-orchestrator: {}'s node {node} has feedback to address ({what}); waking its sandbox", wake.user);
                Ok(Verdict::Poke)
            }
            // Not readable (no access, a failed read): the node's sandbox
            // would read it no better, so it is not woken. The failure is
            // recorded on the node (shown, counted, on the user's list once
            // retrying cannot help) and the look is tried again later, under
            // the same wake.
            GithubCheck::Unreadable(why) => {
                eprintln!("tod-orchestrator: cannot look at GitHub for {}'s node {node}: {why}; recorded on the node, looking again later", wake.user);
                let at = look_failed_next_look(&data.store, node, lifecycle == "merged", &why, now_ms())?;
                Ok(Verdict::Later { id: wake.id.clone(), at })
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
