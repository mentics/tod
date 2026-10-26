//! What a lifecycle agent usually does with `tod-cli`, by role and state.
//!
//! `media/context/cli/usual-actions.yml` lists, for each role (the phase agent,
//! the evaluator) and lifecycle state, the commands that situation most likely
//! needs. [`render`] turns the entries that apply into a section of the agent's
//! opening, so it starts from those instead of working them out from every
//! noun's reference.
//!
//! It is guidance for efficiency, not enforcement: nothing here limits what an
//! agent may run. The syntax stays documented once, in the `cli/` fragments;
//! an entry only names a command and says what is particular to the situation
//! (`tod-cli`'s `doc_sync` tests check that each entry names a real one).
//!
//! The file ships with the app. It is shaped so it could later live in the data
//! root and be edited there; nothing reads it from there today.

use crate::media::MediaPaths;
use anyhow::{Context, Result};
use serde::Deserialize;

/// Where the table is, under `media/context/`.
pub const FILE: &str = "cli/usual-actions.yml";

/// Who is acting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The state's phase agent.
    Phase,
    /// The independent evaluator of a phase.
    Evaluate,
}

/// One command an agent usually runs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// What follows `tod-cli --data-root <DATA_ROOT>`: a real `<noun> <verb>`
    /// and the flags that matter here.
    pub run: String,
    /// What is particular to this situation; the command's reference says the rest.
    #[serde(default)]
    pub say: String,
}

/// A group of actions that apply under the same conditions.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub role: Role,
    /// The states it applies in; none means every state.
    #[serde(default)]
    pub states: Vec<String>,
    /// Whether it applies only with independent evaluation on or off; none
    /// means either.
    #[serde(default)]
    pub independent_evaluation: Option<bool>,
    pub title: String,
    pub actions: Vec<Action>,
}

impl Entry {
    fn applies(&self, role: Role, state: &str, independent: bool) -> bool {
        self.role == role
            && (self.states.is_empty() || self.states.iter().any(|s| s == state))
            && self.independent_evaluation.is_none_or(|on| on == independent)
    }
}

/// Parse the table's text.
pub fn parse(text: &str) -> Result<Vec<Entry>> {
    serde_yaml::from_str(text).context("could not parse the usual-actions table")
}

/// Read the table that ships in the media bundle.
pub fn load(paths: &MediaPaths) -> Result<Vec<Entry>> {
    let path = paths.context_root().join(FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read the usual-actions table {}", path.display()))?;
    parse(&text).with_context(|| format!("in {}", path.display()))
}

/// The section for an agent in `role` on a node in `state`, or `None` when the
/// table has nothing for that situation.
pub fn render_entries(
    entries: &[Entry],
    role: Role,
    state: &str,
    independent: bool,
) -> Option<String> {
    let applying: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.applies(role, state, independent))
        .collect();
    if applying.is_empty() {
        return None;
    }
    let mut out = format!(
        "## Usual actions in `{state}`\n\n\
         Where to start, not a limit: these are the commands this situation usually \
         needs. Use another when the work calls for it. Their syntax is in the \
         reference above.\n"
    );
    for entry in applying {
        out.push_str(&format!("\n**{}**\n\n", entry.title));
        for action in &entry.actions {
            out.push_str(&format!(
                "- `tod-cli --data-root <DATA_ROOT> {}`",
                action.run
            ));
            if !action.say.is_empty() {
                out.push_str(&format!(" — {}", action.say));
            }
            out.push('\n');
        }
    }
    Some(out)
}

/// [`render_entries`] over the table in the media bundle.
pub fn render(
    paths: &MediaPaths,
    role: Role,
    state: &str,
    independent: bool,
) -> Result<Option<String>> {
    Ok(render_entries(&load(paths)?, role, state, independent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::PHASE_AGENT_STATES;
    use tod_store::phase::CERTIFIABLE_STATES;

    fn shipped() -> Vec<Entry> {
        parse(include_str!("../../tod/media/context/cli/usual-actions.yml")).unwrap()
    }

    #[test]
    fn ready_is_offered_only_with_independent_evaluation() {
        let entries = shipped();
        let on = render_entries(&entries, Role::Phase, "released", true).unwrap();
        assert!(on.contains("phase ready"));
        assert!(on.contains("no note"));
        assert!(!on.contains("phase certify"));
        let off = render_entries(&entries, Role::Phase, "released", false).unwrap();
        assert!(off.contains("phase certify --note"));
        assert!(!off.contains("phase ready"));
    }

    #[test]
    fn every_phase_agent_state_has_actions_beyond_the_shared_ones() {
        let entries = shipped();
        for state in PHASE_AGENT_STATES {
            let state_specific = entries.iter().any(|e| {
                e.role == Role::Phase && e.states.iter().any(|s| s == state)
            });
            assert!(state_specific, "no phase-agent actions for `{state}`");
        }
    }

    #[test]
    fn the_evaluator_is_shown_its_verdict_commands_in_every_certifiable_state() {
        let entries = shipped();
        for state in CERTIFIABLE_STATES {
            let text = render_entries(&entries, Role::Evaluate, state, true)
                .unwrap_or_else(|| panic!("nothing for the evaluator in `{state}`"));
            assert!(text.contains("phase certify") && text.contains("phase reject"));
            assert!(!text.contains("phase ready"));
        }
    }

    #[test]
    fn every_state_named_is_a_lifecycle_state_the_role_works() {
        for entry in shipped() {
            let worked: &[&str] = match entry.role {
                Role::Phase => &PHASE_AGENT_STATES,
                Role::Evaluate => &CERTIFIABLE_STATES,
            };
            for state in &entry.states {
                assert!(
                    worked.contains(&state.as_str()),
                    "{:?} entry `{}` names `{state}`, which that role does not work",
                    entry.role,
                    entry.title
                );
            }
        }
    }

    #[test]
    fn an_unknown_field_is_an_error() {
        let err = parse("- role: phase\n  title: t\n  actions: []\n  extra: 1\n").unwrap_err();
        assert!(format!("{err:#}").contains("extra"));
    }
}
