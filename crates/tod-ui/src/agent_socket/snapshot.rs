//! View snapshots for driven/user mode: what the window looked like to one
//! party, so the other party's use of the app can be undone *visually*
//! without touching anyone's data.
//!
//! A snapshot holds only view state: the window's content size, the active
//! shell view, the workbench's tree selection, columns, pins, focused column
//! and chat drawer, and the text of the input being edited. It also holds a
//! [`Fingerprint`] of the data on screen, which the agent's snapshot is
//! compared against when it gets the app back, so a command that depended on
//! something the user changed is refused with the difference named.
//!
//! Not captured: scroll offsets (the panels keep their own scroll handles
//! and there is no registry to read them from; the tree keeps its selection
//! in view), text selections, transient menus and popovers (closed on every
//! switch instead), and the window's position.

use std::collections::BTreeSet;

use gpui::{AnyWindowHandle, App, WeakEntity, Window};
use gpui_component::WindowExt;
use gpui_component::input::AnyInputState;
use uuid::Uuid;

use crate::app::window::{Shell, ShellViewState};
use crate::unified::PanelKind;
use crate::unified::view_state::{NodeFacts, NodeRead, PanelFacts};

/// The data on screen when a snapshot was taken (or, compared against it,
/// now): for the selected node, that it exists, its lifecycle state, who is
/// next, and its pending requests; for each open panel, that it applies.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fingerprint {
    pub node: Option<NodeFacts>,
    pub panels: Vec<PanelFacts>,
}

fn short(id: Uuid) -> String {
    id.simple().to_string()[..4].to_string()
}

impl Fingerprint {
    /// What differs between `self` (saved) and `now` (taken for the same node
    /// and panels), one phrase each, e.g. `request 3f2a answered`,
    /// `ENG-2925 pr_draft→approved`, `FindingsPanel (ENG-2925) no longer
    /// applies`. Empty when nothing the agent depended on changed.
    pub fn conflicts(&self, now: &Fingerprint) -> Vec<String> {
        let mut out = Vec::new();
        if let (Some(a), Some(b)) = (&self.node, &now.node) {
            let name = &a.name;
            let mut present = true;
            match (&a.read, &b.read) {
                (NodeRead::Lifecycle(_), NodeRead::Missing) => {
                    out.push(format!("{name} deleted"));
                    present = false;
                }
                (NodeRead::Lifecycle(x), NodeRead::Lifecycle(y)) if x != y => {
                    out.push(format!("{name} {x}→{y}"));
                }
                (_, NodeRead::Unreadable(err)) => {
                    out.push(format!("could not read {name}: {err}"));
                    present = false;
                }
                (NodeRead::Missing, NodeRead::Lifecycle(y)) => {
                    out.push(format!("{name} exists again ({y})"));
                }
                _ => {}
            }
            if present {
                if a.run_state != b.run_state {
                    out.push(format!("{name} run state {}→{}", a.run_state, b.run_state));
                }
                let gone: BTreeSet<_> = a.requests.difference(&b.requests).collect();
                let new: BTreeSet<_> = b.requests.difference(&a.requests).collect();
                for id in gone {
                    out.push(format!("request {} answered", short(*id)));
                }
                for id in new {
                    out.push(format!("request {} asked", short(*id)));
                }
            }
        }
        for (before, after) in self.panels.iter().zip(&now.panels) {
            if before.applies && !after.applies {
                out.push(format!("{} no longer applies", before.label));
            }
        }
        out
    }
}

/// The text of an input being edited.
#[derive(Clone)]
struct InputDraft {
    state: AnyInputState,
    text: gpui::SharedString,
}

impl InputDraft {
    fn capture(window: &mut Window, cx: &mut App) -> Option<Self> {
        let state = window.focused_input(cx)?;
        match state {
            AnyInputState::Input(_) | AnyInputState::Textarea(_) => {}
            // A code editor or a one-time-code field is not restored.
            _ => return None,
        }
        let text = state.value(cx);
        Some(Self { state, text })
    }

