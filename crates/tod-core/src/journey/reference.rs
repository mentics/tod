//! How a journey is named when a person hands it to an agent
//! (`journey:node/<slug>`, `journey:project`), and the text they paste. The
//! agent reads it with `tod-cli journey show`; nothing about journeys is in
//! any agent's context, so the pasted text carries the command itself.

/// The reference for the user's own recent activity.
pub const PROJECT_REFERENCE: &str = "journey:project";

/// The reference for a node's journey.
pub fn node_reference(slug: &str) -> String {
    format!("journey:node/{slug}")
}

/// What `tod-cli journey show` takes for a reference: `journey:node/<slug>`
/// and `journey:project` give `<slug>` and `project`; anything else (a bare
/// slug or UUID) is returned unchanged.
pub fn target_of(reference: &str) -> &str {
    let target = reference.strip_prefix("journey:").unwrap_or(reference);
    target.strip_prefix("node/").unwrap_or(target)
}

/// The text to paste into a chat with an agent: the reference, and how to
/// read it, so an agent that has never heard of journeys can follow it.
pub fn agent_snippet(reference: &str) -> String {
    let target = target_of(reference);
    let what = if target == "project" { "the user's recent activity" } else { "a node's" };
    format!(
        "Journey ({what}): {reference}\n\
         Read it with `tod-cli journey show {target} --last 100` (`--since 2h` and `--last` \
         narrow it); `tod-cli skills show journeys` says what to look for."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_round_trip_to_their_target() {
        assert_eq!(target_of(&node_reference("fix-login")), "fix-login");
        assert_eq!(target_of(PROJECT_REFERENCE), "project");
        assert_eq!(target_of("fix-login"), "fix-login");
    }

    #[test]
    fn the_snippet_names_the_command_for_the_target() {
        let text = agent_snippet(&node_reference("fix-login"));
        assert!(text.contains("journey:node/fix-login"));
        assert!(text.contains("tod-cli journey show fix-login --last 100"));
        assert!(agent_snippet(PROJECT_REFERENCE).contains("journey show project"));
    }
}
