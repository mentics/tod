//! One global "agent needs permission" prompt, wired to
//! [`tod_agent::AgentProvider::respond_to_permission`]. Any view that polls
//! an agent run and sees [`tod_agent::AgentRunState::NeedsPermission`] calls
//! [`queue_permission_request`] — no `Window` required, so it works from
//! background polling loops too. The app shell drains the queue once per
//! frame via [`drain_queued_requests`]. This is the single place that
//! renders the prompt and answers it, so no surface implements its own
//! version.
//!
//! Rendered as a modal dialog rather than a toast: a permission decision
//! must not be lost or brushed aside by an accidental click, so the dialog
//! has no close button, ignores Escape, and does not close on an overlay
//! click — the only way out is picking one of the agent's own options. Each
//! request opens its own stacked dialog (`Root::active_dialogs` is a plain
//! `Vec`, not a de-duplicated slot like the notification list), so two
//! agents needing permission at once both stay queued until answered, one
//! on top of the other, instead of one silently replacing the other.
//!
//! Called from `crate::app::window`'s render loop, mirroring how
//! `queue_error_toast` / `drain_pending_error_toast` surface background
//! errors.

use std::cell::RefCell;
use std::collections::HashSet;

use gpui::{App, ParentElement, SharedString, Styled, Window, div};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};

use tod_agent::{PermissionRequest, RunId, SharedAgent};

thread_local! {
    /// Requests queued from a context with no `Window` access, waiting for
    /// the app shell to render them as dialogs.
    static QUEUE: RefCell<Vec<(SharedAgent, PermissionRequest)>> = RefCell::new(Vec::new());
    /// Runs whose permission dialog is already queued or open, so a
    /// repeated poll of the same still-pending request doesn't open it
    /// twice.
    static OPEN: RefCell<HashSet<RunId>> = RefCell::new(HashSet::new());
}

/// Queue `request` to be opened as a modal dialog on the next frame. Safe to
/// call every poll tick: a request already queued or open for the same run
/// is not queued twice.
pub fn queue_permission_request(agent: SharedAgent, request: PermissionRequest) {
    let run = request.run;
    let is_new = OPEN.with(|open| open.borrow_mut().insert(run));
    if !is_new {
        return;
    }
    QUEUE.with(|queue| queue.borrow_mut().push((agent, request)));
}

/// Open a dialog for every request queued since the last drain. Called once
/// per frame by the app shell.
pub fn drain_queued_requests(window: &mut Window, cx: &mut App) {
    let queued = QUEUE.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
    for (agent, request) in queued {
        open_permission_dialog(window, cx, agent, request);
    }
}

fn open_permission_dialog(
    window: &mut Window,
    cx: &mut App,
    agent: SharedAgent,
    request: PermissionRequest,
) {
    let run = request.run;
    let title: SharedString = request.title.into();
    let options = request.options;

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let agent = agent.clone();
        let options = options.clone();
        let message = title.clone();
        dialog
            .title("Agent needs permission")
            .overlay(true)
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .child(div().text_sm().child(message))
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .children(options.iter().enumerate().map(|(idx, option)| {
                        let agent = agent.clone();
                        let option_id = option.id.clone();
                        let is_allow = option.id.to_ascii_lowercase().contains("allow");
                        let button =
                            Button::new(("permission-option", idx)).label(option.label.clone());
                        let button = if is_allow { button.primary() } else { button };
                        button.on_click(move |_, window, cx| {
                            OPEN.with(|open| {
                                open.borrow_mut().remove(&run);
                            });
                            if let Ok(mut agent) = agent.lock() {
                                let _ = agent.respond_to_permission(run, &option_id);
                            }
                            window.close_dialog(cx);
                        })
                    })),
            )
    });
}
