//! The blocking jobs behind the task panel's runner menu for a node that runs
//! in a cloud sandbox, and behind `tod-cli runner` (`runner_operator`): Start,
//! the requests to a supervisor, Reset environment, Delete sandbox, Adopt,
//! Sync now. Each returns the sentence the runner line shows (`Ok`) or the
//! failure with what to do next (`Err`, see [`describe`]); none touches a
//! window or a thread, so the panel runs them on its background executor and
//! the CLI runs them where it stands.

use crate::cloud_sync::{self, CloudNode};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// A failure as the panel shows it: the title on the first line, then each
/// cause of the error on a line of its own, and always what to do next. A
/// cause that already says ("What to do: ...", as the sandbox maker's do) is
/// left to; otherwise a temporary failure (the network, Blaxel not ready) says
/// to try again, and any other says where to look.
pub fn describe(title: &str, err: &anyhow::Error) -> String {
    let mut out = title.to_string();
    for cause in err.chain() {
        out.push('\n');
        out.push_str(&cause.to_string());
    }
    if !out.contains(tod_sandbox::health::WHAT_TO_DO) {
        out.push('\n');
        out.push_str(&next_step(tod_sandbox::blaxel::error_is_transient(err)));
    }
    out
}

/// What the user does next after a failure nothing more specific explained.
fn next_step(transient: bool) -> String {
    if transient {
        "What to do: this looks temporary (the network or Blaxel). Try again in a minute.".to_string()
    } else {
        "What to do: try again. If it fails the same way, the log panel has the full detail; send it to whoever maintains tod.".to_string()
    }
}

/// A job's result: the sentence for the line, or the failure text.
pub type JobResult = Result<String, String>;

/// Delete `node_id`'s sandbox in Blaxel, keeping its uncommitted work first;
/// the node's record of it is forgotten.
pub fn delete_sandbox(fleet: &FleetStore, node_id: &str) -> JobResult {
    cloud_sync::lost::delete_node_sandbox(fleet, node_id).map_err(|err| describe("Delete sandbox failed", &err))
}

/// [`delete_sandbox`] without the checkpoint first
/// (`cloud_sync::lost::delete_node_sandbox_anyway`): uncommitted work in the
/// sandbox is lost, so the caller offers it only beside the ordinary delete.
pub fn delete_sandbox_anyway(fleet: &FleetStore, node_id: &str) -> JobResult {
    cloud_sync::lost::delete_node_sandbox_anyway(fleet, node_id).map_err(|err| describe("Delete sandbox failed", &err))
}

/// "Stop and delete": the node's runner is stopped first, then its sandbox is
/// deleted (`cloud_sync::lost::stop_and_delete_node_sandbox`), with the
/// checkpoint unless `checkpoint` is false.
pub fn stop_and_delete_sandbox(
    fleet: &FleetStore,
    node_id: &str,
    checkpoint: bool,
    progress: &mut dyn FnMut(&str),
) -> JobResult {
    let mut log = |msg: &str| {
        cloud_sync::log_progress(&format!("stop and delete {node_id}"), msg);
        progress(msg);
    };
    cloud_sync::lost::stop_and_delete_node_sandbox(fleet, node_id, checkpoint, &mut log)
        .map_err(|err| describe("Stop and delete failed", &err))
}

/// Adopt the sandbox an older build started `node_id` in
/// (`cloud_sync::adopt::adopt_node_sandbox`): it is checked to be the node's
/// and to answer, then recorded as its Files location.
pub fn adopt_sandbox(fleet: &FleetStore, node_id: &str, progress: &mut dyn FnMut(&str)) -> JobResult {
    let mut log = |msg: &str| {
        cloud_sync::log_progress(&format!("adopting the sandbox of {node_id}"), msg);
        progress(msg);
    };
    cloud_sync::adopt::adopt_node_sandbox(fleet, node_id, &mut log)
        .map_err(|err| describe("Adopt existing sandbox failed", &err))
}