    fn restore(&self, window: &mut Window, cx: &mut App) {
        match &self.state {
            AnyInputState::Input(entity) => entity.update(cx, |input, cx| {
                if input.value() != self.text {
                    input.set_value(self.text.clone(), window, cx);
                }
            }),
            AnyInputState::Textarea(entity) => entity.update(cx, |input, cx| {
                if input.value() != self.text {
                    input.set_value(self.text.clone(), window, cx);
                }
            }),
            _ => return,
        }
        self.state.focus_handle(cx).focus(window, cx);
    }
}

/// One party's view of the app.
#[derive(Clone)]
pub struct ViewSnapshot {
    /// The window's content size, logical pixels.
    size: (f32, f32),
    shell: ShellViewState,
    input: Option<InputDraft>,
    panels: Vec<PanelKind>,
    pub fingerprint: Fingerprint,
}

impl ViewSnapshot {
    /// The content size this view had.
    pub fn size(&self) -> (f32, f32) {
        self.size
    }

    /// One line saying what the view holds (the `test-user peek` seam).
    pub fn summary(&self) -> String {
        format!(
            "size={}x{} view={:?} draft={:?}",
            self.size.0.round(),
            self.size.1.round(),
            self.shell.unified,
            self.input.as_ref().map(|i| i.text.to_string()),
        )
    }

    pub fn with_size(mut self, size: (f32, f32)) -> Self {
        self.size = size;
        self
    }
}

/// The two parties' snapshots. `agent` is `Err` when taking it failed, which
/// is then reported as a conflict instead of being dropped.
#[derive(Default)]
pub struct Snapshots {
    pub agent: Option<Result<ViewSnapshot, String>>,
    pub user: Option<ViewSnapshot>,
}

pub fn capture(
    shell: &WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut App,
) -> Result<ViewSnapshot, String> {
    let viewport = window.viewport_size();
    let input = InputDraft::capture(window, cx);
    let (view, node, panels, kinds) = shell
        .read_with(cx, |shell, cx| {
            let view = shell.shepherd_view_state(cx);
            let node = view
                .unified
                .node()
                .map(|id| shell.shepherd_node_facts(id, cx));
            let kinds = view.unified.panels();
            let panels: Vec<PanelFacts> = kinds
                .iter()
                .map(|panel| shell.shepherd_panel_facts(*panel, cx))
                .collect();
            (view, node, panels, kinds)
        })
        .map_err(|err| format!("the shell is gone: {err}"))?;
    Ok(ViewSnapshot {
        size: (f32::from(viewport.width), f32::from(viewport.height)),
        shell: view,
        input,
        panels: kinds,
        fingerprint: Fingerprint { node, panels },
    })
}

/// The data on screen now, for the node and panels `saved` was about.
pub fn fingerprint_now(
    shell: &WeakEntity<Shell>,
    saved: &ViewSnapshot,
    cx: &App,
) -> Result<Fingerprint, String> {
    shell
        .read_with(cx, |shell, cx| Fingerprint {
            node: saved
                .fingerprint
                .node
                .as_ref()
                .map(|node| shell.shepherd_node_facts(node.id, cx)),
            panels: saved
                .panels
                .iter()
                .map(|panel| shell.shepherd_panel_facts(*panel, cx))
                .collect(),
        })
        .map_err(|err| format!("the shell is gone: {err}"))
}

/// Put the window's view back as `snapshot` says (not its size: the driver
/// sets that, marking the resize as the app's own, see `ViewSnapshot::size`). `with_input` is false when the
/// text field it was editing must stay blurred (the user takes over).
pub fn restore(
    shell: &WeakEntity<Shell>,
    snapshot: &ViewSnapshot,
    with_input: bool,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    shell
        .update(cx, |shell, cx| shell.shepherd_restore(&snapshot.shell, window, cx))
        .map_err(|err| format!("the shell is gone: {err}"))?;
    if with_input {
        if let Some(draft) = &snapshot.input {
            draft.restore(window, cx);
        }
    }
    Ok(())
}

