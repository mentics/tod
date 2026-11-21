//! The self-improvement view (`doc/requests/design.md` 11.4,
//! `doc/lifecycle/self-improvement.md`): the feedback tod gathered about its
//! own process (lessons), how the process review grouped it, what was changed
//! because of it (an applied lesson names the commit or node), and the
//! measures. Opened from the status bar's Lessons button, like the incidents
//! dialog.
//!
//! What it shows is `tod_core::self_improvement` (a projection of the lessons
//! and of `tod_store::lesson_stats`, the computation `tod-cli lessons stats`
//! also prints). The list (cursor, keys, groups, columns, row menu) is
//! [`crate::ui::item_list`]. The user can decline a lesson or take a declined
//! one back (`InterviewCommand::Lessons`), open the node a lesson links to in
//! its task panel, and look at an applied commit: `git show --stat` text here,
//! or the node's checkout in the code editor. Everything that reads the store,
//! runs git or writes goes off the UI thread, and a failure is shown, never
//! dropped. Every action is recorded as a journey `UserAction`.

use crate::ui::item_list::keyboard::{
    ItemListCollapse, ItemListDown, ItemListEnd, ItemListExpand, ItemListHome, ItemListPageDown,
    ItemListPageUp, ItemListUp,
};
use crate::ui::item_list::{
    CollapseStep, ColumnSpec, GroupSpec, ItemList, ItemListEvent, ItemListKeys, ItemListRow,
    ItemRowState, bind_item_list_keys,
};
use crate::ui::journey::{Source, record_action};
use crate::ui::key_context;
use crate::ui::off_thread::off_thread;
use crate::ui::selectable_text::{selectable_text, selectable_text_colored};
use crate::ui::style::{self, StatusTone};
use crate::views::rows::{RowAction, RowHost, row_action_buttons, row_group};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, FocusHandle, Focusable, InteractiveElement, StatefulInteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, Styled, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable, WindowExt, h_flex, v_flex};
use gpui_kit_assets::IconName;
use std::rc::Rc;
use std::sync::Arc;
use tod_core::self_improvement::{AppliedRef, LessonRow, Overview, Section};
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use tod_store::lessons::{self, STATUS_APPLIED, STATUS_DECLINED};
use uuid::Uuid;

const CONTEXT: &str = "SelfImprovement";
const SURFACE: &str = "self_improvement";

const COLUMN_SOURCE: &str = "source";
const COLUMN_CATEGORY: &str = "category";
const COLUMN_STATUS: &str = "status";
const COLUMN_LESSON: &str = "lesson";

actions!(
    self_improvement,
    [
        SelfImprovementOpenNode,
        SelfImprovementToggleDeclined,
        SelfImprovementShowChanges,
        SelfImprovementRefresh,
        SelfImprovementToggleTab
    ]
);

pub fn register_self_improvement_keyboard_bindings(cx: &mut App) {
    bind_item_list_keys(cx, CONTEXT, ItemListKeys::default());
    let context = Some(key_context::excluding_input(CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", SelfImprovementOpenNode, context),
        KeyBinding::new("d", SelfImprovementToggleDeclined, context),
        KeyBinding::new("c", SelfImprovementShowChanges, context),
        KeyBinding::new("r", SelfImprovementRefresh, context),
        KeyBinding::new("m", SelfImprovementToggleTab, context),
    ]);
}

/// How the shell shows a node's task panel; the dialog only knows the id.
pub type OpenNode = Rc<dyn Fn(Uuid, &mut Window, &mut App)>;

/// What the user did in the list, queued for the view to apply.
#[derive(Debug, Clone)]
enum Action {
    Select { row_ix: usize },
    ToggleGroup { key: String },
    OpenNode { node: Uuid, source: Source },
    Decline { lesson: Uuid, node: Uuid, source: Source },
    Reopen { lesson: Uuid, node: Uuid, source: Source },
    ShowChanges { node: Uuid, commit: String, source: Source },
    OpenEditor { node: Uuid, source: Source },
    Ignored,
}

impl From<ItemListEvent> for Action {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            ItemListEvent::ToggleGroup { key } => Self::ToggleGroup { key },
            ItemListEvent::ToggleMark { .. } | ItemListEvent::Drop(_) => Self::Ignored,
        }
    }
}

/// The primary thing a lesson affords: what Enter does.
fn open_target(row: &LessonRow) -> Option<Uuid> {
    row.node.as_ref().map(|n| n.id)
}

