//! The side pane of a visual-design conversation: the working draft's state,
//! Accept, and the design window (open or re-dock, close). Design:
//! `doc/ui/visual-design-browser.md` section 8.
//!
//! Nothing here blocks the UI thread. The launcher's methods spawn processes
//! and call the OS, `accept_draft` writes files and the store, and comparing
//! the draft with the saved mockup reads files: each runs on the background
//! executor and reports back. The UI thread reads only plain numbers (tod's
//! window bounds and scale factor) and hands them over.
//!
//! The pane follows the store, not a timer: the conversation view's poll
//! already reloads when the store commits or a turn finishes, and that is when
//! the draft is compared again and the window is pointed at the conversation's
//! draft. The browser reloads itself when the file changes (the server
//! watches it).

use super::{AfterSend, ConversationView};
use crate::ui::agent_conversation::NoticeTone;
use crate::ui::journey::{Source, record_action};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::visual_design::launcher::{Dock, Launcher, LauncherEvent};
use crate::visual_design::placement::Rect;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels,
    Styled, Window, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, Sizable, h_flex, v_flex};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use tod_core::conversation::context::visual_design_draft_path;
use tod_core::visual_design::{accept_draft, draft_differs};
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::Focus;

pub(super) const ACCEPT: &str = "visual-design.accept";
pub(super) const OPEN: &str = "visual-design.open";
pub(super) const CLOSE: &str = "visual-design.close";

/// How often the launcher is asked whether the window was closed.
const TICK: Duration = Duration::from_secs(1);

/// What the pane shows about the focused obligation, from the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DesignSnapshot {
    /// The obligation's own words, first line.
    pub obligation: String,
    /// The linked mockup, when one was accepted.
    pub saved: Option<String>,
}

/// The pane's own state: the shared launcher and what the last action or
/// event said.
#[derive(Default)]
pub(super) struct Designer {
    launcher: Option<Arc<Launcher>>,
    events: Option<Receiver<LauncherEvent>>,
    /// Feedback posted by the page, waiting for the agent to be free.
    feedback_rx: Option<Receiver<crate::visual_design::server::Feedback>>,
    pub(super) feedback_queue: std::collections::VecDeque<crate::visual_design::server::Feedback>,
    window_open: bool,
    /// An action is running off the UI thread.
    busy: Option<&'static str>,
    notice: Option<(NoticeTone, String)>,
    /// Whether the draft file exists, and differs from the saved mockup.
    draft_exists: bool,
    draft_differs: bool,
    /// The draft the window was last pointed at.
    pointed_at: Option<PathBuf>,
    /// The shell is showing the conversation view.
    view_shown: bool,
}

impl Designer {
    fn launcher(&mut self, data_root: &std::path::Path) -> Arc<Launcher> {
        if let Some(l) = &self.launcher {
            return l.clone();
        }
        // `visual_design.browser` in tod.yml overrides browser discovery.
        let browser = crate::interview::TodSettings::load(&crate::interview::TodPaths::at(data_root))
            .ok()
            .and_then(|s| s.visual_design.browser)
            .filter(|b| !b.trim().is_empty())
            .map(PathBuf::from);
        let (launcher, events) = Launcher::system(data_root, browser);
        let launcher = Arc::new(launcher);
        crate::visual_design::launcher::register(&launcher);
        // Detects a window the user closed; ends with the launcher.
        let weak = Arc::downgrade(&launcher);
        std::thread::spawn(move || {
            while let Some(l) = weak.upgrade() {
                l.tick(Instant::now());
                drop(l);
                std::thread::sleep(TICK);
            }
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let weak_launcher = Arc::downgrade(&launcher);
        launcher.set_feedback_handler(Arc::new(move |_token, mut fb| {
            // Runs on a server thread, so waiting on the browser is fine.
            if let Some(clip) = crate::visual_design::cdp::clip_for(&fb) {
                match weak_launcher.upgrade().map(|l| l.capture(clip)) {
                    Some(Ok(png)) => fb.screenshot = Some(png),
                    Some(Err(e)) => fb.note = Some(format!("Screenshot unavailable: {e}")),
                    None => {}
                }
            }
            let _ = tx.lock().unwrap().send(fb);
        }));
        self.feedback_rx = Some(rx);
        self.events = Some(events);
        self.launcher = Some(launcher.clone());
        launcher
    }
}

fn plain(p: Pixels) -> f64 {
    f32::from(p) as f64
}

/// tod's window and the displays as plain numbers; read on the UI thread.
fn read_dock(window: &Window, cx: &App) -> Dock {
    let b = window.bounds();
    let tod = Rect::new(
        plain(b.origin.x),
        plain(b.origin.y),
        plain(b.size.width),
        plain(b.size.height),
    );
    let work_areas = cx
        .displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            Rect::new(
                plain(b.origin.x),
                plain(b.origin.y),
                plain(b.size.width),
                plain(b.size.height),
            )
        })
        .collect();
    Dock {
        tod,
        work_areas,
        scale_factor: window.scale_factor() as f64,
    }
}