/// Whether `handle` is the window the snapshots are of.
pub fn is_window(handle: AnyWindowHandle, window: &Window) -> bool {
    window.window_handle() == handle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u128, lifecycle: &str, run: &str, requests: &[u128]) -> NodeFacts {
        NodeFacts {
            id: Uuid::from_u128(id),
            name: "ENG-2925".into(),
            read: NodeRead::Lifecycle(lifecycle.into()),
            run_state: run.into(),
            requests: requests.iter().map(|r| Uuid::from_u128(*r)).collect(),
        }
    }

    fn print(node: NodeFacts, panels: Vec<PanelFacts>) -> Fingerprint {
        Fingerprint {
            node: Some(node),
            panels,
        }
    }

    fn panel(label: &str, applies: bool) -> PanelFacts {
        PanelFacts {
            label: label.into(),
            applies,
        }
    }

    #[test]
    fn nothing_changed_nothing_to_report() {
        let a = print(node(1, "pr_draft", "you", &[7]), vec![panel("TaskPanel (ENG-2925)", true)]);
        assert!(a.conflicts(&a.clone()).is_empty());
    }

    #[test]
    fn names_each_difference() {
        let id = Uuid::from_u128(0x3f2a_0000_0000_0000_0000_0000_0000_0000);
        let saved = print(
            NodeFacts {
                requests: [id].into_iter().collect(),
                ..node(1, "pr_draft", "you", &[])
            },
            vec![
                panel("TaskPanel (ENG-2925)", true),
                panel("FindingsPanel (ENG-2925)", true),
            ],
        );
        let now = print(
            node(1, "approved", "others", &[]),
            vec![
                panel("TaskPanel (ENG-2925)", true),
                panel("FindingsPanel (ENG-2925)", false),
            ],
        );
        assert_eq!(
            saved.conflicts(&now),
            vec![
                "ENG-2925 pr_draft→approved".to_string(),
                "ENG-2925 run state you→others".to_string(),
                "request 3f2a answered".to_string(),
                "FindingsPanel (ENG-2925) no longer applies".to_string(),
            ]
        );
    }

    #[test]
    fn a_new_request_is_reported() {
        let saved = print(node(1, "design", "working", &[]), vec![]);
        let now = print(node(1, "design", "working", &[0xabcd_0000_0000_0000_0000_0000_0000_0000]), vec![]);
        assert_eq!(saved.conflicts(&now), vec!["request abcd asked".to_string()]);
    }

    #[test]
    fn a_deleted_node_is_one_difference_not_many() {
        let saved = print(node(1, "design", "working", &[5]), vec![panel("PlanPanel (ENG-2925)", true)]);
        let mut gone = node(1, "design", "none", &[]);
        gone.read = NodeRead::Missing;
        let now = print(gone, vec![panel("PlanPanel (ENG-2925)", false)]);
        assert_eq!(
            saved.conflicts(&now),
            vec![
                "ENG-2925 deleted".to_string(),
                "PlanPanel (ENG-2925) no longer applies".to_string(),
            ]
        );
    }

    #[test]
    fn an_unreadable_node_is_never_taken_for_unchanged() {
        let saved = print(node(1, "design", "working", &[]), vec![]);
        let mut broken = node(1, "design", "working", &[]);
        broken.read = NodeRead::Unreadable("database is locked".into());
        let now = print(broken, vec![]);
        assert_eq!(
            saved.conflicts(&now),
            vec!["could not read ENG-2925: database is locked".to_string()]
        );
    }

    #[test]
    fn a_panel_that_did_not_apply_before_is_not_blamed() {
        let saved = Fingerprint {
            node: None,
            panels: vec![panel("PlanPanel (x)", false)],
        };
        assert!(saved.conflicts(&saved.clone()).is_empty());
    }

    #[test]
    fn no_selected_node_means_only_panels_are_compared() {
        let saved = Fingerprint {
            node: None,
            panels: vec![panel("OnOthersPanel", true)],
        };
        assert!(saved.conflicts(&saved.clone()).is_empty());
    }
}
