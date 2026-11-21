//! `tod-cli lifecycle` — move a node's lifecycle state: the operator's `set`,
//! and a judging session's `send-back`.
//!
//! `set` is the same store change as the task panel's phase chip: both send
//! `tod_core::lifecycle::operator::user_move`, so a move back clears what
//! certified the states being redone, reopens verified verdicts, cancels what
//! the node waited on and retakes or clears the lifecycle baseline.
//! Unlike the chip it does not advance the node's Linear ticket (that needs
//! a second store writer beside the daemon). Refused in an agent session,
//! and into `pr_review` or beyond.
//!
//! `send-back` is the agent's: a phase agent, evaluator, implementation (to
//! dispute an obligation), verification, review or fix session sends its own
//! node back to the phase that got it wrong (`tod_store::send_back`, the same
//! store move), with the conversation as the sender. A send-back the loop
//! guard refuses stops the run for the user and fails here with why.
//! `resolve-dispute` is the receiving phase agent's answer to a send-back that
//! disputed an obligation.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::IMPLEMENT_CONVERSATION_ENV;
use tod_core::interview::client::InterviewClient;
use tod_core::lifecycle::operator;
use tod_store::interview::{ACTOR_AGENT, ACTOR_ENV, ACTOR_USER, InterviewCommand, InterviewRepo};
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli lifecycle — move a node's lifecycle state

COMMANDS:
    set <NODE> <STATE> --reason <TEXT>
    send-back --to <STATE> --reason <TEXT> [--evidence <KIND>:<ID>]... [--obligation <ID>]...
    resolve-dispute --as <fixed|upheld|passed-up> --note <TEXT> [--send-back <ID>]

`set` moves the node to <STATE> at once, bypassing the gates, exactly as the
task panel's state chip does: a move back clears what certified the states
being redone and reopens verified verdicts. --reason is required and is kept
on the node's activity, prefixed `tod-cli:`. It does not advance the
node's Linear ticket, as the chip does. <NODE> is a node slug or full UUID.
It is for the operator only and is refused in an agent session. It also
refuses `pr_review` and every state after it: marking the pull request ready
and choosing a reviewer are the user's step.

`send-back` sends the session's own node back to an earlier state, before
`pr_review`, with the reason and evidence that phase starts from. Only a
phase agent, an evaluator, or an implementation, verification, review or fix
session may use it. At `active` or earlier the implementation steps and
verified verdicts reopen; to `verifying`, only the verdicts of the
--obligation(s) named, and at least one is required. With --obligation and a
target other than `verifying` it is a dispute of those obligations, which
must be agent-written and go to the phase that wrote them (`design` for a
design obligation, `proposed` for a requirement); an implementation session
may send back only that way. The same move again on unchanged obligations,
plan and design, or a third send-back to the same state since the user last
acted, is refused and stops the run for the user.

`resolve-dispute` is how the phase agent a dispute was sent to settles it:
`fixed` (a disputed obligation was changed or removed; refused if none was),
`upheld` (it stands; the note says how it is meant) or `passed-up` (design
only: the dispute goes on to `proposed` with the chain). --note is required.
The phase cannot be recorded ready or certified by an agent while a dispute
sent to it is open.
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
        "send-back" => send_back(&inv, &args, |name| std::env::var(name).ok()),
        "resolve-dispute" => resolve_dispute(&inv, &args, |name| std::env::var(name).ok()),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// `env` is the process environment, passed in so a test can play an agent.
