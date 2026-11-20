//! The incident investigation (`crate::incident`, `doc/lifecycle/incidents.md`):
//! one short session, once per incident that is not an `auth` one, about a
//! failure many nodes share. It is given the samples and either fixes what it
//! can (a stuck process, a stale file) or files ONE request through
//! `tod-cli decisions ask`; it never works the node's own task.
//!
//! One turn, run by the autopilot of the node the incident was claimed for
//! (`tod_store::incidents::investigation_for`): there is no daemon-level
//! conversation runner. The session is the node's own focus, so its request
//! lands on that node.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Protocol, ProtocolEnv, scratch_dir};
use crate::dynamic::DynamicContext;
use anyhow::{Context, Result};
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::Workdir;
use tod_store::incidents::{self, Incident};
use uuid::Uuid;

pub struct IncidentProtocol;

/// What the session is told to start with.
pub const STARTER: &str = "Investigate the incident below.";

/// The incident `node`'s session is about: the one claimed for it.
pub fn incident_for(fleet: &tod_store::fleet::FleetStore, node: Uuid) -> Result<Incident> {
    fleet
        .read(|conn| Ok(incidents::list_open(conn)?.into_iter().find(|i| i.escalation_node == Some(node) && !i.notice)))?
        .with_context(|| format!("no incident is claimed for node {node}"))
}

/// The incident as the session sees it.
pub fn render(incident: &Incident) -> String {
    let mut out = format!(
        "- Kind: {}\n- Signature: {}\n- Summary: {}\n- Seen on {} node(s); first seen {}\n\nSamples (one per node, the exact error):\n",
        incident.kind.as_str(),
        incident.signature,
        incident.summary,
        incident.samples.len(),
        chrono::DateTime::from_timestamp_millis(incident.first_seen)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
    );
    for sample in incident.samples.iter().take(8) {
        out.push_str(&format!("\n- node {} while {}:\n```\n{}\n```\n", sample.node, sample.operation, sample.error));
    }
    out
}

impl Protocol for IncidentProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Incident
    }

    fn surface(&self) -> &'static str {
        crate::session_name::INCIDENT_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some(STARTER)
    }

    /// An empty directory: the samples are in its context, and it works on the
    /// machine, not in a node's checkout.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        scratch_dir(env.data_root, "incident").map(Workdir::Host)
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(IMPLEMENT_CONVERSATION_ENV.to_string(), env.conversation_id.to_string())];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let incident = incident_for(env.fleet, node_id(env)?)?;
        let text = render(&incident);
        let ctx = DynamicContext { data_root: Some(env.data_root), incident: &text, ..Default::default() };
        crate::context_recipes::build_message(env.media, &crate::context_recipes::INCIDENT, None, &ctx, "")
    }

    fn resume_snapshot(&self, env: &ProtocolEnv<'_>, _budget_tokens: i64, _before_seq: Option<i64>) -> Result<String> {
        self.opening(env)
    }
}

/// Plays the investigator for `--agent mock`: it does nothing.
pub fn mock_turn(_access: &impl super::mock::Access, _node_id: Uuid, _conversation_id: Uuid) -> Result<String> {
    Ok(String::new())
}
