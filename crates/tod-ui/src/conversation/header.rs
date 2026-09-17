//! The one-line header: back, the conversation picker, then what the
//! conversation is about.

use super::{ConversationView, Pane, Stop};
use crate::ui::app_nav::HasAppNav;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::views::rows::op_name;
use chrono::{Local, TimeZone};
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, Context, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, anchored, deferred,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use tod_core::dynamic::FocusSelection;
use tod_store::conversation::{Focus, NetOp};

/// The focus kind's name and icon.
pub(crate) fn kind_of(focus: Focus) -> (&'static str, IconName) {
    match focus {
        Focus::Project => ("Project", IconName::Layers),
        Focus::Node(_) => ("Node", IconName::Folder),
        Focus::Obligation { .. } => ("Obligation", IconName::FileText),
        Focus::PlanStep { .. } => ("Plan step", IconName::ListChecks),
    }
}

/// Most characters of an item's text the header title keeps; the element
/// ellipsizes whatever still does not fit.
const TITLE_CHARS: usize = 200;

/// What the header calls the focus. An obligation or plan step goes by its
/// text on one line (the kind icon says what it is); `selection.title`, which
/// carries ids for the agent, is only the fallback for a deleted item.
pub(crate) fn display_title(selection: &FocusSelection) -> String {
    let text = match selection.focus {
        Focus::Obligation { .. } | Focus::PlanStep { .. } => selection.text.as_deref(),
        Focus::Project | Focus::Node(_) => None,
    };
    let line = text
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty());
    match line {
        Some(line) if line.chars().count() > TITLE_CHARS => {
            let mut cut: String = line.chars().take(TITLE_CHARS).collect();
            cut.truncate(cut.trim_end().len());
            cut.push('…');
            cut
        }
        Some(line) => line,
        None => selection.title.clone(),
    }
}

pub(crate) fn op_summary(op: NetOp) -> &'static str {
    op_name(op)
}

/// A timestamp as the picker shows it: the time today, else the date.
pub(crate) fn format_time(ms: i64) -> String {
    let Some(at) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    if at.date_naive() == Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d").to_string()
    }
}

/// "3 changes" / "1 change" / "no changes".
pub(crate) fn change_count_label(n: usize) -> String {
    match n {
        0 => "no changes".into(),
        1 => "1 change".into(),
        n => format!("{n} changes"),
    }
}

impl ConversationView {
    /// "N of M", counting from the oldest; "New" for an unsaved conversation.
    pub(crate) fn picker_label(&self) -> String {
        let total = self.data.conversations.len();
        match self.conversation_id.and_then(|id| {
            self.data
                .conversations
                .iter()
                .position(|c| c.conversation.id == id)
        }) {
            Some(ix) => format!("{} of {total}", total - ix),
            None if total == 0 => "New".into(),
            None => format!("New, {total} earlier"),
        }
    }

    pub(super) fn open_picker(&mut self, cx: &mut Context<Self>) {
        let current = self.conversation_id.and_then(|id| {
            self.data
                .conversations
                .iter()
                .position(|c| c.conversation.id == id)
        });
        self.picker = Some(current.unwrap_or(self.data.conversations.len()));
        cx.notify();
    }

    /// Open picker entry `ix`; the last one is "New conversation".
    pub(super) fn choose_picker_entry(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker = None;
        match self.data.conversations.get(ix).map(|c| c.conversation.id) {
            Some(id) => {
                self.show(self.focus, Some(id), false, cx);
                self.focus_handle.focus(window, cx);
            }
            None => self.new_conversation(window, cx),
        }
    }

    pub(super) fn render_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stop = (self.pane == Pane::Transcript && self.picker.is_none() && !self.text_editing())
            .then_some(self.stop);
        let (kind, kind_icon) = kind_of(self.focus);
        let updated = self
            .conversation_id
            .and_then(|id| {
                self.data
                    .conversations
                    .iter()
                    .find(|c| c.conversation.id == id)
            })
            .map(|c| format_time(c.conversation.updated_at));
        let path = self.data.path.clone();
        let path = match self.focus {
            // A node's path ends with its own title, which is the title.
            Focus::Node(_) => path[..path.len().saturating_sub(1)].to_vec(),
            _ => path,
        };
        let status: Option<SharedString> = if self.status.running {
            Some(
                self.status
                    .activity
                    .clone()
                    .unwrap_or_else(|| "Agent working…".into())
                    .into(),
            )
        } else if let Some(error) = &self.status.last_error {
            Some(error.clone().into())
        } else {
            (!self.status_line.is_empty()).then(|| self.status_line.clone())
        };
        let status_is_error = !self.status.running && self.status.last_error.is_some();

        let picker = self.render_picker_button(stop == Some(Stop::Picker), updated, window, cx);
        let app_nav = self.render_app_nav(window, cx).into_any_element();

