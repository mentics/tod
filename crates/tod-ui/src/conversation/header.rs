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
use gpui_component::Disableable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use tod_core::dynamic::FocusSelection;
use tod_store::conversation::{Focus, NetOp, ProtocolKind};

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

/// Most characters a path crumb's button shows; the full title is its
/// tooltip.
const CRUMB_CHARS: usize = 24;

/// A node title as a crumb label: one line, cut to [`CRUMB_CHARS`].
pub(crate) fn crumb_label(title: &str) -> String {
    let line = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > CRUMB_CHARS {
        let mut cut: String = line.chars().take(CRUMB_CHARS).collect();
        cut.truncate(cut.trim_end().len());
        cut.push('…');
        cut
    } else {
        line
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
        // Unsaved: highlight the "New …" entry for the kind being started.
        let new_ix = self.data.conversations.len()
            + self
                .data
                .new_kinds
                .iter()
                .position(|k| *k == self.data.protocol)
                .unwrap_or(0);
        self.picker = Some(current.unwrap_or(new_ix));
        cx.notify();
    }

    /// Open picker entry `ix`. Past the saved conversations come the "New …"
    /// entries, one per kind this focus can start.
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
            None => {
                let kind = ix
                    .checked_sub(self.data.conversations.len())
                    .and_then(|i| self.data.new_kinds.get(i).copied());
                if let Some(kind) = kind {
                    self.protocol = kind;
                }
                self.new_conversation(window, cx)
            }
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
        let crumbs = self.data.path.clone();
        let updated = self
            .conversation_id
            .and_then(|id| {
                self.data
                    .conversations
                    .iter()
                    .find(|c| c.conversation.id == id)
            })
            .map(|c| format_time(c.conversation.updated_at));
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
            .child(
                Button::new("conversation-forward")
                    .icon(Icon::new(IconName::ArrowRight))
                    .ghost()
                    .small()
                    .disabled(!self.history.can_go_forward())
                    .selected(stop == Some(Stop::Forward))
                    .tooltip("Forward (Alt+Right)")
                    .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
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
                Button::new("conversation-focus-project")
                    .icon(Icon::new(IconName::Layers))
                    .ghost()
                    .small()
                    .selected(self.focus == Focus::Project)
                    .tooltip("Everything, across every list")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open(Focus::Project, true, window, cx)
                    })),
            )
            .children(crumbs.into_iter().enumerate().map(|(ix, crumb)| {
                let node = crumb.node;
                let title = SharedString::from(crumb.title.clone());
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .child(style::text_muted(div()).child("›"))
                    .child(
                        Button::new(ElementId::Name(format!("conversation-crumb-{ix}").into()))
                            .label(crumb_label(&crumb.title))
                            .ghost()
                            .small()
                            .tooltip(title)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open(Focus::Node(node), true, window, cx)
                            })),
                    )
            }))
            // The project button is itself the project crumb, so the kind
            // icon would only repeat it.
            .when(self.focus != Focus::Project, |el| {
                el.child(style::text_muted(div()).flex_shrink_0().child("›"))
                    .child(
                        style::text_muted(div())
                            .id("conversation-kind")
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .child(Icon::new(kind_icon).small())
                            .tooltip(move |window, cx| Tooltip::new(kind).build(window, cx)),
                    )
            })
            // The drill-down belongs beside the title, so the pair share the
            // row's spare width rather than the title taking all of it.
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .child(
                        style::text_title(selectable_text(
                            "conversation-title",
                            self.data.title.clone(),
                            window,
                            cx,
                        ))
                        .flex_shrink(1.)
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden(),
                    )
                    .child(self.render_drill_down(window, cx)),
            )
            .child(
                Button::new("conversation-context-toggle")
                    .icon(Icon::new(IconName::PanelRight))
                    .ghost()
                    .small()
                    .selected(self.context.open)
                    .tooltip("Context panel (Ctrl+I)")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_context(window, cx))),
            )
            .into_any_element()
    }

    /// The chevron beside the title: the focused item's children, as a tree
    /// to drill into. Absent when it has none.
    fn render_drill_down(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.nav.is_none() && !self.data.has_children {
            return div().into_any_element();
        }
        let menu = self.nav.is_some().then(|| self.render_nav_menu(window, cx));
        div()
            .id("conversation-drill-down")
            .relative()
            .flex_shrink_0()
            .child(
                Button::new("conversation-drill-down-button")
                    .icon(Icon::new(IconName::ChevronDown))
                    .ghost()
                    .small()
                    .selected(self.nav.is_some())
                    .tooltip("Go to something under this item")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.close_nav_menu(cx) {
                            this.open_nav_menu(cx);
                        }
                    })),
            )
            .when_some(menu, |el, menu| {
                el.child(
                    deferred(
                        anchored()
                            .anchor(Anchor::TopRight)
                            .snap_to_window_with_margin(px(8.))
                            .child(div().occlude().mt_1().child(menu)),
                    )
                    .with_priority(1),
                )
            })
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
                    .when(
                        summary.conversation.protocol != ProtocolKind::Outline,
                        |el| {
                            el.child(
                                style::badge(div())
                                    .flex_shrink_0()
                                    .child(kind_label(summary.conversation.protocol)),
                            )
                        },
                    )
                    // Which transition a gate check checks, or which state an
                    // on-entry run set up.
                    .children(
                        summary
                            .conversation
                            .transition_label()
                            .map(|label| style::badge(div()).flex_shrink_0().child(label)),
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
        let first_new = self.data.conversations.len();
        for (i, kind) in self.data.new_kinds.iter().copied().enumerate() {
            let ix = first_new + i;
            menu = menu.child(
                style::menu_item(h_flex(), highlighted == ix)
                    .id(ElementId::Name(
                        format!("picker-entry-new-{}", kind.as_str()).into(),
                    ))
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
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .child(Icon::new(IconName::Plus).xsmall()),
                    )
                    .child(div().flex_1().child(new_label(kind)))
                    // Ctrl+N starts another of the kind that is open.
                    .when(kind == self.data.protocol, |el| {
                        el.child(style::badge(div()).child("Ctrl+N"))
                    }),
            );
        }
        menu.into_any_element()
    }
}

