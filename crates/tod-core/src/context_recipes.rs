//! The registry of every agent-context recipe, the one assembler that renders
//! them, and the structural rules they must satisfy.
//!
//! A *recipe* names one surface's static fragments (from `media/context/`) and
//! its dynamic blocks. Everything else about assembling a first message —
//! ordering, separators, where the process-bundle role doc goes — is the same
//! for every surface and lives in [`build_message`].
//!
//! See `doc/agent-context-map.md` for what each block category is for and
//! which surfaces are still outside this registry.

use crate::dynamic::{DynamicBlock, DynamicContext};
use crate::media::{MediaPaths, load_static_context};
use anyhow::Result;

/// One surface's context: which static fragments, and which dynamic blocks.
#[derive(Debug, Clone, Copy)]
pub struct ContextRecipe {
    /// Human-readable surface name, for diagnostics and the guardrail tests.
    pub name: &'static str,
    /// Static fragments under `media/context/`, in order. Categories are
    /// assembled stance → domain → cli → surface.
    pub layers: &'static [&'static str],
    /// Dynamic blocks to render after them, in order.
    pub blocks: &'static [DynamicBlock],
}

/// Assemble a first-turn message: static fragments, the process-bundle role
/// doc when the surface has one, the dynamic blocks, then any surface-specific
/// trailing instruction.
///
/// `role_doc` is passed in rather than read here (it needs the bundled process
/// root) so this module stays free of `TodInstallPaths`/`ProcessManifest`
/// concerns — the same split `gate::context` has always used.
pub fn build_message(
    paths: &MediaPaths,
    recipe: &ContextRecipe,
    role_doc: Option<&str>,
    ctx: &DynamicContext<'_>,
    tail: &str,
) -> Result<String> {
    let mut out = load_static_context(paths, recipe.layers)?;

    if let Some(doc) = role_doc.map(str::trim).filter(|d| !d.is_empty()) {
        out.push_str("\n\n---\n\n");
        out.push_str(doc);
    }

    out.push_str("\n\n---\n\n");
    out.push_str(&crate::dynamic::render(recipe.blocks, ctx));

    if !tail.is_empty() {
        out.push_str(tail);
    }

    Ok(out)
}

/// Wording shared by the surfaces that inline a node's own obligations
/// alongside summarized ancestor context, so the distinction is stated the
/// same way everywhere it matters.
const OWN_OBLIGATIONS_NOTE: &str = "This node's own. They define what is in scope here — not the summarized \
     ancestor context below, whose requirements are settled and whose \
     constraints still bind.";

/// The visual-design chat. It mutates through exactly one command, so it loads
/// `cli/visual-design` and none of the other nouns. An obligation is always
/// selected here (the chat is scoped to one), hence no fallback.
pub const VISUAL_DESIGN_CHAT: ContextRecipe = ContextRecipe {
    name: "visual-design chat",
    layers: &[
        "stance/interactive-chat",
        "domain/outline",
        "domain/obligations",
        "cli/intro",
        "cli/visual-design",
        "surface/visual-design",
    ],
    blocks: &[
        DynamicBlock::DataRoot,
        DynamicBlock::Node,
        DynamicBlock::AncestorContext,
        DynamicBlock::SelectedObligation { fallback: "" },
    ],
};

/// An implementation session, launched from the lifecycle panel's Active-phase
/// "Implement" button (see `crate::gate` for the transition gate that requires
/// Agent and Files before a node can reach `active` at all).
///
/// Stance is `autonomous-session`, not `interactive-chat`: nobody is waiting at
/// a prompt, so the agent must act without confirming and may be as verbose as
/// the work needs.
pub const IMPLEMENT_SESSION: ContextRecipe = ContextRecipe {
    name: "implementation session",
    layers: &[
        "stance/autonomous-session",
        "domain/outline",
        "domain/obligations",
        "domain/plan",
        "domain/lifecycle",
        "cli/intro",
        "cli/obligations",
        "cli/plan",
        "surface/implement",
    ],
    blocks: &[
        DynamicBlock::DataRoot,
        DynamicBlock::Node,
        DynamicBlock::Plan,
        DynamicBlock::NodeObligations {
            note: OWN_OBLIGATIONS_NOTE,
        },
        DynamicBlock::AncestorContext,
    ],
};

