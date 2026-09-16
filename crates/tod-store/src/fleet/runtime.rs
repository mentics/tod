//! Callable trait surface for agent-runtime integration (guest liveness, shells, prompts).

use crate::fleet::repos::agent_run::{AgentRun, RUNTIME_STATUS_ACTIVE};
use crate::fleet::repos::shell::ShellSession;

/// After host PID+birth_token match, confirm the guest agent session is reachable.
pub trait GuestLivenessCheck: Send + Sync {
    fn guest_alive(&self, run: &AgentRun) -> bool;
    /// Live runtime status to persist after successful reattach — always
    /// `RUNTIME_STATUS_ACTIVE`; `runtime_status` only distinguishes active
    /// from done, so every impl agrees here regardless of how it checked
    /// liveness.
    fn live_runtime_status(&self, run: &AgentRun) -> &str;
}

/// Shell spawn metadata for reconnect / display (stub until real runtime wiring).
pub trait ShellSpawnMetadata: Send + Sync {
    fn shell_label(&self, session: &ShellSession) -> String;
}

/// No-op guest liveness: host verify alone is sufficient.
pub struct NoopGuestLiveness;

impl GuestLivenessCheck for NoopGuestLiveness {
    fn guest_alive(&self, _run: &AgentRun) -> bool {
        true
    }

    fn live_runtime_status(&self, _run: &AgentRun) -> &str {
        RUNTIME_STATUS_ACTIVE
    }
}

/// Test double that always reports guest unreachable.
pub struct UnreachableGuestLiveness;

impl GuestLivenessCheck for UnreachableGuestLiveness {
    fn guest_alive(&self, _run: &AgentRun) -> bool {
        false
    }

    fn live_runtime_status(&self, _run: &AgentRun) -> &str {
        RUNTIME_STATUS_ACTIVE
    }
}

/// Default shell label from session id.
pub struct DefaultShellSpawnMetadata;

impl ShellSpawnMetadata for DefaultShellSpawnMetadata {
    fn shell_label(&self, session: &ShellSession) -> String {
        format!("shell {}", session.label_number)
    }
}