/// What a lesson affords. Declared once, so it is both the buttons on the row
/// and its right-click menu, and what the keys do.
fn lesson_actions(host: RowHost<Action>) -> impl Fn(&LessonRow) -> Vec<RowAction> {
    move |row| {
        let lesson = row.lesson.id;
        let own_node = row.lesson.node_id;
        let mut out = Vec::new();
        if let Some(node) = open_target(row) {
            let h = host.clone();
            out.push(
                RowAction::new("open-node", "Open node", move |_, cx| {
                    h.push(Action::OpenNode { node, source: Source::Click }, cx);
                })
                .icon(IconName::ExternalLink)
                .tooltip("Open the node this lesson was recorded on, in its task panel"),
            );
        }
        if let Some(run) = &row.run {
            let (h, node) = (host.clone(), run.id);
            out.push(
                RowAction::new("open-review", "Open review", move |_, cx| {
                    h.push(Action::OpenNode { node, source: Source::Click }, cx);
                })
                .tooltip("Open the process-review node that took this lesson in")
                .menu_only(),
            );
        }
        if let AppliedRef::Node(applied) = &row.applied {
            let (h, node) = (host.clone(), applied.id);
            out.push(
                RowAction::new("open-applied", "Open the change", move |_, cx| {
                    h.push(Action::OpenNode { node, source: Source::Click }, cx);
                })
                .tooltip("Open the node that acted on this lesson"),
            );
        }
        if let (AppliedRef::Commit(commit), Some(node)) = (&row.applied, row.changes_node()) {
            let (h, c) = (host.clone(), commit.clone());
            out.push(
                RowAction::new("show-changes", "Show changes", move |_, cx| {
                    h.push(Action::ShowChanges { node, commit: c.clone(), source: Source::Click }, cx);
                })
                .tooltip("Show what this commit changed (git show --stat)"),
            );
            let h = host.clone();
            out.push(
                RowAction::new("open-editor", "Open in editor", move |_, cx| {
                    h.push(Action::OpenEditor { node, source: Source::Click }, cx);
                })
                .tooltip("Open the checkout that holds the commit in the code editor")
                .menu_only(),
            );
        }
        match row.lesson.status.as_str() {
            STATUS_DECLINED => {
                let h = host.clone();
                out.push(
                    RowAction::new("reopen", "Reopen", move |_, cx| {
                        h.push(Action::Reopen { lesson, node: own_node, source: Source::Click }, cx);
                    })
                    .tooltip("Take this lesson back: a review may group it again"),
                );
            }
            STATUS_APPLIED => {}
            _ => {
                let h = host.clone();
                out.push(
                    RowAction::new("decline", "Decline", move |_, cx| {
                        h.push(Action::Decline { lesson, node: own_node, source: Source::Click }, cx);
                    })
                    .tooltip("Do not act on this lesson; the process review leaves it out"),
                );
            }
        }
        let text = row.text();
        out.push(
            RowAction::new("copy", "Copy", move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            })
            .menu_only(),
        );
        out
    }
}

fn status_tone(status: &str) -> StatusTone {
    match status {
        "new" => StatusTone::Ready,
        "grouped" => StatusTone::Active,
        "applied" => StatusTone::Done,
        _ => StatusTone::Idle,
    }
}

fn columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::fixed(COLUMN_SOURCE, COLUMN_SOURCE, px(80.)),
        ColumnSpec::fixed(COLUMN_CATEGORY, COLUMN_CATEGORY, px(150.)),
        ColumnSpec::fixed(COLUMN_STATUS, COLUMN_STATUS, px(78.)),
        ColumnSpec::content(COLUMN_LESSON, COLUMN_LESSON),
    ]
}

/// The links a lesson carries, as one line of context.
fn links_text(row: &LessonRow) -> String {
    let l = &row.lesson;
    let mut parts = Vec::new();
    if let Some(n) = &row.node {
        parts.push(format!("node {}", n.label()));
    }
    if let Some(d) = l.decision_id {
        parts.push(format!("request {}", &d.simple().to_string()[..8]));
    }
    if let Some(c) = l.conversation_id {
        parts.push(format!("session {}", &c.simple().to_string()[..8]));
    }
    if l.send_back_id.is_some() {
        parts.push("send-back".to_string());
    }
    if l.incident_id.is_some() {
        parts.push("incident".to_string());
    }
    if let Some(run) = &row.run {
        parts.push(format!("review {}", run.label()));
    }
    parts.join(" · ")
}