/// What [`run_in_cloud`] did: the node's cloud row once its sandbox accepted
/// it (even when the supervisor then stayed silent), and the line to show.
pub struct Started {
    pub accepted: Option<CloudNode>,
    pub result: JobResult,
}

/// Start `node_id` under its supervisor, making its sandbox when it has none.
pub fn run_in_cloud(fleet: &FleetStore, root: &std::path::Path, node_id: &str, progress: &mut dyn FnMut(&str)) -> Started {
    let mut log = |msg: &str| {
        cloud_sync::log_progress(&format!("starting {node_id}"), msg);
        progress(msg);
    };
    match cloud_sync::start_in_cloud(fleet, root, node_id, &mut log) {
        Ok(started) if started.heard => Started { accepted: Some(started.node), result: Ok("Started.".to_string()) },
        // Accepted, but nothing has come back: said, not left to look like
        // it is working.
        Ok(started) => Started {
            accepted: Some(started.node),
            result: Err(format!(
                "The node's sandbox accepted it, but its supervisor has not reported in after {} seconds.\n\
                 Sync now looks again. If it stays silent, check the log panel for the cloud's problems, \
                 or use Reset environment.",
                cloud_sync::SUPERVISOR_REPORTS_WITHIN.as_secs()
            )),
        },
        Err(err) => Started { accepted: None, result: Err(describe("Start failed", &err)) },
    }
}

/// **The one place that gives a cloud node's agent work**: "this node has
/// work now" (a Start, a Resume, an answer, a wake). The principle
/// (`doc/architecture.md`, "Sandboxes exist only for work"): a node's sandbox
/// is made when there is work for it and not before, so this decides, every
/// time, whether it has one:
///
/// - the node has run in the cloud before (it has a `cloud_nodes` row) and
///   `kind` is a request: [`request`] hands the work to its supervisor; the
///   wake behind it makes a new sandbox first when the node has none or its
///   sandbox is gone;
/// - otherwise (a first Start, or a Start with no request): [`run_in_cloud`]
///   makes the sandbox when there is none and starts the supervisor.
///
/// Only the app makes sandboxes (the user's Blaxel token is only here), so the
/// orchestrator, which finds work for a node with no sandbox (a webhook, a
/// timed wake), marks the node and the app comes back through here
/// (`cloud_sync::lost`).
pub fn submit_work(
    fleet: &FleetStore,
    root: &std::path::Path,
    node_id: &str,
    kind: Option<&'static str>,
    progress: &mut dyn FnMut(&str),
) -> JobResult {
    submit_work_tracked(fleet, root, node_id, kind, progress).result
}

/// Submit work with the accepted cloud row when starting a new sandbox. The
/// task panel uses that row for its immediate display; callers that only need
/// the outcome can use [`submit_work`].
pub fn submit_work_tracked(
    fleet: &FleetStore,
    root: &std::path::Path,
    node_id: &str,
    kind: Option<&'static str>,
    progress: &mut dyn FnMut(&str),
) -> Started {
    match kind {
        Some(kind) if cloud_sync::cloud_node(fleet, node_id).is_some() => Started {
            accepted: None,
            result: request(fleet, root, node_id, kind, progress),
        },
        _ => run_in_cloud(fleet, root, node_id, progress),
    }
}

/// What the user asked of a supervisor, in words, for the line: what was
/// asked for, and the menu entry's name for a failure's title.
pub fn request_words(kind: &str) -> (&'static str, &'static str) {
    match kind {
        cloud_sync::REQUEST_FRESH_SESSION => ("a new session", "Run with a new session"),
        cloud_sync::REQUEST_FRESH_BUDGET => ("a fresh budget", "Resume with a fresh budget"),
        cloud_sync::REQUEST_KEEP_GOING => ("it to go on", "Resume"),
        _ => ("this", "The request"),
    }
}

