//! The Skills block of an agent's context: the skills the user chose, through
//! the Lifecycle config capability, for the phase the agent is working in.
//!
//! A skill name is free text and is not checked: the skill may be set up after
//! the config, and the agent's environment (Cursor, a container, a sandbox) may
//! not have a Skill tool at all. So the block is advice the agent follows where
//! it can. A name that is one of tod's own skills (`crate::skills`, shipped to
//! every sandbox) is pointed at `tod-cli skills show` instead of the Skill tool.

use anyhow::Result;
use rusqlite::Connection;
use tod_store::fleet::FleetStore;
use tod_store::lifecycle_config;
use uuid::Uuid;

/// The block for `phase` on `node`; empty when no skills are configured
/// (including when an explicit empty list turns them off).
pub fn render(conn: &Connection, node: Uuid, phase: &str) -> Result<String> {
    let skills = lifecycle_config::resolve(conn, node, phase)?.map(|r| r.skills).unwrap_or_default();
    if skills.is_empty() {
        return Ok(String::new());
    }
    let tod_skills = crate::skills::SkillsPaths::discover()?.names()?;
    Ok(render_skills(phase, &skills, &tod_skills))
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

/// `tod_skills` are the names of tod's own skills; any of `skills` among them
/// is read with `tod-cli skills show`, the rest through the Skill tool.
pub fn render_skills(phase: &str, skills: &[String], tod_skills: &[String]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let quoted = |names: &[&String]| names.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
    let all: Vec<&String> = skills.iter().collect();
    let (own, other): (Vec<&String>, Vec<&String>) = skills.iter().partition(|s| tod_skills.contains(s));
    let mut how = String::new();
    if !own.is_empty() {
        how.push_str(&format!(
            "{} {} tod's own: read {} with `tod-cli skills show <NAME>`, not your Skill tool. ",
            quoted(&own),
            if own.len() == 1 { "is" } else { "are" },
            if own.len() == 1 { "it" } else { "each" },
        ));
    }
    if !other.is_empty() {
        how.push_str(&format!(
            "Use {} through your Skill tool; if you have no Skill tool, or a skill is not \
             available to you, carry on following what it is meant to do as best you can. ",
            quoted(&other)
        ));
    }
    format!(
        "\n## Skills\n\nThe user chose these skills for the {phase} phase: {}.\n\n\
         Use them in this order, and do the phase's work the way they direct. {how}\
         Everything else in this message still applies: what the phase must record, and how, is \
         unchanged.\n",
        quoted(&all)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_configured_renders_nothing() {
        assert_eq!(render_skills("review", &[], &[]), "");
    }

    #[test]
    fn skills_are_listed_in_order_for_their_phase() {
        let text = render_skills("review", &["security-review".into(), "simplify".into()], &[]);
        assert!(text.contains("review phase"), "{text}");
        assert!(text.find("`security-review`").unwrap() < text.find("`simplify`").unwrap());
        assert!(text.contains("Skill tool"));
        assert!(!text.contains("tod-cli skills show"), "{text}");
    }

    #[test]
    fn tods_own_skills_are_read_with_tod_cli() {
        let text = render_skills("pr", &["tod-open-pr".into(), "simplify".into()], &["tod-open-pr".into()]);
        assert!(text.contains("`tod-open-pr` is tod's own: read it with `tod-cli skills show"), "{text}");
        assert!(text.contains("Use `simplify` through your Skill tool"), "{text}");
        let only_own = render_skills("pr", &["tod-open-pr".into()], &["tod-open-pr".into()]);
        assert!(!only_own.contains("Skill tool;"), "{only_own}");
    }

    /// The skills this repository ships for lifecycle phases are in the bundle
    /// the sandboxes get, under the names a lifecycle config uses.
    #[test]
    fn the_pr_skills_ship_in_the_bundle() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/skills");
        let names = crate::skills::SkillsPaths::from_root(root).unwrap().names().unwrap();
        for name in ["tod-open-pr", "tod-fix-ci"] {
            assert!(names.iter().any(|n| n == name), "{name} missing from {names:?}");
        }
    }
}
