//! The journey column panel (`PanelKind::Journey` for a node,
//! `PanelKind::ProjectJourney` for the user's own recent activity): the
//! recorded timeline, read from this computer's journey files, with what a
//! person needs to hand it to an agent — its reference, and the text that
//! says how to read it (`tod_core::journey::reference`). Local only: nothing
//! here depends on whether journeys are sent anywhere.
//!
//! The files are read on the background executor and re-read on Refresh, not
//! on a timer.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    div,
};
use gpui_component::button::Button;
use tod_core::journey::reference::{PROJECT_REFERENCE, agent_snippet, node_reference};
use tod_journey::timeline;
use tod_journey::{Event, JourneyKey, JourneyReader, Record};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};

/// Records shown until "Show all": a journey can be long.
const RECENT: usize = 300;

/// What a journey read found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Loaded {
    /// `journey:node/<slug>` or `journey:project`.
    pub reference: String,
    /// Timeline lines for the records shown, oldest first.
    pub lines: Vec<String>,
    /// Records in the whole journey (resolved-reference records excluded).
    pub total: usize,
}

/// Reads the journey `key` names, showing the last `limit` records
/// (`None`: all). `slug` is the node's, for its reference.
pub(crate) fn load(
    dir: &std::path::Path,
    key: JourneyKey,
    slug: Option<&str>,
    limit: Option<usize>,
) -> std::io::Result<Loaded> {
    let reader = JourneyReader::open(dir, key)?;
    let records: Vec<&Record> =
        reader.all().iter().filter(|r| !matches!(r.event, Event::Resolved { .. })).collect();
    let total = records.len();
    let skip = limit.map_or(0, |n| total.saturating_sub(n));
    let mut prev_at = None;
    let lines = records[skip..]
        .iter()
        .map(|record| {
            let line = timeline::line(record, prev_at);
            prev_at = Some(record.at);
            line
        })
        .collect();
    let reference = match (key, slug) {
        (JourneyKey::Project, _) => PROJECT_REFERENCE.to_string(),
        (JourneyKey::Node(_), Some(slug)) => node_reference(slug),
        (JourneyKey::Node(id), None) => node_reference(&id.to_string()),
    };
    Ok(Loaded { reference, lines, total })
}

enum State {
    Loading,
    Failed(String),
    Ready(Loaded),
}

pub struct JourneyPanel {
    fleet: Arc<FleetStore>,
    node: Option<Uuid>,
    dir: PathBuf,
    focus_handle: FocusHandle,
    state: State,
    show_all: bool,
    /// What the last copy button put on the clipboard, for a moment's feedback.
    copied: Option<&'static str>,
}