/// Record the user's request of `kind` for `node_id`'s supervisor (a new
/// session, a fresh budget, going on) and wake it
/// (`cloud_sync::wake_node_tracked_requesting`): the sandbox is not touched
/// beyond a request file and a poke.
pub fn request(
    fleet: &FleetStore,
    root: &std::path::Path,
    node_id: &str,
    kind: &'static str,
    progress: &mut dyn FnMut(&str),
) -> JobResult {
    let (what, action) = request_words(kind);
    let mut log = |msg: &str| {
        cloud_sync::log_progress(&format!("{kind} for {node_id}"), msg);
        progress(msg);
    };
    log(&format!("Asking the supervisor for {what}…"));
    match cloud_sync::wake_node_tracked_requesting(fleet, root, node_id, kind, &mut log) {
        Ok((said, true)) => Ok(format!("Asked for {what}: {said}.")),
        Ok((said, false)) => Err(format!(
            "Asked for {what} ({said}), but its supervisor has not reported in after {} seconds.\n\
             Sync now looks again. If it stays silent, check the log panel for the cloud's problems, \
             or use Reset environment.",
            cloud_sync::SUPERVISOR_REPORTS_WITHIN.as_secs()
        )),
        Err(err) => Err(describe(&format!("{action} failed"), &err)),
    }
}

/// A `pause` or `stop_now` (`action` names it): recorded, synced and poked,
/// and answered at once (`cloud_sync::wake_node_requesting`), not when the
/// supervisor reports back; the standing shows it stop.
pub fn stop_request(
    fleet: &FleetStore,
    root: &std::path::Path,
    node_id: &str,
    kind: &'static str,
    action: &'static str,
) -> JobResult {
    match cloud_sync::wake_node_requesting(fleet, root, node_id, kind) {
        Ok(said) => Ok(format!("{action} asked of the supervisor: {said}.")),
        Err(err) => Err(describe(&format!("{action} failed"), &err)),
    }
}

/// Set the node's environment up again, deleting nothing
/// (`environment_reset::reset`).
pub fn reset_environment(fleet: &FleetStore, node: Uuid, progress: &mut dyn FnMut(&str)) -> JobResult {
    let mut log = |msg: &str| {
        cloud_sync::log_progress(&format!("reset environment of {node}"), msg);
        progress(msg);
    };
    crate::environment_reset::reset(fleet, node, &mut log)
        .map(|()| "The environment was set up again.".to_string())
        .map_err(|err| describe("Reset environment failed", &err))
}