fn describe(event: &LauncherEvent) -> Option<(NoticeTone, String)> {
    match event {
        LauncherEvent::Opened | LauncherEvent::Redocked | LauncherEvent::Closed => None,
        LauncherEvent::ChromeMissing(m) => Some((NoticeTone::Error, m.clone())),
        LauncherEvent::MoverUnsupported(m) => Some((
            NoticeTone::Muted,
            format!("The window was opened but not docked: {m}"),
        )),
        LauncherEvent::PermissionDenied(m) | LauncherEvent::Failed(m) => {
            Some((NoticeTone::Error, m.clone()))
        }
    }
}

impl ConversationView {
    fn draft_path(&self) -> Option<PathBuf> {
        let id = self.conversation_id?;
        Some(visual_design_draft_path(self.fleet.paths().root(), id))
    }

    fn is_designer(&self) -> bool {
        self.data.protocol == tod_store::conversation::ProtocolKind::VisualDesign
    }

    /// Take what the launcher has reported; true when the pane changed.
    pub(super) fn designer_drain(&mut self) -> bool {
        let Some(rx) = self.designer.events.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Ok(event) = rx.try_recv() {
            changed = true;
            match &event {
                LauncherEvent::Opened | LauncherEvent::Redocked => {
                    self.designer.window_open = true;
                    self.designer.notice = None;
                }
                LauncherEvent::Closed => self.designer.window_open = false,
                _ => {}
            }
            if let Some(notice) = describe(&event) {
                self.designer.notice = Some(notice);
            }
        }
        changed
    }

    /// Send what the page posted as a user turn of this conversation (the
    /// one the window is pointed at), off the UI thread via `deliver`. A turn
    /// the agent is too busy for waits for the next poll. `images` is the
    /// hook for F3's screenshot (the pasted-image path).
    pub(super) fn designer_deliver_feedback(&mut self, cx: &mut Context<Self>) {
        if let Some(rx) = self.designer.feedback_rx.as_ref() {
            while let Ok(fb) = rx.try_recv() {
                self.designer.feedback_queue.push_back(fb);
            }
        }
        if !self.is_designer() {
            return;
        }
        while let Some(fb) = self.designer.feedback_queue.front() {
            let text = crate::visual_design::feedback::render(fb);
            let images: Vec<tod_agent::PromptImage> = fb
                .screenshot
                .iter()
                .map(|png| tod_agent::PromptImage {
                    mime_type: "image/png".into(),
                    data: png.clone(),
                })
                .collect();
            if !self.deliver(&text, images, AfterSend::Retry, cx) {
                break;
            }
            self.designer.feedback_queue.pop_front();
        }
    }