impl JourneyPanel {
    /// `node`: a node's journey; `None`: the user's recent activity.
    pub fn new(node: Option<Uuid>, fleet: Arc<FleetStore>, cx: &mut Context<Self>) -> Self {
        let dir = fleet.paths().root().join("journeys");
        let mut panel = Self {
            fleet,
            node,
            dir,
            focus_handle: cx.focus_handle(),
            state: State::Loading,
            show_all: false,
            copied: None,
        };
        panel.reload(cx);
        panel
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (fleet, dir, node) = (self.fleet.clone(), self.dir.clone(), self.node);
        let limit = (!self.show_all).then_some(RECENT);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (key, slug) = match node {
                        Some(id) => (
                            JourneyKey::Node(id),
                            fleet.get_node(&id.to_string()).ok().flatten().map(|n| n.slug),
                        ),
                        None => (JourneyKey::Project, None),
                    };
                    load(&dir, key, slug.as_deref(), limit)
                })
                .await;
            let _ = this.update(cx, |this: &mut JourneyPanel, cx| {
                this.state = match result {
                    Ok(loaded) => State::Ready(loaded),
                    Err(err) => State::Failed(format!("could not read the journey: {err}")),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn copy(&mut self, what: &'static str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(what);
        cx.notify();
    }
}

impl ColumnPanel for JourneyPanel {
    fn title(&self, _cx: &App) -> SharedString {
        if self.node.is_some() { "Journey" } else { "Recent activity" }.into()
    }
}

impl EventEmitter<PanelOpenRequest> for JourneyPanel {}

impl Focusable for JourneyPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for JourneyPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div()
            .id("unified-journey-scroll")
            .size_full()
            .overflow_y_scroll()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_2();

        match &self.state {
            State::Loading => {
                body = body.child(style::empty_message(div()).child("Reading the journey…"));
            }
            State::Failed(why) => {
                body = body.child(style::text_error(div()).child(selectable_text(
                    "journey-error",
                    why.clone(),
                    window,
                    cx,
                )));
            }
            State::Ready(loaded) => {
                let reference = loaded.reference.clone();
                let snippet = agent_snippet(&loaded.reference);
                let timeline_text = loaded.lines.join("\n");
                body = body
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(style::text_title(div()).child(selectable_text(
                                "journey-reference",
                                reference.clone(),
                                window,
                                cx,
                            )))
                            .child(style::text_dense_muted(div()).child(format!(
                                "showing {} of {} records",
                                loaded.lines.len(),
                                loaded.total
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(Button::new("journey-copy-reference").label("Copy reference").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.copy("reference", reference.clone(), cx)
                                }),
                            ))
                            .child(Button::new("journey-copy-agent").label("Copy for agent").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.copy("agent text", snippet.clone(), cx)
                                }),
                            ))
                            .child(Button::new("journey-copy-timeline").label("Copy timeline").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.copy("timeline", timeline_text.clone(), cx)
                                }),
                            ))
                            .child(Button::new("journey-refresh").label("Refresh").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.copied = None;
                                    this.reload(cx);
                                }),
                            ))
                            .children((loaded.lines.len() < loaded.total).then(|| {
                                Button::new("journey-show-all").label("Show all").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.show_all = true;
                                        this.reload(cx);
                                    }),
                                )
                            }))
                            .children(self.node.map(|_| {
                                div()
                                    .id("journey-open-recent")
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|_, event: &gpui::MouseDownEvent, _, cx| {
                                            cx.emit(PanelOpenRequest {
                                                target: PanelKind::ProjectJourney,
                                                ctrl: event.modifiers.control || event.modifiers.platform,
                                            });
                                        }),
                                    )
                                    .child(style::text_link(div()).child("Your recent activity"))
                            })),
                    )
                    .children(self.copied.map(|what| {
                        style::text_dense_muted(div()).child(format!("Copied the {what}."))
                    }));
                if loaded.lines.is_empty() {
                    body = body.child(style::empty_message(div()).child(
                        "Nothing recorded yet. Journeys are recorded while \"Record journeys on this computer\" is on in Settings.",
                    ));
                }
                for (i, line) in loaded.lines.iter().enumerate() {
                    body = body.child(style::text_dense(div().font_family("monospace")).child(selectable_text(
                        SharedString::from(format!("journey-line-{i}")),
                        line.clone(),
                        window,
                        cx,
                    )));
                }
            }
        }
        div().id("unified-journey").size_full().track_focus(&self.focus_handle).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::{Actor, JourneyWriter};

    #[test]
    fn load_limits_to_the_last_records_and_names_the_reference() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut writer = JourneyWriter::open(dir.path(), JourneyKey::Node(id)).unwrap();
        for state in ["a", "b", "c"] {
            writer.append(Actor::User, Event::Milestone { state: state.into() });
        }
        drop(writer);

        let all = load(dir.path(), JourneyKey::Node(id), Some("fix-login"), None).unwrap();
        assert_eq!((all.total, all.lines.len()), (3, 3));
        assert_eq!(all.reference, "journey:node/fix-login");
        let last = load(dir.path(), JourneyKey::Node(id), Some("fix-login"), Some(1)).unwrap();
        assert_eq!((last.total, last.lines.len()), (3, 1));
        assert!(last.lines[0].contains("milestone: c"));
        let project = load(dir.path(), JourneyKey::Project, None, None).unwrap();
        assert_eq!((project.reference.as_str(), project.total), ("journey:project", 0));
    }
}
