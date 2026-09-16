//! The registry of every static-context recipe, and the structural rules they
//! must satisfy.
//!
//! A *recipe* is one surface's ordered list of `media/context/` fragments. The
//! lists themselves live next to the code that launches each surface
//! ([`crate::agent_context`], [`crate::gate::context`]); this module collects
//! them so the invariants can be checked in one place, and so adding a surface
//! without registering it shows up as a failing test rather than as a prompt
//! nobody reads.
//!
//! See `doc/agent-context-map.md` for what each block category is for.

use crate::agent_context::{
    IMPLEMENT_CONTEXT_LAYERS, OBLIGATIONS_CONTEXT_LAYERS, VISUAL_DESIGN_CONTEXT_LAYERS,
};
use crate::gate::{GATE_CHECK_CONTEXT_LAYERS, ON_ENTRY_CONTEXT_LAYERS};

/// Every registered surface, as `(name, fragments)`.
pub const ALL_RECIPES: &[(&str, &[&str])] = &[
    ("obligations chat", OBLIGATIONS_CONTEXT_LAYERS),
    ("visual-design chat", VISUAL_DESIGN_CONTEXT_LAYERS),
    ("implementation session", IMPLEMENT_CONTEXT_LAYERS),
    ("gate check", GATE_CHECK_CONTEXT_LAYERS),
    ("on-entry", ON_ENTRY_CONTEXT_LAYERS),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// The in-repo `media/context/` directory, or `None` when the tests are
    /// running somewhere the source tree isn't available.
    fn context_root() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tod")
            .join("media")
            .join("context");
        root.is_dir().then_some(root)
    }

    /// Every `.md` under `context/`, as `/`-separated fragment keys.
    fn all_fragments(root: &PathBuf) -> BTreeSet<String> {
        fn walk(dir: &PathBuf, prefix: &str, out: &mut BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    walk(&path, &format!("{prefix}{name}/"), out);
                } else if let Some(stem) = name.strip_suffix(".md") {
                    out.insert(format!("{prefix}{stem}"));
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(root, "", &mut out);
        out
    }

    /// A typo in a recipe is silently skipped by `load_static_context`, so the
    /// only thing that catches it is this test.
    #[test]
    fn every_referenced_fragment_exists() {
        let Some(root) = context_root() else { return };
        let existing = all_fragments(&root);
        for (surface, layers) in ALL_RECIPES {
            for key in *layers {
                assert!(
                    existing.contains(*key),
                    "{surface} references missing fragment {key:?}; \
                     have: {existing:?}"
                );
            }
        }
    }

    /// Catches fragments orphaned by a split or rename.
    #[test]
    fn every_fragment_is_used_by_some_recipe() {
        let Some(root) = context_root() else { return };
        let used: BTreeSet<&str> = ALL_RECIPES
            .iter()
            .flat_map(|(_, layers)| layers.iter().copied())
            .collect();
        // Stances for surfaces that don't go through the media channel yet
        // (interview and drafting agents — see `doc/agent-context-map.md`
        // step 3).
        let pending = ["stance/agent-to-agent", "cli/drafting"];
        for key in all_fragments(&root) {
            assert!(
                used.contains(key.as_str()) || pending.contains(&key.as_str()),
                "fragment {key:?} is not used by any recipe"
            );
        }
    }

    /// Behavioural policy lives in exactly one place per prompt. Loading two
    /// stances is how the obligations chat ended up telling the agent both to
    /// confirm before editing and to edit without confirming.
    #[test]
    fn every_recipe_has_exactly_one_stance() {
        for (surface, layers) in ALL_RECIPES {
            let stances: Vec<_> = layers
                .iter()
                .filter(|k| k.starts_with("stance/"))
                .collect();
            assert_eq!(
                stances.len(),
                1,
                "{surface} must load exactly one stance fragment, got {stances:?}"
            );
        }
    }

    /// `cli/intro` explains `--data-root` and the no-raw-SQL rule that every
    /// noun section assumes, so a noun without it is a fragment out of context.
    #[test]
    fn cli_nouns_are_never_loaded_without_the_intro() {
        for (surface, layers) in ALL_RECIPES {
            let has_noun = layers
                .iter()
                .any(|k| k.starts_with("cli/") && *k != "cli/intro");
            if has_noun {
                assert!(
                    layers.contains(&"cli/intro"),
                    "{surface} loads a cli/ noun without cli/intro"
                );
            }
        }
    }

    /// `tod-cli` syntax is documented once, under `cli/`. A surface doc that
    /// spells out a command drifts from the reference the moment the command
    /// changes.
    #[test]
    fn tod_cli_syntax_appears_only_under_cli() {
        let Some(root) = context_root() else { return };
        for key in all_fragments(&root) {
            if key.starts_with("cli/") {
                continue;
            }
            let text = std::fs::read_to_string(root.join(format!("{key}.md"))).unwrap();
            assert!(
                !text.contains("tod-cli --data-root"),
                "{key}.md spells out a tod-cli invocation; \
                 document it under cli/ and reference it by name instead"
            );
        }
    }
}
