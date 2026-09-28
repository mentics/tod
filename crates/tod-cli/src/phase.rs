//! `tod-cli phase` — whether a node's current lifecycle phase is done
//! (`doc/lifecycle/phase-agents.md`).
//!
//! The phase agent marks its phase `ready` for evaluation (or certifies it
//! itself when independent evaluation is off); the evaluator `certify`s it or
//! `reject`s it with the fixes it needs. The app records the digest of what
//! was judged, and the gate passes only while it is unchanged. The state is
//! always the node's current one. Inside a conversation `TOD_IMPLEMENT_NODE`
//! supplies `--node` when it is not given.

use crate::Invocation;
use crate::args::Args;
use crate::review::env_uuid;
use tod_core::conversation::implement::IMPLEMENT_NODE_ENV;
use tod_core::gate::evaluate_derived_criterion;
use tod_core::task::model::next_lifecycle;
use tod_store::interview::{ACTOR_AGENT, ACTOR_ENV, InterviewCommand};
use tod_store::outline::GateRepo;
use tod_store::outline::repos::NodeRepo;
use tod_store::phase::{
    CERTIFIER_SELF, CertificateStatus, PhaseRepo, certifier_for_actor, is_certifiable,
};
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli phase — whether a node's current lifecycle phase is done

Every command acts on the node's current lifecycle state. Inside a
conversation --node defaults to the node it is about.

COMMANDS:
    status  [--node <UUID>]
    ready   [--node <UUID>]
    certify [--node <UUID>] --note <TEXT>
    reject  [--node <UUID>] --fix <TEXT> (repeatable, at least one)