/// Sync with the orchestrator.
///
/// With `node`, the result also says what the sync now shows of that node's
/// supervisor, so the click answers "is it working?", not only "did it sync?".
pub fn sync_now(fleet: &FleetStore, root: &std::path::Path, node: Option<Uuid>) -> JobResult {
    let report = cloud_sync::sync_now(fleet, root).map_err(|err| describe("Sync failed", &err))?;
    let mut msg = report.summary();
    if let Some(node) = node
        && let Some(status) = cloud_sync::cloud_status(fleet, node, tod_store::outline::uuid_blob::now_ms())
    {
        msg.push_str(". ");
        let why_not = if status == cloud_sync::CloudStatus::Lost {
            cloud_sync::lost::why_not_replaced_at_sync(fleet, node).map_err(|err| describe("Sync failed", &err))?
        } else {
            None
        };
        if let Some(why_not) = why_not {
            // The check after a sync replaces only a node at work: running it
            // here made nothing and read as a failed replacement.
            msg.push_str(&format!(
                "Its sandbox is gone, and none is made now: {why_not}. A new one is made when the node next has work; Start makes one now."
            ));
        } else if status == cloud_sync::CloudStatus::Lost && cloud_sync::lost::any_marked(fleet) {
            // Sync now is the "next sync": replace it now, and say what
            // happened, not only that it was started.
            match cloud_sync::lost::check_marked_now(fleet) {
                Ok(Some(done)) if done.replaced > 0 => msg.push_str("Its sandbox was gone; a new one was made."),
                Ok(Some(_)) => {
                    let why = cloud_sync::lost::note(&node.to_string()).unwrap_or_else(|| "nothing was replaced, and the check recorded no reason (the log panel has the orchestrator's side)".into());
                    return Err(describe("Replacing its sandbox failed", &anyhow::anyhow!(why)));
                }
                Ok(None) => msg.push_str("Its sandbox is gone; another check is already making a new one."),
                Err(err) => return Err(describe("Replacing its sandbox failed", &err)),
            }
        } else {
            msg.push_str(&status.describe());
        }
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(check: tod_sandbox::health::Check, detail: &str, setup: tod_sandbox::health::Setup) -> tod_sandbox::health::HealthFailure {
        tod_sandbox::health::HealthFailure { check, detail: detail.into(), plain: false, setup }
    }

    /// An existing sandbox's health failure carries its own "What to do:", so
    /// the panel adds no second, conflicting step.
    #[test]
    fn an_existing_sandboxes_health_failure_keeps_its_own_next_step() {
        use tod_sandbox::health::{Check, Setup};
        let text = describe("Start failed", &anyhow::Error::new(failure(Check::Relay, "the relay does not answer", Setup::Existing)));
        assert_eq!(text.matches("What to do:").count(), 1, "{text}");
        assert!(text.contains("use Reset environment in the task panel's runner menu"), "{text}");
        assert!(text.contains("Technical detail: sandbox health check \"relay up\" failed"), "{text}");
    }

    /// A temporary failure while Start makes a sandbox (one that did not
    /// wake): the health text has no step, and the maker's (it deleted the
    /// sandbox), which `discard_failed_sandbox` adds, is the only one.
    #[test]
    fn a_new_sandboxes_temporary_health_failure_has_exactly_the_makers_step() {
        use tod_sandbox::health::{Check, Setup};
        let make = || failure(Check::Reachable, "node-x: did not wake", Setup::New);
        assert!(tod_sandbox::blaxel::error_is_transient(&anyhow::Error::new(make())));
        let err = anyhow::Error::new(make()).context(format!(
            "node-x was deleted. This looks temporary (the network or Blaxel). {} start the node again, which makes a fresh sandbox",
            tod_sandbox::health::WHAT_TO_DO
        ));
        let text = describe("Start failed", &err);
        assert_eq!(text.matches("What to do:").count(), 1, "{text}");
        assert!(text.contains("What to do: start the node again, which makes a fresh sandbox"), "{text}");
        assert!(!text.contains("next Start deletes"), "{text}");
        assert!(text.contains("Technical detail: sandbox health check \"reachable\" failed"), "{text}");
    }

    /// "Asked for {what}" and "Asking the supervisor for {what}" must read as
    /// sentences for every request.
    #[test]
    fn request_words_read_in_the_asked_for_sentence() {
        for kind in [cloud_sync::REQUEST_FRESH_SESSION, cloud_sync::REQUEST_FRESH_BUDGET, cloud_sync::REQUEST_KEEP_GOING] {
            let (what, _) = request_words(kind);
            assert!(!what.starts_with("to "), "{kind}: Asked for {what}");
        }
    }

    #[test]
    fn every_failure_says_what_to_do_next() {
        let temporary = anyhow::anyhow!("sandbox API timeout: connect").context("upload /x");
        let text = describe("Start failed", &temporary);
        assert!(text.starts_with("Start failed\nupload /x\n"), "{text}");
        assert!(text.contains("What to do: this looks temporary"), "{text}");

        let unknown = anyhow::anyhow!("the checkout has no branch");
        let text = describe("Start failed", &unknown);
        assert!(text.contains("What to do: try again. If it fails the same way"), "{text}");

        // A cause that already says what to do is not given a second line.
        let said = anyhow::anyhow!("x was kept. What to do: open it with `tod-sandbox shell x`");
        let text = describe("Start failed", &said);
        assert_eq!(text.matches("What to do:").count(), 1, "{text}");
    }
}