/// A kind of conversation, as the picker badges it.
pub(super) fn kind_label(kind: ProtocolKind) -> &'static str {
    match kind {
        ProtocolKind::Outline => "outline",
        ProtocolKind::Implementation => "implementation",
        ProtocolKind::Verification => "verification",
        ProtocolKind::Review => "review",
        ProtocolKind::Fix => "fix",
        ProtocolKind::Pr => "pr",
        ProtocolKind::Chat => "chat",
        ProtocolKind::VisualDesign => "visual design",
        ProtocolKind::GateCheck => "gate check",
        ProtocolKind::OnEntry => "on entry",
        ProtocolKind::Incoming => "incoming check",
    }
}

/// The picker entry that starts a conversation of `kind`.
pub(super) fn new_label(kind: ProtocolKind) -> &'static str {
    match kind {
        ProtocolKind::Outline => "New outline",
        ProtocolKind::Implementation => "New implementation",
        ProtocolKind::Verification => "New verification",
        ProtocolKind::Review => "New review",
        ProtocolKind::Fix => "New fix",
        // Started by the lifecycle buttons, which know the transition.
        ProtocolKind::Pr => "New pr",
        ProtocolKind::Chat => "New conversation",
        ProtocolKind::VisualDesign => "New visual design",
        // Started by the lifecycle buttons, which know the transition.
        ProtocolKind::GateCheck => "New gate check",
        ProtocolKind::OnEntry => "New on-entry run",
        // Started from the lifecycle panel or the tree, never the picker.
        ProtocolKind::Incoming => "New incoming-changes check",
    }
}