`status` shows the node's state, each gate criterion for leaving it with the
app's verdict now, and the phase's certificate: none, current (who certified
it and their note), or stale (and what changed since).
`ready` says the phase's work is done: have it evaluated.
`certify` says the phase is done. The app records a digest of what was judged,
and the gate passes only while that is unchanged. --note is required: one line
saying why.
`reject` sends the phase back to its agent with the fixes it must make, one per
--fix.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "status" => status(&inv, &args),
        "ready" => ready(&inv, &args),
        "certify" => certify(&inv, &args),
        "reject" => reject(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn node(inv: &Invocation, args: &Args) -> anyhow::Result<Uuid> {
    if let Some(raw) = args.get("--node") {
        return crate::node::resolve(inv, raw);
    }
    env_uuid(IMPLEMENT_NODE_ENV)?
        .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required outside a conversation"))
}

/// The node's current lifecycle state.
fn state(inv: &Invocation, node: Uuid) -> anyhow::Result<String> {
    inv.client()
        .read(|conn| NodeRepo::new(conn).get_lifecycle(node))?
        .ok_or_else(|| anyhow::anyhow!("node {node} has no lifecycle state"))
}

fn status(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node = node(inv, args)?;
    let state = state(inv, node)?;
    let next = next_lifecycle(&state);
    let (criteria, certificate) = inv.client().read(|conn| {
        let mut criteria = Vec::new();
        if let Some(next) = next {
            for criterion in GateRepo::new(conn).list_for_transition(&state, next)? {
                let outcome = evaluate_derived_criterion(conn, node, &criterion)?;
                criteria.push((criterion, outcome));
            }
        }
        let certificate = is_certifiable(&state)
            .then(|| PhaseRepo::new(conn).certificate_status(node, &state))
            .transpose()?;
        Ok((criteria, certificate))
    })?;

    if inv.json {
        let criteria: Vec<_> = criteria
            .iter()
            .map(|(c, outcome)| {
                serde_json::json!({
                    "slug": c.slug,
                    "label": c.label,
                    "outcome": outcome.as_ref().map(|o| o.outcome),
                    "detail": outcome.as_ref().map(|o| o.detail.as_str()),
                })
            })
            .collect();
        let certificate = match &certificate {
            None => serde_json::json!("not needed"),
            Some(CertificateStatus::None) => serde_json::json!({ "status": "none" }),
            Some(CertificateStatus::Current(e)) => serde_json::json!({
                "status": "current", "certifier": e.certifier, "note": e.body,
            }),
            Some(CertificateStatus::Stale { event, changed }) => serde_json::json!({
                "status": "stale", "certifier": event.certifier, "note": event.body,
                "changed": changed,
            }),
        };
        return Ok(serde_json::to_string(&serde_json::json!({
            "state": state,
            "next": next,
            "criteria": criteria,
            "certificate": certificate,
        }))?);
    }

    let mut out = vec![format!("state: {state}")];
    match next {
        None => out.push(format!("gate: none (`{state}` is the last state)")),
        Some(next) => {
            out.push(format!("gate {state} → {next}:"));
            if criteria.is_empty() {
                out.push("  (no criteria)".into());
            }
            for (criterion, outcome) in &criteria {
                match outcome {
                    Some(o) => out.push(format!("  {} {}: {}", o.outcome, criterion.slug, o.detail)),
                    None => out.push(format!("  ? {}: not checked by the app", criterion.slug)),
                }
            }
        }
    }
    out.push(match certificate {
        None => format!("certificate: not needed in `{state}`"),
        Some(CertificateStatus::None) => "certificate: none".into(),
        Some(CertificateStatus::Current(e)) => {
            format!("certificate: current ({}): {}", e.certifier, e.body)
        }
        Some(CertificateStatus::Stale { event, changed }) => {
            let mut lines = vec![format!(
                "certificate: stale ({}): {}",
                event.certifier, event.body
            )];
            lines.push("changed since:".into());
            lines.extend(changed.iter().map(|c| format!("  {c}")));
            lines.join("\n")
        }
    });
    Ok(out.join("\n"))
}

fn ready(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node_id = node(inv, args)?;
    let state = state(inv, node_id)?;
    let result = inv
        .client()
        .interview(InterviewCommand::PhaseReady { node_id, state })?;
    ack(inv, result)
}

fn certify(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node_id = node(inv, args)?;
    let note = args.require("--note")?.trim().to_string();
    if note.is_empty() {
        anyhow::bail!("--note must say why the phase is done");
    }
    refuse_self_certification(inv)?;
    let state = state(inv, node_id)?;
    let result = inv.client().interview(InterviewCommand::PhaseCertify {
        node_id,
        state,
        note,
    })?;
    ack(inv, result)
}

/// With independent evaluation on, only the evaluator certifies: the agent
/// that did the phase's work records it `ready` instead.
fn refuse_self_certification(inv: &Invocation) -> anyhow::Result<()> {
    if !tod_core::phase::independent_evaluation(&inv.data_root) {
        return Ok(());
    }
    let actor = std::env::var(ACTOR_ENV).unwrap_or_else(|_| ACTOR_AGENT.to_string());
    let (_, certifier) = inv
        .client()
        .read(|conn| certifier_for_actor(conn, &actor))?;
    if certifier == CERTIFIER_SELF {
        anyhow::bail!(
            "independent evaluation is on, so a separate session certifies this phase:              run `tod-cli phase ready` when its work is done"
        );
    }
    Ok(())
}

fn reject(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let node_id = node(inv, args)?;
    let fixes: Vec<String> = args
        .get_all("--fix")
        .iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    if fixes.is_empty() {
        anyhow::bail!("reject needs at least one --fix <TEXT>");
    }
    let state = state(inv, node_id)?;
    let result = inv.client().interview(InterviewCommand::PhaseReject {
        node_id,
        state,
        fixes,
    })?;
    ack(inv, result)
}

fn ack(inv: &Invocation, result: serde_json::Value) -> anyhow::Result<String> {
    if inv.json {
        return Ok(serde_json::to_string(&result)?);
    }
    Ok("ok".into())
}
