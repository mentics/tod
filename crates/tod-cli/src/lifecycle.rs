//! `tod-cli lifecycle` — the operator's way to move a node's lifecycle state.
//!
//! The same store change as the task panel's phase chip: both send
//! `tod_core::lifecycle::operator::user_move`, so a move back clears what
//! certified the states being redone, reopens verified verdicts, cancels what
//! the node waited on and retakes or clears the lifecycle baseline.
//! Unlike the chip it does not advance the node's Linear ticket (that needs
//! a second store writer beside the daemon). Refused in an agent session,
//! and into `pr_review` or beyond.

use crate::Invocation;
use crate::args::Args;
use tod_core::interview::client::InterviewClient;
use tod_core::lifecycle::operator;
use tod_store::interview::ACTOR_USER;
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli lifecycle — move a node's lifecycle state (operator only)

COMMANDS:
    set <NODE> <STATE> --reason <TEXT>

`set` moves the node to <STATE> at once, bypassing the gates, exactly as the
task panel's state chip does: a move back clears what certified the states
being redone and reopens verified verdicts. --reason is required and is kept
on the node's activity, prefixed `tod-cli:`. It does not advance the
node's Linear ticket, as the chip does. <NODE> is a node slug or full UUID.
It is for the operator only and is refused in an agent session. It also
refuses `pr_review` and every state after it: marking the pull request ready
and choosing a reviewer are the user's step.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "set" => set(&inv, &args, |name| std::env::var(name).ok()),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// `env` is the process environment, passed in so a test can play an agent.
fn set(inv: &Invocation, args: &Args, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    operator::refuse_agent_session(env)?;
    let (Some(raw), Some(state)) = (args.positional.first(), args.positional.get(1)) else {
        anyhow::bail!("usage: tod-cli lifecycle set <NODE> <STATE> --reason <TEXT>");
    };
    let reason = args.require("--reason")?.trim().to_string();
    if reason.is_empty() {
        anyhow::bail!("--reason must say why the node is moved");
    }
    let node = crate::node::resolve(inv, raw)?;
    // Written as the user, like the phase chip, and never as the agent actor
    // `Invocation::client` would default to.
    let client = InterviewClient::new(&inv.data_root, ACTOR_USER);
    let current = current_state(&client, node)?;
    operator::check_operator_target(&current, state)?;
    client.outline(operator::user_move(node, state, Some(&format!("tod-cli: {reason}"))))?;
    let now = current_state(&client, node)?;
    if now != *state {
        anyhow::bail!("asked to move the node to `{state}`, but it is in `{now}`");
    }
    if inv.json {
        return Ok(serde_json::to_string(&serde_json::json!({ "node": node, "from": current, "to": now }))?);
    }
    Ok(format!("ok {current} -> {now}"))
}

fn current_state(client: &InterviewClient, node: Uuid) -> anyhow::Result<String> {
    client.read(|conn| {
        NodeRepo::new(conn)
            .get_lifecycle(node)?
            .ok_or_else(|| anyhow::anyhow!("node {node} has no lifecycle (enable the Lifecycle capability first)"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn inv(root: &PathBuf, args: &[&str]) -> (Invocation, Args) {
        let rest: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let parsed = Args::parse(&rest).unwrap();
        (Invocation { data_root: root.clone(), json: false, rest }, parsed)
    }

    fn state(root: &PathBuf, node: Uuid) -> String {
        InterviewClient::new(root, ACTOR_USER)
            .read(|conn| Ok(NodeRepo::new(conn).get_lifecycle(node)?.unwrap()))
            .unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn an_agent_session_is_refused_and_nothing_moves() {
        let (root, node, _) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let id = node.to_string();
        let (i, a) = inv(&root, &[&id, "active", "--reason", "x"]);
        let err = set(&i, &a, |n| (n == "TOD_INTERVIEW_ACTOR").then(|| "conversation:abc".into())).unwrap_err();
        assert!(err.to_string().contains("operator only"), "{err}");
        assert_eq!(state(&root, node), "review");
    }

    #[test]
    fn pr_review_is_refused_and_nothing_moves() {
        let (root, node, _) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let id = node.to_string();
        let (i, a) = inv(&root, &[&id, "pr_review", "--reason", "x"]);
        let err = set(&i, &a, no_env).unwrap_err();
        assert!(err.to_string().contains("refusing to move"), "{err}");
        assert_eq!(state(&root, node), "review");
    }

    #[test]
    fn a_reason_is_required() {
        let (root, node, _) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let id = node.to_string();
        let (i, a) = inv(&root, &[&id, "active"]);
        assert!(set(&i, &a, no_env).unwrap_err().to_string().contains("--reason"));
    }

    /// review -> active does what the chip's mutation does: the state changes
    /// as the user's own move (with the reason), what certified the states
    /// redone is cleared, and verified verdicts are reopened.
    #[test]
    fn review_to_active_has_the_effects_of_the_chip() {
        use tod_store::verification::VerdictRepo;
        let (root, node, _) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let obligation = Uuid::new_v4();
        {
            let db = tod_store::fleet::FleetStore::open(&root).unwrap().paths().db().to_path_buf();
            let conn = tod_store::fleet::schema::open_writer_connection(&db).unwrap();
            tod_store::outline::repos::ObligationRepo::new(&conn)
                .insert_at(obligation, node, "requirement", usize::MAX, None, "Holds", "requirements")
                .unwrap();
            VerdictRepo::new(&conn).record(node, obligation, None, "verified", "Saw it work.").unwrap();
        }
        let id = node.to_string();
        let (i, a) = inv(&root, &[&id, "active", "--reason", "work was lost"]);
        assert_eq!(set(&i, &a, no_env).unwrap(), "ok review -> active");
        assert_eq!(state(&root, node), "active");

        let (timeline, verdict) = InterviewClient::new(&root, ACTOR_USER)
            .read(|conn| {
                let timeline = tod_store::node_timeline::TimelineRepo::new(conn).list(node)?;
                let verdict = VerdictRepo::new(conn).latest_for_node(node)?;
                Ok((timeline, verdict))
            })
            .unwrap();
        let change = timeline
            .iter()
            .filter(|e| e.kind == tod_store::node_timeline::KIND_PHASE && e.phase == "active")
            .next_back()
            .expect("a phase entry for the move");
        assert!(change.by_user, "shown as the user's move");
        let detail = change.phase_change().unwrap();
        assert_eq!(detail.from, "review");
        assert_eq!(detail.reason.as_deref(), Some("tod-cli: work was lost"));
        assert_eq!(verdict[&obligation].status, tod_store::verification::VERDICT_REOPENED);
    }
}
