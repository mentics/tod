//! The findings column panel: a node's review findings on the item list,
//! answered from the status chip — the same row and the same answer path as
//! `conversation/side_pane.rs`'s findings list, hosted on its own instead of
//! beside a conversation transcript.
//!
//! **E** on a finding opens the conversation that recorded it (its
//! transcript), when it still has one. An obligation or a plan step has no
//! panel of its own yet, so those panels bind no `E` handler at all.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};

use crate::ui::panel_export::RecordText as _;
use crate::ui::item_list::keyboard::{
    ItemListDown, ItemListEnd, ItemListHome, ItemListPageDown, ItemListPageUp, ItemListUp,
};
use crate::ui::item_list::{
    ItemList, ItemListEvent, ItemListKeys, ItemListRow, bind_item_list_keys,
};
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::unified::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::views::rows::{
    FindingRowEvent, FindingRowProps, RowHost, RowOptions, Score, StatusMenu, finding_columns,
    finding_row,
};
use tod_journey::Presented;
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::review::{
    FINDING_REJECTED, FINDING_STATUSES, LatestCoverage, ReviewFinding, ReviewRepo,
    USER_FINDING_STATUSES, coverage,
};
use uuid::Uuid;

const FINDINGS_CONTEXT: &str = "UnifiedFindings";

actions!(
    unified_findings,
    [FindingsOpenTranscript, FindingsOpenTranscriptCtrl, FindingsStatusMenu]
);

/// Register the findings panel's own keys, alongside every other
/// `register_*_keyboard_bindings`.
pub fn register_findings_keyboard_bindings(cx: &mut App) {
    // A findings list is flat (no grouping) and answered from the status
    // chip, not edited in place: navigation only.
    bind_item_list_keys(cx, FINDINGS_CONTEXT, ItemListKeys::default());
    bind_modified_pane_nav(cx, FINDINGS_CONTEXT);
    let context = Some(key_context::excluding_input(FINDINGS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("e", FindingsOpenTranscript, context),
        KeyBinding::new("ctrl-e", FindingsOpenTranscriptCtrl, context),
        KeyBinding::new("t", FindingsStatusMenu, context),
    ]);
}

#[derive(Debug, Clone)]
struct FindingItem {
    finding: ReviewFinding,
}

type FindingRow = ItemListRow<FindingItem>;

#[derive(Debug, Clone)]
enum ListAction {
    Select { row_ix: usize },
    ToggleStatusMenu { finding_id: Uuid },
    ChooseStatus { finding_id: Uuid, status: &'static str },
    DismissStatusMenu,
    CycleScore { finding_id: Uuid, score: Score },
    Ignored,
}

impl From<FindingRowEvent> for ListAction {
    fn from(event: FindingRowEvent) -> Self {
        match event {
            FindingRowEvent::Select { row_ix } => Self::Select { row_ix },
            FindingRowEvent::ToggleStatusMenu { finding_id } => {
                Self::ToggleStatusMenu { finding_id }
            }
            FindingRowEvent::ChooseStatus { finding_id, status } => {
                Self::ChooseStatus { finding_id, status }
            }
            FindingRowEvent::DismissStatusMenu => Self::DismissStatusMenu,
            FindingRowEvent::CycleScore { finding_id, score } => Self::CycleScore { finding_id, score },
        }
    }
}

impl From<ItemListEvent> for ListAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            // Findings are a flat run: no group to collapse, no checkbox.
            ItemListEvent::ToggleGroup { .. } | ItemListEvent::ToggleMark { .. } => Self::Ignored,
            ItemListEvent::Drop(_) => Self::Ignored,
        }
    }
}

use super::node_title;

/// One coverage line: the lens (or pass), then what it found.
fn coverage_line(
    id: String,
    pass: String,
    result: String,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    h_flex()
        .gap(style::space::RELATED)
        .items_start()
        .child(
            style::text_dense_muted(div())
                .w(style::size::TABLE_CELL)
                .flex_shrink_0()
                .child(pass.clone())
                .record_text(pass),
        )
        .child(style::text_dense(div()).min_w_0().child(selectable_text(id, result, window, cx)))
        .into_any_element()
}