    /// The shell shows or leaves the conversation view: the design window is
    /// hidden while the view is away and restored when it returns.
    pub(crate) fn designer_set_view_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.designer.view_shown = shown;
        self.designer_sync_visibility(cx);
    }

    /// The window is shown only while the conversation view is on screen and
    /// holds a visual-design conversation. Hide and show are quick, so the
    /// browser is never closed for this; they run off the UI thread.
    fn designer_sync_visibility(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = self.designer.launcher.clone() else {
            return;
        };
        if !self.designer.window_open {
            return;
        }
        let want_shown = self.designer.view_shown && self.is_designer();
        if want_shown != launcher.is_hidden() {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                if want_shown {
                    launcher.show();
                } else {
                    launcher.hide();
                }
            })
            .detach();
    }

    /// Called when the store committed or a turn finished (and when the view
    /// switches conversation): compare the draft with the saved mockup, and
    /// point an open window at this conversation's draft. Off the UI thread.
    pub(super) fn designer_refresh(&mut self, cx: &mut Context<Self>) {
        self.designer_sync_visibility(cx);
        if !self.is_designer() {
            return;
        }
        let Some(draft) = self.draft_path() else {
            return;
        };
        let saved = self.data.design.as_ref().and_then(|d| d.saved.clone());
        let navigate = (self.designer.window_open && self.designer.pointed_at.as_ref() != Some(&draft))
            .then(|| self.designer.launcher.clone())
            .flatten();
        if navigate.is_some() {
            self.designer.pointed_at = Some(draft.clone());
        }
        cx.spawn(async move |this, cx| {
            let (exists, differs) = cx
                .background_executor()
                .spawn(async move {
                    let exists = draft.is_file();
                    if exists {
                        if let Some(launcher) = navigate {
                            launcher.navigate(&draft);
                        }
                    }
                    (exists, draft_differs(&draft, saved.as_deref()))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if (this.designer.draft_exists, this.designer.draft_differs) != (exists, differs) {
                    this.designer.draft_exists = exists;
                    this.designer.draft_differs = differs;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The buttons on offer right now: id, label, primary, disabled.
    pub(super) fn designer_buttons(&self) -> [(&'static str, &'static str, bool, bool); 3] {
        let d = &self.designer;
        let idle = d.busy.is_none();
        let can_accept = self.conversation_id.is_some()
            && d.draft_exists
            && d.draft_differs
            && matches!(self.focus, Focus::Obligation { .. });
        [
            (ACCEPT, "Accept", can_accept, !(can_accept && idle)),
            (
                OPEN,
                if d.window_open { "Re-dock" } else { "Open" },
                !can_accept && !d.window_open && d.draft_exists,
                !(d.draft_exists && idle),
            ),
            (CLOSE, "Close window", false, !(d.window_open && idle)),
        ]
    }

    fn presented_designer(&self) -> Presented {
        Presented {
            actions: self
                .designer_buttons()
                .iter()
                .map(|(id, label, primary, disabled)| PresentedAction {
                    id: id.to_string(),
                    label: label.to_string(),
                    primary: *primary,
                    disabled: *disabled,
                })
                .collect(),
            focused: None,
            notices: self
                .designer
                .notice
                .iter()
                .map(|(_, text)| text.clone())
                .collect(),
        }
    }

    /// A button in the pane was pressed: record it with what was on offer,
    /// then start the work off the UI thread.
    pub(super) fn designer_action(
        &mut self,
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let disabled = self
            .designer_buttons()
            .iter()
            .any(|(b, _, _, disabled)| *b == id && *disabled);
        if disabled {
            return;
        }
        let presented = self.presented_designer();
        record_action(cx, self.focus, id, Source::Click, "visual-design", presented);
        let data_root = self.fleet.paths().root().to_path_buf();
        self.designer.notice = None;
        self.designer.busy = Some(id);
        match id {
            ACCEPT => {
                let (Some(conversation), Focus::Obligation { id: obligation, .. }) =
                    (self.conversation_id, self.focus)
                else {
                    return;
                };
                let fleet = self.fleet.clone();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            accept_draft(&fleet, &data_root, conversation, obligation)
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.designer.busy = None;
                        this.designer.notice = Some(match result {
                            Ok(path) => (
                                NoticeTone::Muted,
                                format!("Accepted: linked {}", path.display()),
                            ),
                            Err(err) => (NoticeTone::Error, format!("Could not accept: {err:#}")),
                        });
                        this.reload();
                        this.designer_refresh(cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            OPEN => {
                let Some(draft) = self.draft_path() else {
                    return;
                };
                let launcher = self.designer.launcher(&data_root);
                let dock = read_dock(window, cx);
                self.designer.pointed_at = Some(draft.clone());
                cx.spawn(async move |this, cx| {
                    let shown = cx
                        .background_executor()
                        .spawn(async move { launcher.open_or_redock(&draft, &dock) })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.designer.busy = None;
                        this.designer.window_open = shown;
                        cx.notify();
                    });
                })
                .detach();
            }
            CLOSE => {
                let Some(launcher) = self.designer.launcher.clone() else {
                    self.designer.busy = None;
                    return;
                };
                self.designer.pointed_at = None;
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .spawn(async move { launcher.close_window() })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.designer.busy = None;
                        this.designer.window_open = false;
                        cx.notify();
                    });
                })
                .detach();
            }
            _ => self.designer.busy = None,
        }
        cx.notify();
    }

    pub(super) fn render_designer_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let design = self.data.design.clone().unwrap_or_default();
        let d = &self.designer;
        let state = if let Some(id) = d.busy {
            match id {
                ACCEPT => "Accepting…",
                OPEN => "Opening the window…",
                _ => "Closing the window…",
            }
        } else if d.window_open {
            "Window open"
        } else {
            "Window closed"
        };
        let draft = match (self.conversation_id, d.draft_exists, d.draft_differs) {
            (None, ..) => "Send a message to start designing.".to_string(),
            (_, false, _) => "No draft yet: the agent writes mockup.html as it works.".to_string(),
            (_, true, true) if design.saved.is_some() => {
                "The draft differs from the accepted mockup.".to_string()
            }
            (_, true, true) => "The draft is not accepted yet.".to_string(),
            (_, true, false) => "The draft is the accepted mockup.".to_string(),
        };
        let saved = design
            .saved
            .clone()
            .unwrap_or_else(|| "none accepted yet".to_string());
        let notice = d.notice.clone();
        let buttons = self.designer_buttons();
        let row = |buttons: &[(&'static str, &'static str, bool, bool)], cx: &mut Context<Self>| {
            let mut row = h_flex().gap(style::space::INLINE).px(style::space::RELATED);
            for (id, label, primary, disabled) in buttons.iter().copied() {
                let button = Button::new(ElementId::Name(id.into()))
                    .label(label)
                    .small()
                    .disabled(disabled)
                    .when(primary, |b| b.primary())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.designer_action(id, window, cx);
                    }));
                row = row.child(button);
            }
            row
        };
        v_flex()
            .size_full()
            .min_w_0()
            .gap(style::space::RELATED)
            .child(
                style::panel_header(div()).child(style::text_muted(div()).child("Visual design")),
            )
            .child(
                v_flex()
                    .px(style::space::RELATED)
                    .gap(style::space::HAIRLINE)
                    .child(selectable_text(
                        "designer-obligation",
                        design.obligation.clone(),
                        window,
                        cx,
                    ))
                    .child(style::text_dense_muted(div()).child(selectable_text(
                        "designer-mockup",
                        format!("Mockup: mockup.html. Accepted file: {saved}"),
                        window,
                        cx,
                    )))
                    .child(style::text_dense(div()).child(selectable_text(
                        "designer-draft",
                        draft,
                        window,
                        cx,
                    ))),
            )
            .child(row(&buttons, cx))
            .child(
                style::text_dense_muted(div())
                    .px(style::space::RELATED)
                    .child(selectable_text("designer-state", state, window, cx)),
            )
            .children(notice.map(|(tone, text)| {
                let line = selectable_text("designer-notice", text, window, cx);
                let line = div().px(style::space::RELATED).child(line);
                match tone {
                    NoticeTone::Error => style::text_error(line),
                    _ => style::text_dense_muted(line),
                }
            }))
            .into_any_element()
    }
}