fn lesson_row(
    row: &LessonRow,
    state: ItemRowState<'_>,
    host: &RowHost<Action>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let l = &row.lesson;
    let group = row_group(state.key);
    let select_host = host.clone();
    let row_ix = state.row_ix;
    let key = state.key.to_string();
    let muted = cx.theme().muted_foreground;
    let mut body = v_flex().min_w_0().gap_0p5().child(selectable_text(
        format!("lesson-summary-{key}"),
        l.summary.trim().to_string(),
        window,
        cx,
    ));
    if !l.user_quote.trim().is_empty() && l.user_quote.trim() != l.summary.trim() {
        body = body.child(selectable_text_colored(
            format!("lesson-quote-{key}"),
            format!("The user said: \u{201c}{}\u{201d}", l.user_quote.trim()),
            muted,
            window,
            cx,
        ));
    }
    if !l.target.is_empty() {
        body = body.child(selectable_text_colored(
            format!("lesson-target-{key}"),
            format!("Target: {}", l.target),
            muted,
            window,
            cx,
        ));
    }
    body = body.child(selectable_text_colored(format!("lesson-links-{key}"), links_text(row), muted, window, cx));
    style::row(h_flex())
        .group(group.clone())
        .w_full()
        .items_start()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            select_host.push(ItemListEvent::Select { row_ix }.into(), cx);
        })
        .when(state.highlighted, style::highlighted)
        .child(state.column(COLUMN_SOURCE, style::text_dense_muted(div())).child(l.source.clone()))
        .child(
            state
                .column(COLUMN_CATEGORY, style::text_dense_muted(div()))
                .child(l.category.replace('_', " ")),
        )
        .child(
            state
                .column(COLUMN_STATUS, div())
                .child(style::status_chip(div(), status_tone(&l.status)).child(l.status.clone())),
        )
        .child(state.column(COLUMN_LESSON, div().min_w_0()).child(body))
        .children(row_action_buttons(state.key, &group, state.highlighted, state.actions.to_vec()))
        .into_any_element()
}

fn build_rows(
    sections: &[Section],
    is_collapsed: impl Fn(&str) -> bool,
) -> Vec<ItemListRow<LessonRow>> {
    let mut rows = Vec::new();
    for section in sections {
        let collapsed = is_collapsed(&section.key);
        rows.push(ItemListRow::heading(
            GroupSpec::new(section.key.clone(), 0, section.heading.clone())
                .count(section.rows.len())
                .collapsed(collapsed),
        ));
        if collapsed {
            continue;
        }
        for row in &section.rows {
            rows.push(ItemListRow::item(format!("{}/{}", section.key, row.lesson.id), row.clone()));
        }
    }
    rows
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Lessons,
    Measures,
}

enum Load {
    Loading,
    Loaded(Box<Overview>),
    Failed(String),
}

/// The text of what an applied commit changed, as it arrives.
enum Changes {
    None,
    Loading(String),
    Shown { title: String, text: String },
    Failed { title: String, error: String },
}

struct SelfImprovement {
    fleet: Arc<FleetStore>,
    open_node: OpenNode,
    focus_handle: FocusHandle,
    load: Load,
    /// Bumped on every load: an older result is dropped when it lands.
    generation: u64,
    tab: Tab,
    list: ItemList<LessonRow>,
    host: RowHost<Action>,
    changes: Changes,
    /// A write or an editor open that failed.
    notice: Option<String>,
}

/// Opens the view. `open_node` shows a node's task panel in the shell.
pub fn open_self_improvement_dialog(
    window: &mut Window,
    cx: &mut App,
    fleet: Arc<FleetStore>,
    open_node: OpenNode,
) {
    let view = cx.new(|cx| {
        let host = RowHost::for_entity(cx.weak_entity());
        let list = ItemList::new()
            .with_columns(columns())
            .with_row_actions(lesson_actions(host.clone()))
            .with_row_text(LessonRow::text);
        SelfImprovement {
            fleet,
            open_node,
            focus_handle: cx.focus_handle(),
            load: Load::Loading,
            generation: 0,
            tab: Tab::Lessons,
            list,
            host,
            changes: Changes::None,
            notice: None,
        }
    });
    view.update(cx, |this, cx| this.reload(cx));
    let handle = view.read(cx).focus_handle.clone();
    window.open_dialog(cx, move |d, _, _| {
        d.title("Self-improvement")
            .w(px(1000.))
            .overlay(true)
            .overlay_closable(true)
            .keyboard(true)
            .close_button(true)
            .child(view.clone())
    });
    handle.focus(window, cx);
}