pub struct FindingsPanel {
    fleet: Arc<FleetStore>,
    node_id: Uuid,
    items: Vec<ReviewFinding>,
    /// The latest review's coverage record: what it looked at, lens by lens,
    /// and what each lens found. `None` when the node was never reviewed; an
    /// error is shown, not hidden.
    coverage: Result<Option<LatestCoverage>, String>,
    focus_handle: FocusHandle,
    list: ItemList<FindingItem>,
    host: RowHost<ListAction>,
    status_menu: Option<StatusMenu>,
    /// Why the user's last edit of a score did not go through; shown, not
    /// hidden, until the next one succeeds.
    edit_error: Option<String>,
}

impl FindingsPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = RowHost::for_entity(cx.weak_entity());
        let mut this = Self {
            fleet,
            node_id,
            items: Vec::new(),
            coverage: Ok(None),
            focus_handle: cx.focus_handle(),
            list: ItemList::new().with_columns(finding_columns()),
            host,
            status_menu: None,
            edit_error: None,
        };
        this.reload(cx);
        this
    }

    #[cfg(test)]
    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let node_id = self.node_id;
        self.items = self
            .fleet
            .read(|conn| ReviewRepo::new(conn).list_for_node(node_id))
            .unwrap_or_default();
        self.coverage = self
            .fleet
            .read(|conn| coverage::latest_for_node(conn, node_id))
            .map_err(|err| format!("Could not read the review's coverage: {err:#}"));
        let rows: Vec<FindingRow> = self
            .items
            .iter()
            .cloned()
            .map(|finding| ItemListRow::item(finding.id.to_string(), FindingItem { finding }))
            .collect();
        self.list.set_rows(rows);
        cx.notify();
    }

    /// The latest review's coverage, compact: one line per lens, its result
    /// beside it, then the lenses it has not covered yet. Nothing while the
    /// node was never reviewed.
    fn render_coverage(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let border = cx.theme().border;
        let section = v_flex()
            .flex_shrink_0()
            .px(style::space::INSET)
            .py(style::space::RELATED)
            .gap(style::space::HAIRLINE)
            .border_t_1()
            .border_color(border);
        let latest = match &self.coverage {
            Ok(None) => return None,
            Ok(Some(latest)) => latest,
            Err(err) => {
                return Some(
                    section
                        .child(style::text_error(div()).child(selectable_text(
                            "unified-findings-coverage-error",
                            err.clone(),
                            window,
                            cx,
                        )))
                        .into_any_element(),
                );
            }
        };
        let summary = latest.summary_label();
        let mut section =
            section.child(style::text_dense_label(div()).child(summary.clone()).record_text(summary));
        for entry in &latest.entries {
            section = section.child(coverage_line(
                format!("unified-findings-coverage-{}", entry.id),
                entry.pass.title(),
                entry.result_label(),
                window,
                cx,
            ));
        }
        for lens in &latest.gaps.missing_lenses {
            section = section.child(coverage_line(
                format!("unified-findings-coverage-missing-{}", lens.slug),
                lens.title.to_string(),
                "not recorded".to_string(),
                window,
                cx,
            ));
        }
        Some(section.into_any_element())
    }

    fn selected_finding(&self) -> Option<ReviewFinding> {
        self.list.cursor_item().map(|item| item.finding.clone())
    }

    fn open_status_menu(&mut self, finding_id: Uuid, cx: &mut Context<Self>) {
        let Some(f) = self.items.iter().find(|f| f.id == finding_id) else {
            return;
        };
        let options: &'static [&'static str] = if f.status == FINDING_REJECTED {
            &FINDING_STATUSES
        } else {
            &USER_FINDING_STATUSES
        };
        self.status_menu = Some(StatusMenu::open(finding_id, options, &f.status));
        cx.notify();
    }

    fn close_status_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.status_menu.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    /// Answer a finding as the user; a reopen (back to `open`) clears its
    /// response, same as `conversation/side_pane.rs`.
    fn respond_to_finding(&mut self, finding_id: Uuid, status: &str, cx: &mut Context<Self>) {
        self.status_menu = None;
        let Some(current) = self.items.iter().find(|f| f.id == finding_id) else {
            return;
        };
        if current.status != status {
            let _ = self.fleet.interview(
                ACTOR_USER,
                InterviewCommand::RespondReviewFinding {
                    finding_id,
                    status: status.to_string(),
                    response: current.response.clone(),
                },
            );
            self.reload(cx);
        }
        cx.notify();
    }

    /// The user edits a finding's score: it moves to the next value, which the
    /// store checks like a new finding's. Recorded as a journey `UserAction`.
    fn cycle_score(&mut self, finding_id: Uuid, score: Score, cx: &mut Context<Self>) {
        self.status_menu = None;
        let Some(current) = self.items.iter().find(|f| f.id == finding_id) else {
            return;
        };
        let value = match score {
            Score::Importance => &current.importance,
            Score::Impact => &current.impact,
            Score::Complexity => &current.complexity,
        };
        let next = score.next(value).to_string();
        crate::ui::journey::record_action(
            cx,
            Focus::Node(self.node_id),
            format!("RescoreFinding:{score:?}:{next}"),
            crate::ui::journey::Source::Click,
            "findings_panel",
            Presented { actions: Vec::new(), focused: None, notices: Vec::new() },
        );
        let (importance, impact, complexity) = match score {
            Score::Importance => (Some(next), None, None),
            Score::Impact => (None, Some(next), None),
            Score::Complexity => (None, None, Some(next)),
        };
        self.edit_error = self
            .fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::RescoreReviewFinding { finding_id, importance, impact, complexity },
            )
            .err()
            .map(|err| format!("Could not change the finding's score: {err}"));
        self.reload(cx);
        cx.notify();
    }

    fn drain_row_actions(&mut self, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                ListAction::Select { row_ix } => {
                    self.status_menu = None;
                    self.list.set_cursor(row_ix);
                    cx.notify();
                }
                ListAction::ToggleStatusMenu { finding_id } => {
                    if !self
                        .status_menu
                        .take()
                        .is_some_and(|m| m.is_on(finding_id))
                    {
                        self.open_status_menu(finding_id, cx);
                    }
                    cx.notify();
                }
                ListAction::ChooseStatus { finding_id, status } => {
                    self.respond_to_finding(finding_id, status, cx);
                }
                ListAction::DismissStatusMenu => {
                    self.close_status_menu(cx);
                }
                ListAction::CycleScore { finding_id, score } => {
                    self.cycle_score(finding_id, score, cx);
                }
                ListAction::Ignored => {}
            }
        }
    }

    fn move_status_menu(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.status_menu.as_mut() else {
            return false;
        };
        menu.move_highlight(delta);
        cx.notify();
        true
    }

    fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.list.move_cursor(delta) {
            self.status_menu = None;
            cx.notify();
        }
    }

    fn on_arrow_up(&mut self, _: &ItemListUp, _window: &mut Window, cx: &mut Context<Self>) {
        if self.move_status_menu(-1, cx) {
            return;
        }
        self.move_selection(-1, cx);
    }

    fn on_arrow_down(&mut self, _: &ItemListDown, _window: &mut Window, cx: &mut Context<Self>) {
        if self.move_status_menu(1, cx) {
            return;
        }
        self.move_selection(1, cx);
    }

    fn on_page_up(&mut self, _: &ItemListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<FindingItem>::page_rows(window.viewport_size().height) as i32;
        self.move_selection(-page, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<FindingItem>::page_rows(window.viewport_size().height) as i32;
        self.move_selection(page, cx);
    }

    fn on_home(&mut self, _: &ItemListHome, _window: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_home() {
            cx.notify();
        }
    }

    fn on_end(&mut self, _: &ItemListEnd, _window: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_end() {
            cx.notify();
        }
    }

    fn on_status_menu(
        &mut self,
        _: &FindingsStatusMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_status_menu(cx) {
            return;
        }
        if let Some(finding) = self.selected_finding() {
            self.open_status_menu(finding.id, cx);
        }
    }

    /// `E`: open the finding's own transcript — the conversation that
    /// recorded it, while it still exists.
    fn on_open_transcript(
        &mut self,
        _: &FindingsOpenTranscript,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_transcript(false, cx);
    }

    /// Ctrl+E: the same, opened as a Ctrl+click would (beside this column,
    /// not replacing it).
    fn on_open_transcript_ctrl(
        &mut self,
        _: &FindingsOpenTranscriptCtrl,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_transcript(true, cx);
    }

    fn open_transcript(&mut self, ctrl: bool, cx: &mut Context<Self>) {
        let Some(conversation_id) = self.selected_finding().and_then(|f| f.conversation_id)
        else {
            return;
        };
        cx.emit(PanelOpenRequest {
            target: PanelKind::Transcript(conversation_id),
            ctrl,
        });
    }
}

