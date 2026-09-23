//! Rules every agent working in a codebase is given, whatever launched it.
//!
//! These are not part of any [`crate::context_recipes::ContextRecipe`]: they
//! follow from where the agent runs, not from its job, so they are attached
//! at the launch sites by working directory. The fragment is compiled in
//! rather than loaded from the media bundle, so these rules cannot be absent
//! from any build.

use std::path::Path;
use tod_store::fleet::Workdir;

/// `media/context/workspace/codebase.md`.
pub const CODEBASE_RULES: &str =
    include_str!("../../tod/media/context/workspace/codebase.md");

/// Whether `cwd` is inside a git checkout — any codebase, not only the
/// node's own.
pub fn is_codebase(cwd: &Path) -> bool {
    cwd.ancestors().any(|dir| dir.join(".git").exists())
}

/// `context` with [`CODEBASE_RULES`] appended when `cwd` is a codebase.
pub fn with_codebase_rules(context: String, cwd: &Path) -> String {
    append_rules(context, is_codebase(cwd))
}

/// [`with_codebase_rules`] for a directory that may be inside a dev
/// container. One there is the node's Files directory, always a checkout.
pub fn with_codebase_rules_in(context: String, cwd: &Workdir) -> String {
    match cwd {
        Workdir::Host(path) => with_codebase_rules(context, path),
        Workdir::Container { .. } => append_rules(context, true),
    }
}

fn append_rules(mut context: String, codebase: bool) -> String {
    if codebase {
        if !context.is_empty() {
            context.push_str("\n\n---\n\n");
        }
        context.push_str(CODEBASE_RULES.trim());
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_follow_the_working_directory() {
        let root = std::env::temp_dir().join(format!("tod-codebase-{}", uuid::Uuid::new_v4()));
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(with_codebase_rules("ctx".into(), &plain), "ctx");

        let repo = root.join("repo");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        // A worktree's `.git` is a file, not a directory.
        std::fs::write(repo.join(".git"), "gitdir: elsewhere").unwrap();
        let out = with_codebase_rules("ctx".into(), &nested);
        assert!(out.starts_with("ctx"));
        assert!(out.contains("dev container"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
