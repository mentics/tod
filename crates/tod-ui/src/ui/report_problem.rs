//! App-wide "report a problem" shortcut.
//!
//! Mirrors `crate::ui::agent_chat`: every surface that can say what the user
//! is looking at handles [`ReportProblem`], so the same keystroke opens the
//! report dialog everywhere, scoped to whatever the focused view is showing.
//! Views with nothing to offer should `cx.propagate()`; the shell root is the
//! fallback (the task list's selection, else the whole project).
//!
//! A view that knows its selection dispatches [`OpenReportDialog`] with the
//! journey key (and, for the conversation view, which conversation it came
//! from), which the shell handles by opening the dialog.

use gpui::{App, KeyBinding, actions};
use tod_journey::JourneyKey;
use uuid::Uuid;

use crate::ui::key_context;

actions!(report_problem, [ReportProblem]);

/// Ctrl+Enter in the report-a-problem dialog's note field.
pub const REPORT_DIALOG_CONTEXT: &str = "ReportDialog";
actions!(report_problem, [ReportDialogSubmit]);

/// Open the report-a-problem dialog against `key`. Dispatched by views,
/// handled by the shell. Never bound to a key directly — `ReportProblem` is.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = report_problem, no_json)]
pub struct OpenReportDialog {
    pub key: JourneyKey,
    /// Set when the report was opened from a specific conversation, so the
    /// dialog can offer that conversation's context too.
    pub conversation: Option<Uuid>,
}

impl OpenReportDialog {
    pub fn project() -> Self {
        Self {
            key: JourneyKey::Project,
            conversation: None,
        }
    }

    pub fn node(id: Uuid) -> Self {
        Self {
            key: JourneyKey::Node(id),
            conversation: None,
        }
    }
}

pub fn register_report_problem_keyboard_bindings(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-shift-r", ReportProblem, None)]);
    let input = Some(key_context::including_input(REPORT_DIALOG_CONTEXT));
    cx.bind_keys([KeyBinding::new("ctrl-enter", ReportDialogSubmit, input)]);
}