/// A gate-check turn. Stance is `one-shot`: a single structured-response turn
/// that must not end by asking a question. It loads no `cli/` fragments at all
/// — it returns YAML and the app persists the result, so it never mutates
/// anything. It does load `domain/capabilities`, because gate criteria
/// routinely turn on which capabilities a node has.
pub const GATE_CHECK: ContextRecipe = ContextRecipe {
    name: "gate check",
    layers: &[
        "stance/one-shot",
        "domain/outline",
        "domain/obligations",
        "domain/lifecycle",
        "domain/capabilities",
        "domain/plan",
        "surface/gate-check",
    ],
    blocks: &[
        DynamicBlock::DataRoot,
        DynamicBlock::Node,
        DynamicBlock::NodeProcessFields,
        DynamicBlock::NodeObligations {
            note: "This node's own — evaluate the gate and probe questions against \
                   these, not against the ancestor context below.",
        },
        DynamicBlock::AncestorContext,
        DynamicBlock::Plan,
    ],
};

/// An on-entry turn. Unlike gate-check this turn does real work (e.g.
/// `planning` writing plan steps), so its stance is `autonomous-session` and
/// it loads the CLI nouns it writes through.
pub const ON_ENTRY: ContextRecipe = ContextRecipe {
    name: "on-entry",
    layers: &[
        "stance/autonomous-session",
        "domain/outline",
        "domain/obligations",
        "domain/lifecycle",
        "domain/plan",
        "cli/intro",
        "cli/obligations",
        "cli/plan",
        "surface/on-entry",
    ],
    blocks: GATE_CHECK.blocks,
};

/// An autonomous fleet run: a state agent launched into a worktree to work a
/// node on its own. Until now this got the state role doc and nothing else —
/// no domain model and no `tod-cli` reference, despite its role docs telling
/// it to use `tod-cli`.
///
pub const FLEET_AUTONOMOUS: ContextRecipe = ContextRecipe {
    name: "fleet autonomous run",
    layers: &[
        "stance/autonomous-session",
        "domain/outline",
        "domain/obligations",
        "domain/lifecycle",
        "cli/intro",
        "cli/node",
        "cli/obligations",
        "cli/plan",
    ],
    blocks: &[
        DynamicBlock::DataRoot,
        DynamicBlock::Node,
        DynamicBlock::Workspace,
    ],
};

/// An interview agent turn (question-maker or answer-processor). This recipe
/// contributes static fragments only; the dynamic half is
/// `interview::context::{snapshot, delta}`, whose sections are single-use and
/// phase- and role-filtered, so they are not blocks. See "Decisions" in
/// `doc/agent-context-map.md`.
pub const INTERVIEW_AGENT: ContextRecipe = ContextRecipe {
    name: "interview agent",
    layers: &[
        "stance/agent-to-agent",
        "domain/outline",
        "domain/obligations",
        "domain/plan",
        "cli/intro",
        "cli/obligations",
        "cli/content",
        "cli/plan",
        "cli/questions",
        "cli/memory",
        "cli/interview",
    ],
    blocks: &[],
};

/// The conversation view's agent. Interactive, with the whole project in
/// scope: `surface/conversation` states its exception to "confirm first"
/// (every action is recorded and reversible) and its reply rule. The focus is
/// only where the conversation starts.
pub const CONVERSATION: ContextRecipe = ContextRecipe {
    name: "conversation",
    layers: &[
        "stance/interactive-chat",
        "domain/outline",
        "domain/obligations",
        "domain/plan",
        "domain/lifecycle",
        "cli/intro",
        "cli/node",
        "cli/obligations",
        "cli/plan",
        "cli/content",
        "cli/changeset",
        "surface/conversation",
    ],
    blocks: &[DynamicBlock::DataRoot, DynamicBlock::Focus],
};

/// A chat opened on one item from the action panel. Read-only: it loads the
/// same nouns as `CONVERSATION` so the agent can look anything up, and
/// `surface/chat` states the exception that it must not write with them. It
/// has no change set, so it loads no `cli/changeset` either.
pub const NODE_CHAT: ContextRecipe = ContextRecipe {
    name: "node chat",
    layers: &[
        "stance/interactive-chat",
        "domain/outline",
        "domain/obligations",
        "domain/plan",
        "domain/lifecycle",
        "cli/intro",
        "cli/node",
        "cli/obligations",
        "cli/plan",
        "cli/content",
        "surface/chat",
    ],
    blocks: &[DynamicBlock::DataRoot, DynamicBlock::Focus],
};

