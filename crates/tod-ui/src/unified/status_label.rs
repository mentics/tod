//! The status label (`doc/ui/unified-view.md` "Status label"): a node's
//! lifecycle state, compactly showing whether a gate check is leaving it or
//! an on-entry agent is entering it.
//!
//! | Label | Meaning |
//! |---|---|
//! | `verifying` | In this state. |
//! | `verifying →` | Checking the gate to leave this state. |
//! | `→ verifying` | Running the on-entry agent for this state. |
//!
//! The text/shape logic (`compute`) is pure and unit-tested on its own;
//! `render` turns it into an element styled per `doc/ui-style-guide.yaml`
//! (the muted-foreground text color already used for secondary text, no raw
//! colors).

use gpui::{App, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::ActiveTheme;

use crate::ui::agent_runs::NodeRun;

/// The label's shape: plain state text, or state text plus a running
/// direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// In this state, nothing running.
    None,
    /// A gate check is running, leaving this state.
    Leaving,
    /// An on-entry agent is running, entering this state.
    Entering,
}

/// The label's computed shape for a node: its lifecycle state plus whichever
/// direction (if any) a run in `runs` is working on it.
///
/// When more than one run matches (which should not happen in practice —
/// a node has at most one gate check and one on-entry turn in flight) the
/// first match wins, checked leaving before entering.
pub fn compute(lifecycle: &str, runs: &[NodeRun]) -> (String, Direction) {
    let direction = if runs.iter().any(|r| r.leaving()) {
        Direction::Leaving
    } else if runs.iter().any(|r| r.entering()) {
        Direction::Entering
    } else {
        Direction::None
    };
    (lifecycle.to_string(), direction)
}

/// Renders `compute`'s result as text: `state`, `state →`, or `→ state`.
pub fn text(lifecycle: &str, runs: &[NodeRun]) -> SharedString {
    let (state, direction) = compute(lifecycle, runs);
    SharedString::from(match direction {
        Direction::None => state,
        Direction::Leaving => format!("{state} \u{2192}"),
        Direction::Entering => format!("\u{2192} {state}"),
    })
}

/// The status label as a styled element (muted secondary text, per the style
/// guide — no raw colors).
pub fn render(lifecycle: &str, runs: &[NodeRun], cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    div().text_xs().text_color(muted).child(text(lifecycle, runs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::conversation::ProtocolKind;
    use uuid::Uuid;

    fn run(protocol: ProtocolKind, running: bool) -> NodeRun {
        NodeRun {
            conversation_id: Some(Uuid::new_v4()),
            protocol,
            running,
            from_state: None,
            to_state: None,
        }
    }

    #[test]
    fn plain_state_with_no_runs() {
        assert_eq!(text("verifying", &[]), SharedString::from("verifying"));
    }

    #[test]
    fn gate_check_running_shows_a_trailing_arrow() {
        let runs = vec![run(ProtocolKind::GateCheck, true)];
        assert_eq!(text("verifying", &runs), SharedString::from("verifying \u{2192}"));
        assert_eq!(compute("verifying", &runs).1, Direction::Leaving);
    }

    #[test]
    fn on_entry_running_shows_a_leading_arrow() {
        let runs = vec![run(ProtocolKind::OnEntry, true)];
        assert_eq!(text("verifying", &runs), SharedString::from("\u{2192} verifying"));
        assert_eq!(compute("verifying", &runs).1, Direction::Entering);
    }

    #[test]
    fn a_finished_gate_check_run_does_not_add_an_arrow() {
        let runs = vec![run(ProtocolKind::GateCheck, false)];
        assert_eq!(text("verifying", &runs), SharedString::from("verifying"));
    }

    #[test]
    fn an_unrelated_protocol_running_does_not_add_an_arrow() {
        let runs = vec![run(ProtocolKind::Fix, true)];
        assert_eq!(text("verifying", &runs), SharedString::from("verifying"));
    }

    #[test]
    fn leaving_wins_over_entering_when_both_present() {
        let runs = vec![run(ProtocolKind::OnEntry, true), run(ProtocolKind::GateCheck, true)];
        assert_eq!(compute("verifying", &runs).1, Direction::Leaving);
    }
}