fn send_back(inv: &Invocation, args: &Args, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let set = |name: &str| env(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let Some(raw) = set(IMPLEMENT_CONVERSATION_ENV) else {
        anyhow::bail!("not in an agent session: {}", tod_store::send_back::WHO_MAY);
    };
    let conversation_id = Uuid::parse_str(&raw)
        .map_err(|_| anyhow::anyhow!("{IMPLEMENT_CONVERSATION_ENV} is not a UUID (`{raw}`)"))?;
    let to = args.require("--to")?.trim().to_string();
    let reason = args.require("--reason")?.trim().to_string();
    let evidence = args
        .get_all("--evidence")
        .iter()
        .map(|raw| crate::decisions::parse_evidence(inv, raw))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let client = InterviewClient::new(&inv.data_root, set(ACTOR_ENV).unwrap_or_else(|| ACTOR_AGENT.to_string()));
    let obligations = args
        .get_all("--obligation")
        .iter()
        .map(|raw| client.read(|conn| InterviewRepo::new(conn).resolve_obligation_id(raw)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let result = client.interview(InterviewCommand::SendBack(tod_store::send_back::Request {
        conversation_id,
        to: to.clone(),
        reason,
        evidence,
        obligations,
    }))?;
    if result["outcome"] == tod_store::send_back::OUTCOME_STOPPED {
        anyhow::bail!(
            "not sent back: {}. The run stops here for the user; end your turn",
            result["why"].as_str().unwrap_or("the loop guard refused it")
        );
    }
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok(format!(
        "ok sent back {} -> {}; end your turn, the {} phase takes it from here",
        result["from"].as_str().unwrap_or("?"),
        to,
        to
    ))
}

/// The receiving phase agent settles the dispute sent to its state.
fn resolve_dispute(inv: &Invocation, args: &Args, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let set = |name: &str| env(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let Some(raw) = set(IMPLEMENT_CONVERSATION_ENV) else {
        anyhow::bail!("not in an agent session: only the phase agent a dispute was sent to settles it");
    };
    let conversation_id = Uuid::parse_str(&raw)
        .map_err(|_| anyhow::anyhow!("{IMPLEMENT_CONVERSATION_ENV} is not a UUID (`{raw}`)"))?;
    let kind = args.require("--as")?.trim().replace('-', "_");
    let note = args.require("--note")?.trim().to_string();
    let send_back = args
        .get("--send-back")
        .map(|raw| Uuid::parse_str(raw.trim()).map_err(|_| anyhow::anyhow!("--send-back is not a UUID (`{raw}`)")))
        .transpose()?;
    let client = InterviewClient::new(&inv.data_root, set(ACTOR_ENV).unwrap_or_else(|| ACTOR_AGENT.to_string()));
    let result = client.interview(InterviewCommand::ResolveDispute(tod_store::send_back::Resolution {
        conversation_id,
        send_back,
        kind: kind.clone(),
        note,
    }))?;
    if result["outcome"] == tod_store::send_back::OUTCOME_STOPPED {
        anyhow::bail!(
            "not passed up: {}. The run stops here for the user; end your turn",
            result["why"].as_str().unwrap_or("the loop guard refused it")
        );
    }
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    if result["outcome"] == tod_store::send_back::OUTCOME_MOVED {
        return Ok("ok passed up to proposed; end your turn, that phase takes it from here".to_string());
    }
    Ok(format!("ok dispute {kind}"))
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
        let (root, node) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let id = node.to_string();
        let (i, a) = inv(&root, &[&id, "active", "--reason", "x"]);
        let err = set(&i, &a, |n| (n == "TOD_INTERVIEW_ACTOR").then(|| "conversation:abc".into())).unwrap_err();
        assert!(err.to_string().contains("operator only"), "{err}");
        assert_eq!(state(&root, node), "review");
    }

    #[test]
    fn pr_review_is_refused_and_nothing_moves() {
        let (root, node) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let id = node.to_string();
        let (i, a) = inv(&root, &[&id, "pr_review", "--reason", "x"]);
        let err = set(&i, &a, no_env).unwrap_err();
        assert!(err.to_string().contains("refusing to move"), "{err}");
        assert_eq!(state(&root, node), "review");
    }

    #[test]
    fn a_reason_is_required() {
        let (root, node) = crate::tests::data_root();
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
        let (root, node) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let obligation = Uuid::new_v4();
        {
            let db = tod_store::fleet::FleetStore::open(&root).unwrap().paths().db().to_path_buf();
            let conn = tod_store::fleet::schema::open_writer_connection(&db).unwrap();
            tod_store::outline::repos::ObligationRepo::new(&conn)
                .insert_at(obligation, node, "requirement", usize::MAX, None, "Holds", "requirements")
                .unwrap();
            VerdictRepo::new(&conn).record(node, obligation, None, "verified", "Saw it work.", &tod_store::verification::VerdictHow::method("live")).unwrap();
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

    fn conversation(root: &PathBuf, node: Uuid, protocol: tod_store::conversation::ProtocolKind) -> Uuid {
        let db = tod_store::fleet::FleetStore::open(root).unwrap().paths().db().to_path_buf();
        let conn = tod_store::fleet::schema::open_writer_connection(&db).unwrap();
        tod_store::conversation::ConversationRepo::new(&conn)
            .create(tod_store::conversation::Focus::Node(node), protocol, None, None, None)
            .unwrap()
            .id
    }

    fn session(conversation: Uuid) -> impl Fn(&str) -> Option<String> {
        move |name| (name == IMPLEMENT_CONVERSATION_ENV).then(|| conversation.to_string())
    }

    #[test]
    fn send_back_outside_a_session_says_who_may() {
        let (root, node) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let (i, a) = inv(&root, &["--to", "active", "--reason", "x"]);
        let err = send_back(&i, &a, no_env).unwrap_err();
        assert!(err.to_string().contains("only a phase agent"), "{err}");
        assert_eq!(state(&root, node), "review");
    }

    #[test]
    fn send_back_by_an_implementation_session_is_refused() {
        let (root, node) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "active");
        let id = conversation(&root, node, tod_store::conversation::ProtocolKind::Implementation);
        let (i, a) = inv(&root, &["--to", "design", "--reason", "x"]);
        let err = send_back(&i, &a, session(id)).unwrap_err();
        assert!(err.to_string().contains("only to dispute an obligation"), "{err}");
        assert_eq!(state(&root, node), "active");
    }

    #[test]
    fn a_review_session_sends_the_node_back_and_a_repeat_is_refused() {
        let (root, node) = crate::tests::data_root();
        crate::tests::set_lifecycle(&root, node, "review");
        let id = conversation(&root, node, tod_store::conversation::ProtocolKind::Review);
        let (i, a) = inv(&root, &["--to", "active", "--reason", "the build is wrong"]);
        let out = send_back(&i, &a, session(id)).unwrap();
        assert!(out.starts_with("ok sent back review -> active"), "{out}");
        assert_eq!(state(&root, node), "active");

        crate::tests::set_lifecycle(&root, node, "review");
        let err = send_back(&i, &a, session(id)).unwrap_err();
        assert!(err.to_string().contains("not sent back"), "{err}");
        assert!(err.to_string().contains("unchanged"), "{err}");
        assert_eq!(state(&root, node), "review");
    }
}
