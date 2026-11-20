//! The task panel's Guidance section (find-it-first section 12): what the
//! selector, the skills' own triggers and the user picked of the project's
//! skills, files, lenses and checks for this node, each with its reason, and
//! the user's Remove and Add. The selection itself is `tod_store::guidance`;
//! every implement, verify, review and fix session's opening carries it.
//!
//! What is shown and offered is a pure function of the store
//! ([`GuidanceView::load`], [`GuidanceView::presented`]) so it is tested
//! without a window; `TaskPanel` renders it and records each click as a
//! journey `UserAction` with this section as presented.

use tod_journey::{Presented, PresentedAction};
use tod_store::fleet::FleetStore;
use tod_store::guidance::{self as store, Guidance, KIND_LENS};
use uuid::Uuid;

/// The section's content for one node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GuidanceView {
    /// The live selection, lenses that do not apply included.
    pub live: Vec<Guidance>,
    /// What the user may add: the latest run's candidates and every lens,
    /// less what is selected, as `(kind, item)`.
    pub addable: Vec<(String, String)>,
    /// Whether a selection has run for this node yet.
    pub has_run: bool,
    /// Reading the selection failed: said in the section, never hidden.
    pub error: Option<String>,
}

impl GuidanceView {
    pub(crate) fn load(fleet: &FleetStore, node: Uuid) -> Self {
        match fleet.read(|conn| {
            let live = store::live(conn, node)?;
            let runs = store::runs(conn, node)?;
            Ok((live, runs))
        }) {
            Ok((live, runs)) => Self::from_rows(live, &runs),
            Err(err) => Self { error: Some(format!("Could not read this task's guidance: {err:#}")), ..Self::default() },
        }
    }

    fn from_rows(live: Vec<Guidance>, runs: &[store::Run]) -> Self {
        let candidates = runs.last().map(|r| r.candidates.clone()).unwrap_or_default();
        let mut offered: Vec<(String, String)> = Vec::new();
        for (kind, items) in [
            (store::KIND_SKILL, &candidates.skills),
            (store::KIND_FILE, &candidates.files),
            (store::KIND_CHECK, &candidates.checks),
        ] {
            offered.extend(items.iter().map(|i| (kind.to_string(), i.clone())));
        }
        offered.extend(tod_store::review::LENSES.iter().map(|l| (KIND_LENS.to_string(), l.slug.to_string())));
        // A lens recorded as not applying can still be added as applying.
        let addable = offered
            .into_iter()
            .filter(|(kind, item)| !live.iter().any(|g| &g.kind == kind && &g.item == item && g.applies))
            .collect();
        Self { live, addable, has_run: !runs.is_empty(), error: None }
    }

    /// The rows as shown: the items that apply first, then the lenses that
    /// do not.
    pub(crate) fn rows(&self) -> Vec<&Guidance> {
        let mut rows: Vec<&Guidance> = self.live.iter().filter(|g| g.applies).collect();
        rows.extend(self.live.iter().filter(|g| !g.applies));
        rows
    }

    /// The section's actions as presented, for the journey: each row's
    /// Remove, then each Add entry.
    pub(crate) fn presented(&self) -> Presented {
        let mut actions: Vec<PresentedAction> = self
            .rows()
            .into_iter()
            .map(|g| PresentedAction {
                id: remove_id(&g.kind, &g.item),
                label: "Remove".to_string(),
                primary: false,
                disabled: false,
            })
            .collect();
        actions.extend(self.addable.iter().map(|(kind, item)| PresentedAction {
            id: add_id(kind, item),
            label: add_label(kind, item),
            primary: false,
            disabled: false,
        }));
        Presented { actions, focused: None, notices: vec![format!("{} selected", self.live.len())] }
    }
}

pub(crate) fn remove_id(kind: &str, item: &str) -> String {
    format!("GuidanceRemove({kind}:{item})")
}

pub(crate) fn add_id(kind: &str, item: &str) -> String {
    format!("GuidanceAdd({kind}:{item})")
}

pub(crate) fn add_label(kind: &str, item: &str) -> String {
    format!("{kind} {item}")
}

/// One row's text: kind and item, why, who picked it and at which stage.
pub(crate) fn row_text(g: &Guidance) -> String {
    let what = if g.applies { g.kind.clone() } else { format!("{} (does not apply)", g.kind) };
    format!("{what} {} — {} ({}, {})", g.item, g.reason, g.selected_by_label(), g.stage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::guidance::{Candidates, Run};

    fn row(kind: &str, item: &str, applies: bool, by: &str) -> Guidance {
        Guidance {
            id: Uuid::new_v4(),
            node_id: Uuid::nil(),
            item: item.into(),
            kind: kind.into(),
            applies,
            reason: "because".into(),
            stage: "actual".into(),
            change_ref: "abc".into(),
            selected_by: by.into(),
            run_id: None,
            created_at: 1,
            last_selected_at: 1,
            removed_at: None,
            removed_by: None,
        }
    }

    fn run(candidates: Candidates) -> Run {
        Run {
            id: Uuid::new_v4(),
            node_id: Uuid::nil(),
            stage: "actual".into(),
            change_ref: "abc".into(),
            areas: Vec::new(),
            candidates,
            material: String::new(),
            conversation_id: None,
            outcome: "finished".into(),
            started_at: 1,
            finished_at: Some(2),
            added: Vec::new(),
            dropped: Vec::new(),
        }
    }

    #[test]
    fn the_section_lists_what_applies_first_and_offers_only_what_is_not_selected() {
        let lens = tod_store::review::LENSES[0].slug;
        let live = vec![
            row(KIND_LENS, lens, false, "conversation:x"),
            row("skill", "style", true, "app"),
        ];
        let candidates = Candidates {
            skills: vec!["style".into(), "sql".into()],
            files: vec!["REVIEW.md".into()],
            checks: Vec::new(),
        };
        let view = GuidanceView::from_rows(live, &[run(candidates)]);
        let shown: Vec<&str> = view.rows().iter().map(|g| g.item.as_str()).collect();
        assert_eq!(shown, ["style", lens]);
        assert_eq!(row_text(view.rows()[0]), "skill style — because (its triggers, actual)");
        assert!(row_text(view.rows()[1]).starts_with("lens (does not apply)"));
        assert!(view.addable.contains(&("skill".into(), "sql".into())));
        assert!(view.addable.contains(&("file".into(), "REVIEW.md".into())));
        assert!(!view.addable.contains(&("skill".into(), "style".into())));
        // A lens that does not apply may still be added as applying.
        assert!(view.addable.contains(&(KIND_LENS.into(), lens.into())));

        let presented = view.presented();
        assert_eq!(presented.actions[0].id, "GuidanceRemove(skill:style)");
        assert!(presented.actions.iter().any(|a| a.id == "GuidanceAdd(skill:sql)" && a.label == "skill sql"));
        assert!(view.has_run);
        assert!(!GuidanceView::from_rows(Vec::new(), &[]).has_run);
    }
}
