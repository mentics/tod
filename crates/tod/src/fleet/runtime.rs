//! Callable trait surface for agent-runtime integration (guest liveness, shells, prompts).

use crate::fleet::repos::agent::FleetAgent;
use crate::fleet::repos::shell::ShellSession;

/// After host PID+birth_token match, confirm the guest agent session is reachable.
pub trait GuestLivenessCheck: Send + Sync {
    fn guest_alive(&self, agent: &FleetAgent) -> bool;
    /// Live runtime status to persist after successful reattach (e.g. `waiting`).
    fn live_runtime_status(&self, agent: &FleetAgent) -> &str;
}

/// Shell spawn metadata for reconnect / display (stub until real runtime wiring).
pub trait ShellSpawnMetadata: Send + Sync {
    fn shell_label(&self, session: &ShellSession) -> String;
}

/// Memory-only prompt delivery state (queued vs in-flight).
pub trait PromptDeliveryState: Send + Sync {
    fn queued_count(&self, agent_id: &str) -> usize;
    fn in_flight_count(&self, agent_id: &str) -> usize;
    fn total_queued(&self) -> usize;
    fn total_in_flight(&self) -> usize;
}

/// No-op guest liveness: host verify alone is sufficient; status becomes `waiting`.
pub struct NoopGuestLiveness;

impl GuestLivenessCheck for NoopGuestLiveness {
    fn guest_alive(&self, _agent: &FleetAgent) -> bool {
        true
    }

    fn live_runtime_status(&self, _agent: &FleetAgent) -> &str {
        "waiting"
    }
}

/// Test double that always reports guest unreachable.
pub struct UnreachableGuestLiveness;

impl GuestLivenessCheck for UnreachableGuestLiveness {
    fn guest_alive(&self, _agent: &FleetAgent) -> bool {
        false
    }

    fn live_runtime_status(&self, _agent: &FleetAgent) -> &str {
        "waiting"
    }
}

/// Default shell label from session id.
pub struct DefaultShellSpawnMetadata;

impl ShellSpawnMetadata for DefaultShellSpawnMetadata {
    fn shell_label(&self, session: &ShellSession) -> String {
        format!("shell {}", session.id)
    }
}