impl SelfImprovement {
    /// Read the lessons and the measures off the UI thread.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || fleet.read(tod_core::self_improvement::load).map_err(|e| format!("{e:#}")),
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                this.load = match result {
                    Ok(overview) => Load::Loaded(Box::new(overview)),
                    Err(error) => Load::Failed(error),
                };
                this.rebuild_rows();
                cx.notify();
            },
        );
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let rows = match &self.load {
            Load::Loaded(o) => build_rows(&o.sections, |key| self.list.is_collapsed(key)),
            _ => Vec::new(),
        };
        self.list.set_rows(rows);
    }

    fn record(&self, cx: &mut App, node: Uuid, action: &str, source: Source, row: Option<&LessonRow>) {
        let actions = match row {
            Some(row) => lesson_actions(self.host.clone())(row)
                .into_iter()
                .enumerate()
                .map(|(i, a)| PresentedAction {
                    id: a.id.to_string(),
                    label: a.label.to_string(),
                    primary: i == 0,
                    disabled: false,
                })
                .collect(),
            None => Vec::new(),
        };
        record_action(
            cx,
            Focus::Node(node),
            action,
            source,
            SURFACE,
            Presented { actions, focused: self.list.cursor_key().map(str::to_string), notices: Vec::new() },
        );
    }

    fn row_for(&self, lesson: Uuid) -> Option<LessonRow> {
        match &self.load {
            Load::Loaded(o) => o.rows().find(|r| r.lesson.id == lesson).cloned(),
            _ => None,
        }
    }

    /// Decline or reopen: a store write off the UI thread, then reload.
    fn set_declined(&mut self, lesson: Uuid, node: Uuid, decline: bool, source: Source, cx: &mut Context<Self>) {
        let row = self.row_for(lesson);
        self.record(cx, node, if decline { "decline_lesson" } else { "reopen_lesson" }, source, row.as_ref());
        self.notice = None;
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || {
                let op = if decline { lessons::Op::Decline { id: lesson } } else { lessons::Op::Reopen { id: lesson } };
                fleet.interview("user", InterviewCommand::Lessons(op)).map_err(|e| format!("{e:#}"))
            },
            move |this, result, cx| {
                if let Err(error) = result {
                    this.notice = Some(format!(
                        "Could not {} the lesson: {error}",
                        if decline { "decline" } else { "reopen" }
                    ));
                }
                this.reload(cx);
            },
        );
    }

    fn show_changes(&mut self, node: Uuid, commit: String, source: Source, cx: &mut Context<Self>) {
        self.record(cx, node, "show_lesson_changes", source, None);
        let title = format!("git show --stat {commit}");
        self.changes = Changes::Loading(title.clone());
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || changes_text(&fleet, node, &commit),
            move |this, result, cx| {
                this.changes = match result {
                    Ok(text) => Changes::Shown { title, text },
                    Err(error) => Changes::Failed { title, error },
                };
                cx.notify();
            },
        );
        cx.notify();
    }

    fn open_editor(&mut self, node: Uuid, source: Source, cx: &mut Context<Self>) {
        self.record(cx, node, "open_lesson_changes_in_editor", source, None);
        self.notice = None;
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || {
                let editor = tod_store::fleet::code_editor::code_editors()
                    .iter()
                    .copied()
                    .find(|e| e.is_available())
                    .ok_or_else(|| "no code editor found on this machine".to_string())?;
                tod_store::fleet::code_editor::open_code_editor_for_node(&fleet, editor, &node.to_string())
                    .map(|_| ())
                    .map_err(|e| format!("{e:#}"))
            },
            |this, result, cx| {
                if let Err(error) = result {
                    this.notice = Some(format!("Could not open the checkout in the editor: {error}"));
                    cx.notify();
                }
            },
        );
    }

    /// Close the dialog and show `node`'s task panel. Called from the view's
    /// own render or key handler, so it waits for them to finish: closing the
    /// dialog and moving the shell's focus would otherwise update this view
    /// while it is updating.
    fn open_node_panel(&mut self, node: Uuid, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        self.record(cx, node, "open_lesson_node", source, None);
        let open = self.open_node.clone();
        window.defer(cx, move |window, cx| {
            window.close_dialog(cx);
            open(node, window, cx);
        });
    }

    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                Action::Select { row_ix } => {
                    if self.list.set_cursor(row_ix) {
                        cx.notify();
                    }
                }
                Action::ToggleGroup { key } => {
                    self.list.toggle_collapsed(&key);
                    self.rebuild_rows();
                    cx.notify();
                }
                Action::OpenNode { node, source } => self.open_node_panel(node, source, window, cx),
                Action::Decline { lesson, node, source } => self.set_declined(lesson, node, true, source, cx),
                Action::Reopen { lesson, node, source } => self.set_declined(lesson, node, false, source, cx),
                Action::ShowChanges { node, commit, source } => self.show_changes(node, commit, source, cx),
                Action::OpenEditor { node, source } => self.open_editor(node, source, cx),
                Action::Ignored => {}
            }
        }
    }

    fn cursor_lesson(&self) -> Option<LessonRow> {
        self.list.cursor_item().cloned()
    }

    /// Enter: open the node of the lesson under the cursor; on a heading,
    /// collapse or expand it.
    fn on_open(&mut self, _: &SelfImprovementOpenNode, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.cursor_lesson() {
            if let Some(node) = open_target(&row) {
                self.open_node_panel(node, Source::Keyboard, window, cx);
            }
            return;
        }
        if let Some(key) = self.list.cursor_row().and_then(|r| r.as_group()).map(|g| g.key.clone()) {
            self.list.toggle_collapsed(&key);
            self.rebuild_rows();
            cx.notify();
        }
    }

    fn on_toggle_declined(&mut self, _: &SelfImprovementToggleDeclined, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.cursor_lesson() else { return };
        let (lesson, node) = (row.lesson.id, row.lesson.node_id);
        match row.lesson.status.as_str() {
            STATUS_DECLINED => self.set_declined(lesson, node, false, Source::Keyboard, cx),
            STATUS_APPLIED => {}
            _ => self.set_declined(lesson, node, true, Source::Keyboard, cx),
        }
    }

    fn on_show_changes(&mut self, _: &SelfImprovementShowChanges, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.cursor_lesson() else { return };
        if let (AppliedRef::Commit(commit), Some(node)) = (&row.applied, row.changes_node()) {
            self.show_changes(node, commit.clone(), Source::Keyboard, cx);
        }
    }

    fn on_refresh(&mut self, _: &SelfImprovementRefresh, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    fn on_toggle_tab(&mut self, _: &SelfImprovementToggleTab, _: &mut Window, cx: &mut Context<Self>) {
        self.tab = if self.tab == Tab::Lessons { Tab::Measures } else { Tab::Lessons };
        cx.notify();
    }

    fn move_cursor(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.list.move_cursor(delta) {
            cx.notify();
        }
    }

    fn on_arrow_up(&mut self, _: &ItemListUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, cx);
    }

    fn on_arrow_down(&mut self, _: &ItemListDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, cx);
    }

    fn on_page_up(&mut self, _: &ItemListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<LessonRow>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(-page, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<LessonRow>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(page, cx);
    }

    fn on_home(&mut self, _: &ItemListHome, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_home() {
            cx.notify();
        }
    }

    fn on_end(&mut self, _: &ItemListEnd, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_end() {
            cx.notify();
        }
    }

    fn on_collapse(&mut self, _: &ItemListCollapse, _: &mut Window, cx: &mut Context<Self>) {
        match self.list.collapse_step() {
            CollapseStep::Collapsed => {
                self.rebuild_rows();
                cx.notify();
            }
            CollapseStep::MovedToParent => cx.notify(),
            CollapseStep::Nothing => {}
        }
    }

    fn on_expand(&mut self, _: &ItemListExpand, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.expand_step() {
            self.rebuild_rows();
            cx.notify();
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let counts = match &self.load {
            Load::Loaded(o) => format!("{} lesson(s)", o.lesson_count()),
            Load::Loading => "Loading…".to_string(),
            Load::Failed(_) => String::new(),
        };
        h_flex()
            .gap(style::space::RELATED)
            .items_center()
            .child(
                Button::new("self-improvement-tab-lessons")
                    .label("Lessons")
                    .ghost()
                    .compact()
                    .selected(self.tab == Tab::Lessons)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = Tab::Lessons;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("self-improvement-tab-measures")
                    .label("Measures")
                    .ghost()
                    .compact()
                    .selected(self.tab == Tab::Measures)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = Tab::Measures;
                        cx.notify();
                    })),
            )
            .child(style::text_dense_muted(div()).child(counts))
            .child(div().flex_1())
            .child(
                Button::new("self-improvement-refresh")
                    .label("Refresh")
                    .ghost()
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }

    fn render_lessons(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: &str| {
            style::empty_message(div()).p(style::space::INSET).child(text.to_string()).into_any_element()
        };
        match &self.load {
            Load::Loading => message("Loading lessons…"),
            Load::Failed(error) => style::text_error(div())
                .p(style::space::INSET)
                .child(selectable_text("self-improvement-error", error.clone(), window, cx))
                .into_any_element(),
            Load::Loaded(o) if o.sections.is_empty() => message(
                "No lessons yet. They are recorded from triage, send-backs, quick fixes that escalated, tool incidents, and `tod-cli lessons add`.",
            ),
            Load::Loaded(_) => {
                let host = self.host.clone();
                self.list.render(
                    "self-improvement-list",
                    &self.host,
                    move |row, state, window, cx| lesson_row(row, state, &host, window, cx),
                    window,
                    cx,
                )
            }
        }
    }

    fn render_measures(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match &self.load {
            Load::Loaded(o) => {
                // The same text `tod-cli lessons stats` prints, including what is not measured.
                let text = o.stats.render().trim_end().to_string();
                div()
                    .id("self-improvement-measures-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .p(style::space::INSET)
                    .font_family("monospace")
                    .child(selectable_text("self-improvement-measures", text, window, cx))
                    .into_any_element()
            }
            Load::Loading => {
                style::empty_message(div()).p(style::space::INSET).child("Loading…").into_any_element()
            }
            Load::Failed(error) => style::text_error(div())
                .p(style::space::INSET)
                .child(selectable_text("self-improvement-measures-error", error.clone(), window, cx))
                .into_any_element(),
        }
    }

    fn render_changes(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = |title: String, body: AnyElement, cx: &mut Context<Self>| {
            v_flex()
                .flex_shrink_0()
                .max_h(px(180.))
                .overflow_hidden()
                .border_t_1()
                .border_color(cx.theme().border)
                .p(style::space::INSET)
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(style::text_dense_label(div()).child(title))
                        .child(
                            Button::new("self-improvement-close-changes")
                                .label("Hide")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.changes = Changes::None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(body)
                .into_any_element()
        };
        match &self.changes {
            Changes::None => None,
            Changes::Loading(title) => Some(panel(
                title.clone(),
                style::text_dense_muted(div()).child("Running git…").into_any_element(),
                cx,
            )),
            Changes::Shown { title, text } => Some(panel(
                title.clone(),
                div()
                    .id("self-improvement-changes-scroll")
                    .overflow_y_scroll()
                    .font_family("monospace")
                    .child(selectable_text("self-improvement-changes", text.clone(), window, cx))
                    .into_any_element(),
                cx,
            )),
            Changes::Failed { title, error } => Some(panel(
                title.clone(),
                style::text_error(div())
                    .child(selectable_text("self-improvement-changes-error", error.clone(), window, cx))
                    .into_any_element(),
                cx,
            )),
        }
    }
}

/// `git show --stat` of `commit` in the checkout `node` works in. Off the UI
/// thread: it reads the store and runs git (in a container or sandbox through
/// it). A checkout that is not made, or that does not have the commit, is an
/// error that says so.
fn changes_text(fleet: &FleetStore, node: Uuid, commit: &str) -> Result<String, String> {
    if !tod_core::self_improvement::is_commit_ref(commit) {
        return Err(format!("`{commit}` is not a commit id"));
    }
    let dir = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string())
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| {
            "That node's checkout has not been made (or was released), so the commit cannot be shown here. \
             Open the node and start its work to make it."
                .to_string()
        })?;
    dir.git(&["show", "--stat", "--format=fuller", commit])
        .map_err(|e| format!("git show {commit} in {}: {e:#}", dir.path_text()))
}