        style::panel_header(h_flex())
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .child(app_nav)
            .child(
                Button::new("conversation-back")
                    .icon(Icon::new(IconName::ArrowLeft))
                    .ghost()
                    .small()
                    .selected(stop == Some(Stop::Back))
                    .tooltip("Back (Alt+Left)")
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(picker)
            .child(
                div()
                    .flex_shrink_0()
                    .w(style::size::BORDER)
                    .h(px(16.))
                    .bg(style::color::divider()),
            )
            .child(
                style::text_muted(div())
                    .id("conversation-kind")
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(Icon::new(kind_icon).small())
                    .tooltip(move |window, cx| Tooltip::new(kind).build(window, cx)),
            )
            .when(!path.is_empty(), |el| {
                el.child(
                    style::text_muted(selectable_text(
                        "conversation-path",
                        format!("{} ›", path.join(" › ")),
                        window,
                        cx,
                    ))
                    .flex_shrink(1.)
                    .min_w_0()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden(),
                )
            })
            .child(
                style::text_title(selectable_text(
                    "conversation-title",
                    self.data.title.clone(),
                    window,
                    cx,
                ))
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .text_ellipsis()
                .overflow_hidden(),
            )
            .when_some(status, |el, status| {
                let text = selectable_text("conversation-status", status, window, cx)
                    .flex_shrink_0()
                    .max_w(px(320.))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden();
                el.child(if status_is_error {
                    style::text_error(text)
                } else {
                    style::text_dense_muted(text)
                })
            })
            .child(
                Button::new("conversation-context-toggle")
                    .icon(Icon::new(IconName::PanelRight))
                    .ghost()
                    .small()
                    .selected(self.context.open)
                    .tooltip("Context panel (Ctrl+.)")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_context(window, cx))),
            )
            .into_any_element()
    }

    fn render_picker_button(
        &mut self,
        highlighted: bool,
        updated: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = match updated {
            Some(time) => format!("{} · {time}", self.picker_label()),
            None => self.picker_label(),
        };
        let open = self.picker.is_some();
        let menu = self
            .picker
            .map(|ix| self.render_picker_menu(ix, window, cx));
        div()
            .id("conversation-picker")
            .relative()
            .flex_shrink_0()
            .child(
                Button::new("conversation-picker-button")
                    .icon(Icon::new(IconName::MessagesSquare))
                    .label(label)
                    .ghost()
                    .small()
                    .selected(highlighted || open)
                    .tooltip("Conversations about this item")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.picker.take().is_none() {
                            this.pane = Pane::Transcript;
                            this.stop = Stop::Picker;
                            this.open_picker(cx);
                        }
                        cx.notify();
                    })),
            )
            .when_some(menu, |el, menu| {
                el.child(
                    deferred(
                        anchored()
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(div().occlude().mt_1().child(menu)),
                    )
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    fn render_picker_menu(
        &self,
        highlighted: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.conversation_id;
        let mut menu = style::floating_panel(v_flex())
            .id("conversation-picker-menu")
            .min_w(px(320.))
            .max_w(px(480.))
            .gap(style::space::HAIRLINE)
            .px(style::space::INLINE)
            .py(style::space::INLINE)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.picker = None;
                cx.notify();
            }));
        for (ix, summary) in self.data.conversations.iter().enumerate() {
            let is_current = current == Some(summary.conversation.id);
            let opening = if summary.opening.is_empty() {
                "(no messages)".to_string()
            } else {
                summary.opening.clone()
            };
            menu = menu.child(
                style::menu_item(h_flex(), ix == highlighted)
                    .id(ElementId::Name(format!("picker-entry-{ix}").into()))
                    .w_full()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.choose_picker_entry(ix, window, cx);
                        }),
                    )
                    .child(div().w(px(16.)).flex_shrink_0().when(is_current, |el| {
                        el.child(Icon::new(IconName::Check).xsmall())
                    }))
                    .child(
                        style::text_dense_muted(div())
                            .flex_shrink_0()
                            .child(format_time(summary.conversation.updated_at)),
                    )
                    .child(
                        style::text_dense_muted(div())
                            .flex_shrink_0()
                            .child(change_count_label(summary.change_count)),
                    )
                    .child(
                        selectable_text(
                            ElementId::Name(format!("picker-opening-{ix}").into()),
                            opening,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis(),
                    ),
            );
        }
        let new_ix = self.data.conversations.len();
        menu.child(
            style::menu_item(h_flex(), highlighted == new_ix)
                .id("picker-entry-new")
                .w_full()
                .items_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.choose_picker_entry(new_ix, window, cx);
                    }),
                )
                .child(
                    div()
                        .w(px(16.))
                        .flex_shrink_0()
                        .child(Icon::new(IconName::Plus).xsmall()),
                )
                .child(div().flex_1().child("New conversation"))
                .child(style::badge(div()).child("Ctrl+N")),
        )
        .into_any_element()
    }
}