impl ColumnPanel for FindingsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Findings".into()
    }

}

impl EventEmitter<PanelOpenRequest> for FindingsPanel {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{AppContext, Entity, TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tod_store::interview::ACTOR_AGENT;
    use tod_store::review::NewFinding;

    fn add_finding(fixture: &Fixture) -> Uuid {
        fixture
            .store
            .interview(
                ACTOR_AGENT,
                InterviewCommand::AddReviewFinding {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    finding: NewFinding {
                        importance: "blocker".into(), impact: "high".into(), complexity: "simple".into(),
                        summary: "Off-by-one in the loop".into(),
                        file: None,
                        line: None,
                        detail: None,
                        root_cause: None,
                        rule: None,
                    },
                },
            )
            .unwrap();
        fixture
            .store
            .read(|conn| Ok(ReviewRepo::new(conn).list_for_node(fixture.node_id)?[0].id))
            .unwrap()
    }

    fn open_view<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<FindingsPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let (store, node_id) = (fixture.store.clone(), fixture.node_id);
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| FindingsPanel::new(node_id, store, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (view, cx)
    }

    #[gpui::test]
    fn opens_for_a_node_and_shows_its_findings(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let finding_id = add_finding(&fixture);
        let (view, cx) = open_view(&fixture, cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.node_id(), fixture.node_id);
            assert_eq!(view.items.len(), 1);
            assert_eq!(view.items[0].id, finding_id);
        });
    }

    #[gpui::test]
    fn shows_the_latest_reviews_coverage(cx: &mut TestAppContext) {
        use tod_store::conversation::{Focus, ProtocolKind};
        use tod_store::review::{CoveragePass, CoverageResult, NewCoverage};

        let fixture = Fixture::new();
        let review = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_AGENT,
                InterviewCommand::CreateConversation {
                    id: review,
                    focus: Focus::Node(fixture.node_id),
                    protocol: ProtocolKind::Review,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_AGENT,
                InterviewCommand::RecordReviewCoverage {
                    conversation_id: review,
                    entry: NewCoverage {
                        pass: CoveragePass::Lens("reach".into()),
                        files: vec!["src/import.rs".into()],
                        checked: Some("every caller of the importer".into()),
                        result: CoverageResult::None,
                        read: Vec::new(),
                    },
                },
            )
            .unwrap();
        let (view, cx) = open_view(&fixture, cx);

        view.read_with(cx, |view, _| {
            let latest = view.coverage.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(latest.conversation_id, review);
            assert_eq!(latest.entries.len(), 1);
            assert_eq!(latest.entries[0].result_label(), "nothing found");
            assert_eq!(latest.gaps.missing_lenses.len(), 10);
            assert_eq!(latest.summary_label(), "Coverage 1/11 lenses · saturation pass owed");
        });
    }

    #[gpui::test]
    fn choosing_a_status_answers_the_finding(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let finding_id = add_finding(&fixture);
        let (view, cx) = open_view(&fixture, cx);

        view.update(cx, |view, cx| {
            view.respond_to_finding(finding_id, tod_store::review::FINDING_DECLINED, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(view.items[0].status, tod_store::review::FINDING_DECLINED);
        });
    }

    /// A click on a score moves it to its next value and the panel shows it.
    #[gpui::test]
    fn clicking_a_score_moves_it_to_its_next_value(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let finding_id = add_finding(&fixture);
        let (view, cx) = open_view(&fixture, cx);

        view.update(cx, |view, cx| {
            view.cycle_score(finding_id, Score::Importance, cx);
            view.cycle_score(finding_id, Score::Complexity, cx);
        });

        view.read_with(cx, |view, _| {
            // blocker -> major, simple -> involved; impact untouched.
            assert_eq!(
                (view.items[0].importance.as_str(), view.items[0].impact.as_str(), view.items[0].complexity.as_str()),
                ("major", "high", "involved")
            );
            assert!(view.edit_error.is_none());
        });
    }

    /// Answering `open` on a finding tod left makes it block again (like a
    /// major, from a minor or a nit).
    #[gpui::test]
    fn reopening_a_left_finding_makes_it_block(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let finding_id = add_finding(&fixture);
        fixture
            .store
            .interview(ACTOR_AGENT, InterviewCommand::RescoreReviewFinding {
                finding_id,
                importance: Some("nit".into()),
                impact: None,
                complexity: None,
            })
            .unwrap();
        fixture
            .store
            .interview(ACTOR_AGENT, InterviewCommand::RespondReviewFinding {
                finding_id,
                status: tod_store::review::FINDING_LEFT.into(),
                response: Some("nit: left as it is".into()),
            })
            .unwrap();
        let (view, cx) = open_view(&fixture, cx);
        view.read_with(cx, |view, _| assert!(view.items[0].dismissed()));

        view.update(cx, |view, cx| {
            view.respond_to_finding(finding_id, tod_store::review::FINDING_OPEN, cx);
        });

        view.read_with(cx, |view, _| {
            assert!(view.items[0].stands());
            assert_eq!(view.items[0].importance, "major");
        });
    }
}

