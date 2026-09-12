//! Agent transport re-exports.
//!
//! Implementations live in `tod-agent`. This module keeps the historical
//! `crate::interview::agent::*` import path working.
//! TODO(step 3): callers move to `tod_agent` directly when the UI is carved out.

#[allow(unused_imports)] // transitional facade; removed in the tod-ui split
pub use tod_agent::{
    AcpHost, AgentBackend, AgentLaunchOptions, AgentPlatform, AgentProvider, AgentRunState,
    CursorAcpProvider, MockAgentProvider, PermissionOption, PermissionRequest,
    RoutingAgentProvider, RunId, SharedAgent,
};
