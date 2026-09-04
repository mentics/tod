//! Launch options and static model/effort catalogs for Cursor and Claude agents.

use crate::settings::AgentPlatform;
use serde::{Deserialize, Serialize};

/// Per-launch platform / model / effort applied when starting a new ACP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunchOptions {
    pub platform: AgentPlatform,
    pub model: String,
    pub effort: String,
}

impl AgentLaunchOptions {
    pub fn for_platform(platform: AgentPlatform) -> Self {
        Self {
            platform,
            model: default_model_for(platform).to_string(),
            effort: DEFAULT_EFFORT.to_string(),
        }
    }

    pub fn from_settings(
        platform: AgentPlatform,
        model: impl Into<String>,
        effort: impl Into<String>,
    ) -> Self {
        Self {
            platform,
            model: model.into(),
            effort: effort.into(),
        }
    }

    /// Effort value to send over ACP, or `None` when unset (`auto` / empty).
    pub fn effort_for_acp(&self) -> Option<&str> {
        effort_for_acp(&self.effort)
    }
}

pub const DEFAULT_EFFORT: &str = "auto";

/// Claude Code model aliases.
/// Source: <https://code.claude.com/docs/en/model-config>
pub const CLAUDE_MODELS: &[&str] = &[
    "default",
    "best",
    "fable",
    "sonnet",
    "opus",
    "haiku",
    "sonnet[1m]",
    "opus[1m]",
    "opusplan",
];

/// Claude Code effort levels (no `ultracode`).
/// Source: <https://code.claude.com/docs/en/model-config>
pub const CLAUDE_EFFORTS: &[&str] = &["auto", "low", "medium", "high", "xhigh", "max"];

/// Cursor model IDs (static snapshot; account availability varies).
/// Sources: <https://cursor.com/docs/models>, <https://cursor.com/docs/subagents>
pub const CURSOR_MODELS: &[&str] = &[
    "auto",
    "composer-2",
    "composer-2.5",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-fable-5",
];

/// Cursor effort / reasoning levels.
/// Source: Cursor subagents `effort=` parameter docs.
pub const CURSOR_EFFORTS: &[&str] = &["auto", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn default_model_for(platform: AgentPlatform) -> &'static str {
    match platform {
        AgentPlatform::Claude => "default",
        AgentPlatform::Cursor => "auto",
    }
}

pub fn models_for(platform: AgentPlatform) -> &'static [&'static str] {
    match platform {
        AgentPlatform::Claude => CLAUDE_MODELS,
        AgentPlatform::Cursor => CURSOR_MODELS,
    }
}

pub fn efforts_for(platform: AgentPlatform) -> &'static [&'static str] {
    match platform {
        AgentPlatform::Claude => CLAUDE_EFFORTS,
        AgentPlatform::Cursor => CURSOR_EFFORTS,
    }
}

pub fn effort_for_acp(effort: &str) -> Option<&str> {
    let trimmed = effort.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(trimmed)
    }
}

pub fn parse_platform(raw: &str) -> Option<AgentPlatform> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "cursor" => Some(AgentPlatform::Cursor),
        "claude" | "anthropic" => Some(AgentPlatform::Claude),
        _ => None,
    }
}

pub fn platform_storage(platform: AgentPlatform) -> &'static str {
    match platform {
        AgentPlatform::Cursor => "cursor",
        AgentPlatform::Claude => "claude",
    }
}

/// If `model` is not in the platform catalog, return the platform default.
pub fn coerce_model(platform: AgentPlatform, model: &str) -> String {
    let models = models_for(platform);
    if models.iter().any(|m| *m == model) {
        model.to_string()
    } else {
        default_model_for(platform).to_string()
    }
}

/// If `effort` is not in the platform catalog, return `auto`.
pub fn coerce_effort(platform: AgentPlatform, effort: &str) -> String {
    let efforts = efforts_for(platform);
    if efforts.iter().any(|e| *e == effort) {
        effort.to_string()
    } else {
        DEFAULT_EFFORT.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_differ_by_platform() {
        assert_eq!(default_model_for(AgentPlatform::Claude), "default");
        assert_eq!(default_model_for(AgentPlatform::Cursor), "auto");
        assert_eq!(
            AgentLaunchOptions::for_platform(AgentPlatform::Claude).model,
            "default"
        );
    }

    #[test]
    fn effort_auto_skips_acp() {
        assert_eq!(effort_for_acp("auto"), None);
        assert_eq!(effort_for_acp(""), None);
        assert_eq!(effort_for_acp("high"), Some("high"));
    }

    #[test]
    fn coerce_resets_invalid() {
        assert_eq!(
            coerce_model(AgentPlatform::Claude, "not-a-model"),
            "default"
        );
        assert_eq!(coerce_effort(AgentPlatform::Cursor, "bogus"), "auto");
        assert_eq!(
            coerce_model(AgentPlatform::Cursor, "composer-2.5"),
            "composer-2.5"
        );
    }

    #[test]
    fn catalogs_contain_defaults() {
        assert!(CLAUDE_MODELS.contains(&"default"));
        assert!(CURSOR_MODELS.contains(&"auto"));
        assert!(CLAUDE_EFFORTS.contains(&"auto"));
        assert!(CURSOR_EFFORTS.contains(&"auto"));
    }
}