impl Focusable for SelfImprovement {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SelfImprovement {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_row_actions(window, cx);
        let body = match self.tab {
            Tab::Lessons => self.render_lessons(window, cx),
            Tab::Measures => self.render_measures(window, cx),
        };
        let notice = self.notice.clone();
        let changes = self.render_changes(window, cx);
        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .h(px(560.))
            .w_full()
            .gap_2()
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_toggle_declined))
            .on_action(cx.listener(Self::on_show_changes))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_toggle_tab))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_collapse))
            .on_action(cx.listener(Self::on_expand))
            .child(self.render_tabs(cx))
            .when_some(notice, |el, notice| {
                el.child(style::text_error(div()).child(selectable_text("self-improvement-notice", notice, window, cx)))
            })
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .children(changes)
            .child(style::text_dense_muted(div()).child(
                "\u{2191}/\u{2193} navigate \u{b7} Enter opens the node \u{b7} D declines or reopens \u{b7} C shows an applied commit's changes \u{b7} M measures \u{b7} R refreshes",
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::self_improvement::{NodeRef, sections};
    use tod_store::lessons::Lesson;

    fn lesson(status: &str, applied_ref: &str) -> Lesson {
        Lesson {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            source: "triage".into(),
            decision_id: Some(Uuid::new_v4()),
            conversation_id: None,
            send_back_id: None,
            incident_id: None,
            category: "brief_unclear".into(),
            target: "triage brief".into(),
            summary: "the brief was long".into(),
            user_quote: "too long".into(),
            status: status.into(),
            applied_ref: applied_ref.into(),
            created_at: 1,
            updated_at: 1,
            event_key: String::new(),
            run_node_id: None,
            grouped_at: None,
        }
    }

    fn row(status: &str, applied_ref: &str, applied: AppliedRef) -> LessonRow {
        let l = lesson(status, applied_ref);
        let node = Some(NodeRef { id: l.node_id, slug: "task".into(), title: "Task".into() });
        LessonRow { lesson: l, node, run: None, applied }
    }

    fn ids(actions: &[RowAction]) -> Vec<String> {
        actions.iter().map(|a| a.id.to_string()).collect()
    }

    fn host() -> RowHost<Action> {
        RowHost::new(|_| {})
    }

    #[test]
    fn a_new_lesson_can_be_declined_and_a_declined_one_reopened() {
        let new = lesson_actions(host())(&row("new", "", AppliedRef::None));
        assert_eq!(ids(&new), vec!["open-node", "decline", "copy"]);
        let declined = lesson_actions(host())(&row("declined", "", AppliedRef::None));
        assert_eq!(ids(&declined), vec!["open-node", "reopen", "copy"]);
    }

    #[test]
    fn an_applied_commit_can_be_shown_and_is_not_declinable() {
        let applied = lesson_actions(host())(&row("applied", "abc1234", AppliedRef::Commit("abc1234".into())));
        assert_eq!(ids(&applied), vec!["open-node", "show-changes", "open-editor", "copy"]);
        assert!(applied.iter().find(|a| a.id == "open-editor").unwrap().menu_only);
    }

    #[test]
    fn an_applied_node_links_to_it() {
        let node = NodeRef { id: Uuid::new_v4(), slug: "fix".into(), title: "Fix".into() };
        let applied = lesson_actions(host())(&row("applied", "fix", AppliedRef::Node(node)));
        assert_eq!(ids(&applied), vec!["open-node", "open-applied", "copy"]);
    }

    #[test]
    fn rows_have_a_heading_per_section_and_collapsed_ones_hide_their_lessons() {
        let secs = sections(vec![row("new", "", AppliedRef::None), row("declined", "", AppliedRef::None)]);
        let all = build_rows(&secs, |_| false);
        assert_eq!(all.len(), 4);
        let collapsed = build_rows(&secs, |key| key == "new");
        assert_eq!(collapsed.len(), 3, "the new section shows its heading alone");
    }

    #[test]
    fn links_name_the_node_the_request_and_the_review() {
        let mut r = row("grouped", "", AppliedRef::None);
        r.run = Some(NodeRef { id: Uuid::new_v4(), slug: "pr-1".into(), title: "Review".into() });
        let text = links_text(&r);
        assert!(text.starts_with("node Task (task) \u{b7} request "), "{text}");
        assert!(text.ends_with("review Review (pr-1)"), "{text}");
    }

    #[test]
    fn a_made_up_commit_is_refused_before_git_runs() {
        let root = tod_test_util::scratch_dir(format!("tod-lessons-view-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        let err = changes_text(&fleet, Uuid::new_v4(), "--output=/x").unwrap_err();
        assert!(err.contains("not a commit id"), "{err}");
    }
}