impl Focusable for FindingsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FindingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_row_actions(cx);
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let title = node_title(&self.fleet, self.node_id);
        let empty = self.items.is_empty();

        v_flex()
            .key_context(FINDINGS_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(|_, _: &PaneFocusLeft, _, cx| cx.propagate()))
            .on_action(cx.listener(Self::on_status_menu))
            .on_action(cx.listener(Self::on_open_transcript))
            .on_action(cx.listener(Self::on_open_transcript_ctrl))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(title.clone())
                    .record_text(title),
            )
            .child(if empty {
                div()
                    .p_3()
                    .text_sm()
                    .text_color(muted)
                    .child("No findings recorded.")
                    .record_text("No findings recorded.")
                    .into_any_element()
            } else {
                let row_host = self.host.clone();
                let menu = self.status_menu;
                self.list.render(
                    "unified-findings-scroll",
                    &self.host,
                    move |item, state, window, cx| {
                        let props = FindingRowProps {
                            finding: &item.finding,
                            row_ix: state.row_ix,
                            highlighted: state.highlighted,
                            status_menu: menu,
                            columns: state.columns,
                        };
                        finding_row(props, &row_host, RowOptions::default(), window, cx)
                    },
                    window,
                    cx,
                )
            })
            .children(self.render_coverage(window, cx))
            .children(self.edit_error.clone().map(|why| {
                div().px_3().py_1().text_xs().text_color(cx.theme().danger).child(selectable_text(
                    "unified-findings-edit-error",
                    why,
                    window,
                    cx,
                ))
            }))
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child("↑/↓ navigate · T sets status (open reopens a left finding) · click a score to change it · E opens transcript"),
            )
    }
}