/// Every registered surface.
pub const ALL_RECIPES: &[ContextRecipe] = &[
    CONVERSATION,
    NODE_CHAT,
    VISUAL_DESIGN_CHAT,
    IMPLEMENT_SESSION,
    GATE_CHECK,
    ON_ENTRY,
    FLEET_AUTONOMOUS,
    INTERVIEW_AGENT,
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
        for recipe in ALL_RECIPES {
            for key in recipe.layers {
                assert!(
                    existing.contains(*key),
                    "{} references missing fragment {key:?}; have: {existing:?}",
                    recipe.name
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
            .flat_map(|r| r.layers.iter().copied())
            .collect();
        for key in all_fragments(&root) {
            assert!(
                used.contains(key.as_str()),
                "fragment {key:?} is not used by any recipe"
            );
        }
    }

    /// Behavioural policy lives in exactly one place per prompt. Loading two
    /// stances is how the (since removed) obligations chat ended up telling
    /// the agent both to confirm before editing and to edit without
    /// confirming.
    #[test]
    fn every_recipe_has_exactly_one_stance() {
        for recipe in ALL_RECIPES {
            let stances: Vec<_> = recipe
                .layers
                .iter()
                .filter(|k| k.starts_with("stance/"))
                .collect();
            assert_eq!(
                stances.len(),
                1,
                "{} must load exactly one stance fragment, got {stances:?}",
                recipe.name
            );
        }
    }

    /// `cli/intro` explains `--data-root` and the no-raw-SQL rule that every
    /// noun section assumes, so a noun without it is a fragment out of context.
    #[test]
    fn cli_nouns_are_never_loaded_without_the_intro() {
        for recipe in ALL_RECIPES {
            let has_noun = recipe
                .layers
                .iter()
                .any(|k| k.starts_with("cli/") && *k != "cli/intro");
            if has_noun {
                assert!(
                    recipe.layers.contains(&"cli/intro"),
                    "{} loads a cli/ noun without cli/intro",
                    recipe.name
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

    /// The same rule over `assets/process/`. The interview role docs (and the
    /// since-removed ones for the old spec-writing agent) carried their own command table, abbreviated and already
    /// drifted from the binary (the interview one omitted `obligations add
    /// --phase`, which is required). They now point at the `cli/` fragments
    /// their recipes load.
    ///
    /// A fenced block is the tell: prose may still say "change them with
    /// `tod-cli plan`".
    #[test]
    fn process_docs_do_not_carry_their_own_command_tables() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("process");
        if !root.is_dir() {
            return;
        }
        let nouns = [
            "node ",
            "obligations ",
            "plan ",
            "content ",
            "questions ",
            "memory ",
            "interview ",
            "visual-design ",
            "changeset ",
        ];
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let mut fenced = false;
                for line in text.lines() {
                    if line.trim_start().starts_with("```") {
                        fenced = !fenced;
                        continue;
                    }
                    if !fenced {
                        continue;
                    }
                    assert!(
                        !nouns.iter().any(|n| line.starts_with(n)),
                        "{} has a tod-cli command table ({line:?}); \
                         the cli/ fragments are the one reference — point at them",
                        path.display()
                    );
                }
            }
        }
    }

    /// A recipe that renders obligations without `AncestorContext` would claim
    /// "not the ancestor context below" with nothing below it.
    #[test]
    fn own_obligations_are_always_followed_by_ancestor_context() {
        for recipe in ALL_RECIPES {
            let obligations = recipe
                .blocks
                .iter()
                .position(|b| matches!(b, DynamicBlock::NodeObligations { .. }));
            let Some(idx) = obligations else { continue };
            let ancestors = recipe
                .blocks
                .iter()
                .position(|b| *b == DynamicBlock::AncestorContext);
            assert_eq!(
                ancestors,
                Some(idx + 1),
                "{} must render AncestorContext directly after NodeObligations",
                recipe.name
            );
        }
    }

    /// A surface that tells the agent to pass `--data-root` to `tod-cli` but
    /// never says what it is sends it looking for the value.
    ///
    /// Recipes with no blocks render their dynamic half elsewhere (the
    /// interview snapshots, which emit their own `Data root:` line) and are out of scope here.
    #[test]
    fn recipes_that_load_cli_fragments_render_the_data_root() {
        for recipe in ALL_RECIPES {
            if recipe.blocks.is_empty() || !recipe.layers.iter().any(|k| k.starts_with("cli/")) {
                continue;
            }
            assert!(
                recipe.blocks.contains(&DynamicBlock::DataRoot),
                "{} documents tod-cli but never renders the data root",
                recipe.name
            );
        }
    }
}
