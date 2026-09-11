//! Interview phase keys.

pub use tod_store::interview::phase_for_session_key;

/// Session phase key without any parenthesised suffix.
pub fn base_interview_phase(phase: &str) -> &str {
    phase.split('(').next().unwrap_or(phase).trim()
}
