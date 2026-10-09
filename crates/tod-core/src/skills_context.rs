//! The Skills block of an agent's context: the skills the user chose, through
//! the Lifecycle config capability, for the phase the agent is working in.
//!
//! A skill name is free text and is not checked: the skill may be set up after
//! the config, and the agent's environment (Cursor, a container, a sandbox) may
//! not have a Skill tool at all. So the block is advice the agent follows where
//! it can.

use anyhow::Result;
use rusqlite::Connection;
use tod_store::fleet::FleetStore;
use tod_store::lifecycle_config;
use uuid::Uuid;

/// The block for `phase` on `node`; empty when no skills are configured
/// (including when an explicit empty list turns them off).
pub fn render(conn: &Connection, node: Uuid, phase: &str) -> Result<String> {
    let skills = lifecycle_config::resolve(conn, node, phase)?.map(|r| r.skills).unwrap_or_default();
    Ok(render_skills(phase, &skills))
}

/// [`render`] for a caller with the store: a failed read leaves the block out
/// (with a warning) rather than stopping the agent.
pub fn for_node(fleet: &FleetStore, node: Uuid, phase: &str) -> String {
    fleet
        .read(|conn| render(conn, node, phase))
        .unwrap_or_else(|err| {
            tracing::warn!("could not read the configured skills for {phase}: {err:#}");
            String::new()
        })
}

pub fn render_skills(phase: &str, skills: &[String]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let list: Vec<String> = skills.iter().map(|s| format!("`{s}`")).collect();
    format!(
        "\n## Skills\n\nThe user chose these skills for the {phase} phase: {}.\n\n\
         Use them, in this order, through your Skill tool, and do the phase's work the way they \
         direct. Everything else in this message still applies: what the phase must record, and \
         how, is unchanged. If you have no Skill tool, or a skill is not available to you, carry \
         on following what it is meant to do as best you can.\n",
        list.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_configured_renders_nothing() {
        assert_eq!(render_skills("review", &[]), "");
    }

    #[test]
    fn skills_are_listed_in_order_for_their_phase() {
        let text = render_skills("review", &["security-review".into(), "simplify".into()]);
        assert!(text.contains("review phase"), "{text}");
        assert!(text.find("`security-review`").unwrap() < text.find("`simplify`").unwrap());
        assert!(text.contains("Skill tool"));
    }
}
