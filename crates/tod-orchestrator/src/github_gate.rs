//! The orchestrator answers a GitHub wait's deadline itself
//! (`doc/cloud-sandboxes/orchestrator.md`, "Waiting on GitHub").
//!
//! A node in `approved` or `merged` waits for a pull request to be merged or
//! released, which a webhook usually announces (`crate::webhooks`). The wait
//! also has a deadline, the next scheduled look, in case a webhook was lost.
//! Poking the node's sandbox for that look would wake it only to find
//! nothing; instead a due wake passes this gate ([`GithubGate`], a
//! [`WakeGate`]), which runs the node's own check
//! (`tod_core::autopilot::check_github`) against the user's copy of the
//! database, with the orchestrator's GitHub access (a proxy rule, `TOD_GITHUB_AUTH=proxy`).
//!
//! - Still waiting: the check has recorded the next look; the wake moves to
//!   it and the sandbox stays asleep, or does not exist.
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
