//! Drafting v3 — the view for a node in `proposed` (capture) or `design`
//! (the drafting loop). The user dumps whatever they have; the drafter turns
//! it into obligations; the user reviews what the drafter flagged and answers
//! the rare choice. Spec: `doc/drafting/protocol.md`.

use crate::interview::agent::SharedAgent;
use crate::interview::{TaskListProceedContext, TodPaths, TodSettings};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context::{self, set_input_tab_stop};
use crate::ui::pane_nav::{PaneFocusLeft, PaneFocusRight, bind_pane_nav};
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::views::obligations::{ObligationsEvent, ObligationsView};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, actions,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_core::drafting::DraftingMode;
use tod_core::drafting::driver::{DraftingConfig, DraftingDriver, DraftingEvent, DraftingStatus};
use tod_core::process_bundle::{ProcessManifest, TodInstallPaths, drafting_session_prefix};
use tod_store::drafting::{
    ATTENTION_HIGH, ATTENTION_LOW, ATTENTION_MEDIUM, CHOICE_OPEN, DraftingChoice, DraftingDump,
    DraftingRepo, DraftingSummary, MarkedObligation, NodePick,
};
use tod_store::fleet::{FleetStore, ensure_interview_agent_for_node};
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{OUTCOME_FAIL, OUTCOME_PASS, OutlineMutation};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const CONTEXT: &str = "Drafting";
const PICKER_TAG: &str = "DraftingPicker";
const RECENT_SUMMARIES: usize = 8;
const RECENT_DUMPS: usize = 5;
const PICKER_MATCHES: usize = 8;
const REVIEW_COLUMN_WIDTH: f32 = 360.;
const DUMP_COLUMN_WIDTH: f32 = 340.;
const COLUMN_MIN: f32 = 200.;
const DUMP_HEIGHT: f32 = 120.;

actions!(
    drafting,
    [
        DraftUp,
        DraftDown,
        DraftActivate,
        DraftConfirm,
        DraftEdit,
        DraftDelete,
        DraftSendElsewhere,
        DraftYouPick,
        DraftDigit1,
        DraftDigit2,
        DraftDigit3,
        DraftDigit4,
        DraftDigit5,
        DraftDigit6,
        DraftDigit7,
        DraftDigit8,
        DraftDigit9,
        DraftSubmit,
        DraftEscape,
        DraftBack,
        DraftFocusDump,
        DraftPickerUp,
        DraftPickerDown,
    ]
);

pub fn register_drafting_keyboard_bindings(cx: &mut App) {
    let nav = Some(key_context::excluding_input(CONTEXT));
    let input = Some(key_context::including_input(CONTEXT));
    let picker = Some(key_context::including_tag(CONTEXT, PICKER_TAG));
    cx.bind_keys([
        KeyBinding::new("up", DraftUp, nav),
        KeyBinding::new("down", DraftDown, nav),
        KeyBinding::new("enter", DraftActivate, nav),
        KeyBinding::new("c", DraftConfirm, nav),
        KeyBinding::new("e", DraftEdit, nav),
        KeyBinding::new("d", DraftDelete, nav),
        KeyBinding::new("delete", DraftDelete, nav),
        KeyBinding::new("m", DraftSendElsewhere, nav),
        KeyBinding::new("y", DraftYouPick, nav),
        KeyBinding::new("1", DraftDigit1, nav),
        KeyBinding::new("2", DraftDigit2, nav),
        KeyBinding::new("3", DraftDigit3, nav),
        KeyBinding::new("4", DraftDigit4, nav),
        KeyBinding::new("5", DraftDigit5, nav),
        KeyBinding::new("6", DraftDigit6, nav),
        KeyBinding::new("7", DraftDigit7, nav),
        KeyBinding::new("8", DraftDigit8, nav),
        KeyBinding::new("9", DraftDigit9, nav),
        KeyBinding::new("i", DraftFocusDump, nav),
        KeyBinding::new("escape", DraftEscape, nav),
        KeyBinding::new("alt-left", DraftBack, nav),
        KeyBinding::new("ctrl-enter", DraftSubmit, input),
        KeyBinding::new("escape", DraftEscape, input),
        KeyBinding::new("up", DraftPickerUp, picker),
        KeyBinding::new("down", DraftPickerDown, picker),
    ]);
    bind_pane_nav(cx, CONTEXT);
}

#[derive(Debug, Clone)]
pub enum DraftingViewEvent {
    ReturnToTaskList,
    ProceedToLifecycle { task_id: String, lifecycle: String },
    /// Forwarded from the embedded obligations panel's chat icon.
    OpenAgentChat {
        node_id: Uuid,
        obligation_id: Option<Uuid>,
        config_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Review,
    Dump,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewItem {
    Choice(DraftingChoice),
    Obligation(MarkedObligation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewKey {
    Choice(i64),
    Obligation(Uuid),
}

impl ReviewItem {
    fn key(&self) -> ReviewKey {
        match self {
            Self::Choice(c) => ReviewKey::Choice(c.seq),
            Self::Obligation(m) => ReviewKey::Obligation(m.obligation.id),
        }
    }
}

struct OpenNode {
    id: Uuid,
    title: String,
    lifecycle: String,
}

impl OpenNode {
    fn mode(&self) -> DraftingMode {
        DraftingMode::for_lifecycle(&self.lifecycle).unwrap_or(DraftingMode::Drafting)
    }
}

/// What the view shows for the open node, re-read every poll.
#[derive(Default, PartialEq)]
struct Snapshot {
    lifecycle: String,
    review: Vec<ReviewItem>,
    summaries: Vec<DraftingSummary>,
    dumps: Vec<DraftingDump>,
    /// (outcome, detail) of the buildable criterion.
    buildable: Option<(String, Option<String>)>,
    pre_v3_node: usize,
    pre_v3_subtree: usize,
}

struct Picker {
    obligation_id: Uuid,
    input: Entity<InputState>,
    nodes: Vec<NodePick>,
    matches: Vec<NodePick>,
    selected: usize,
    _subscription: Subscription,
}

pub struct DraftingView {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    paths: TodPaths,
    focus_handle: FocusHandle,
    node: Option<OpenNode>,
    proceed: Option<TaskListProceedContext>,
    /// Drafters by node. They outlive the open node so switching nodes never
    /// loses a turn in flight, and a subtree rewrite runs every node's drafter.
    drivers: HashMap<Uuid, Arc<Mutex<DraftingDriver>>>,
    status: DraftingStatus,
    data: Snapshot,
    selected: Option<ReviewKey>,
    /// The user moved the selection themselves; stop following the queue's top.
    user_selected: bool,
    pane: Pane,
    dump_input: Entity<TextareaState>,
    dump_editing: bool,
    edit_input: Entity<TextareaState>,
    editing: Option<Uuid>,
    picker: Option<Picker>,
    error: Option<SharedString>,
    status_line: SharedString,
    obligations: Entity<ObligationsView>,
    _obligations_subscription: Subscription,
    _poll_task: Task<()>,
    app_nav: AppNavMenu,
}

impl EventEmitter<DraftingViewEvent> for DraftingView {}

impl DraftingView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        fleet: Arc<FleetStore>,
    ) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let dump_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(5)
                .placeholder("Dump anything — Enter to write, Ctrl+Enter to send")
        });
        let edit_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Obligation text (Ctrl+Enter to save, Esc to cancel)")
        });
        let obligations = cx.new(|cx| ObligationsView::new(window, cx, fleet.clone()));
        let _obligations_subscription = cx.subscribe_in(
            &obligations,
            window,
            |this, panel, event, window, cx| match event {
                // The column is always open while drafting; reverse a Close.
                ObligationsEvent::Close => {
                    if let Some(node) = &this.node {
                        let (id, title, phase) = (node.id, node.title.clone(), node.mode().phase());
                        panel.update(cx, |panel, cx| {
                            panel.retarget(id, &title, Some(phase), true, window, cx);
                        });
                    }
                }
                ObligationsEvent::FocusTaskList => this.focus_pane(Pane::Dump, window, cx),
                ObligationsEvent::OpenAgentChat {
                    node_id,
                    obligation_id,
                    config_id,
                } => cx.emit(DraftingViewEvent::OpenAgentChat {
                    node_id: *node_id,
                    obligation_id: *obligation_id,
                    config_id: config_id.clone(),
                }),
                ObligationsEvent::RewritePreV3 { .. } => this.request_rewrite(false, cx),
                ObligationsEvent::DeleteSelectedTask
                | ObligationsEvent::OpenAgentConfig { .. }
                | ObligationsEvent::OpenVisualDesign { .. } => {}
            },
        );
        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.poll() {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });
        Self {
            fleet,
            agent,
            paths,
            focus_handle: cx.focus_handle().tab_stop(true),
            node: None,
            proceed: None,
            drivers: HashMap::new(),
            status: DraftingStatus::default(),
            data: Snapshot::default(),
            selected: None,
            user_selected: false,
            pane: Pane::Review,
            dump_input,
            dump_editing: false,
            edit_input,
            editing: None,
            picker: None,
            error: None,
            status_line: SharedString::default(),
            obligations,
            _obligations_subscription,
            _poll_task: poll_task,
            app_nav: AppNavMenu::default(),
        }
    }

    pub fn close_app_nav(&mut self) {
        self.app_nav.close();
    }

    /// Open drafting for `node_id`. `rewrite` asks the drafter to rewrite
    /// pre-v3 obligations on the node (`Some(false)`) or its whole subtree
    /// (`Some(true)`).
    pub fn open(
        &mut self,
        node_id: Uuid,
        proceed: Option<TaskListProceedContext>,
        rewrite: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, lifecycle) = self
            .fleet
            .read(|conn| {
                let nodes = NodeRepo::new(conn);
                let title = nodes.get(node_id)?.map(|n| n.title).unwrap_or_default();
                let lifecycle = nodes.get_lifecycle(node_id)?.unwrap_or_default();
                Ok((title, lifecycle))
            })
            .unwrap_or_default();
        let switching = self.node.as_ref().is_none_or(|n| n.id != node_id);
        self.node = Some(OpenNode {
            id: node_id,
            title: title.clone(),
            lifecycle: lifecycle.clone(),
        });
        self.proceed = proceed;
        if switching {
            self.data = Snapshot::default();
            self.selected = None;
            self.user_selected = false;
            self.editing = None;
            self.picker = None;
            self.dump_editing = false;
            self.error = None;
            self.status_line = SharedString::default();
            self.status = DraftingStatus::default();
        }
        let phase = self.node.as_ref().map(|n| n.mode().phase());
        self.obligations.update(cx, |panel, cx| {
            panel.retarget(node_id, &title, phase, false, window, cx);
        });
        if let Err(err) = self.ensure_driver(node_id) {
            self.error = Some(err.into());
        }
        if let Some(subtree) = rewrite {
            self.request_rewrite(subtree, cx);
        }
        self.reload();
        self.pane = Pane::Review;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Drafter turns in flight, for the close-window warning.
    pub fn running_drafting_work(&self) -> Vec<String> {
        self.drivers
            .values()
            .filter_map(|d| {
                let d = d.lock().ok()?;
                d.status()
                    .busy()
                    .then(|| format!("Drafter running: {}", d.config().node_title))
            })
            .collect()
    }

    /// The node's drafter, built (or rebuilt, when the node's lifecycle moved
    /// it to another mode) on demand.
    fn ensure_driver(&mut self, node_id: Uuid) -> Result<Arc<Mutex<DraftingDriver>>, String> {
        let (title, lifecycle) = self
            .fleet
            .read(|conn| {
                let nodes = NodeRepo::new(conn);
                Ok((
                    nodes.get(node_id)?.map(|n| n.title).unwrap_or_default(),
                    nodes.get_lifecycle(node_id)?.unwrap_or_default(),
                ))
            })
            .map_err(|e| format!("{e:#}"))?;
        let mode = DraftingMode::for_lifecycle(&lifecycle).unwrap_or(DraftingMode::Drafting);
        // Only the node the user has open in `design` starts a round unprompted.
        let kickoff = lifecycle == "design" && self.node.as_ref().is_some_and(|n| n.id == node_id);
        if let Some(existing) = self.drivers.get(&node_id) {
            let keep = existing.lock().map_or(true, |d| {
                let config = d.config();
                d.status().busy() || (config.mode == mode && config.kickoff == kickoff)
            });
            if keep {
                return Ok(existing.clone());
            }
        }
        let settings = TodSettings::load(&self.paths).unwrap_or_default();
        let agent_ctx = ensure_interview_agent_for_node(
            &self.fleet,
            &self.paths,
            &settings,
            &node_id.to_string(),
        )
        .map_err(|e| format!("Drafting agent setup failed: {e}"))?;
        let install = TodInstallPaths::discover().map_err(|e| format!("Process bundle: {e}"))?;
        let manifest = ProcessManifest::load(&install).map_err(|e| format!("Process bundle: {e}"))?;
        let prefix =
            drafting_session_prefix(&manifest, mode).map_err(|e| format!("Process bundle: {e}"))?;
        let driver = Arc::new(Mutex::new(DraftingDriver::new(DraftingConfig {
            node_id,
            node_title: title,
            mode,
            agent_config_id: agent_ctx.agent.id.clone(),
            repo_cwd: agent_ctx.cwd,
            data_root: self.fleet.paths().root().to_path_buf(),
            tod_cli: tod_core::interview::tod_cli_path(),
            launch: settings.interview_launch_options(),
            context: settings.interview_context.clone(),
            prefix,
            kickoff,
        })));
        self.drivers.insert(node_id, driver.clone());
        Ok(driver)
    }

    fn request_rewrite(&mut self, subtree: bool, cx: &mut Context<Self>) {
        let Some(node_id) = self.node.as_ref().map(|n| n.id) else {
            return;
        };
        let counts = match self
            .fleet
            .read(|conn| DraftingRepo::new(conn).pre_v3_counts(node_id, subtree))
        {
            Ok(counts) => counts,
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                cx.notify();
                return;
            }
        };
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        let mut errors = Vec::new();
        for (id, _) in &counts {
            match self.ensure_driver(*id) {
                Ok(driver) => {
                    if let Ok(mut driver) = driver.lock() {
                        driver.request_rewrite();
                    }
                }
                Err(err) => errors.push(err),
            }
        }
        self.error = (!errors.is_empty()).then(|| errors.join("\n").into());
        self.status_line = if total == 0 {
            "No pre-v3 obligations to rewrite".into()
        } else {
            format!(
                "Rewriting {total} pre-v3 obligation{} on {} node{}",
                plural(total),
                counts.len(),
                plural(counts.len())
            )
            .into()
        };
        cx.notify();
    }

    /// Advance every drafter and refresh from the database. Returns whether
    /// anything visible changed.
    fn poll(&mut self) -> bool {
        let open = self.node.as_ref().map(|n| n.id);
        let mut finished = Vec::new();
        let mut status = None;
        if let Ok(mut agent) = self.agent.try_lock() {
            for (node_id, driver) in &self.drivers {
                let Ok(mut driver) = driver.lock() else {
                    continue;
                };
                for event in driver.tick(&self.fleet, agent.as_mut()) {
                    let DraftingEvent::TurnFinished { error } = event;
                    finished.push((*node_id, driver.config().node_title.clone(), error));
                }
                if Some(*node_id) == open {
                    status = Some(driver.status());
                }
            }
        }
        let mut changed = !finished.is_empty();
        for (node_id, title, error) in finished {
            match error {
                Some(err) if Some(node_id) == open => self.error = Some(err.into()),
                Some(err) => self.status_line = format!("Drafter failed on {title}: {err}").into(),
                None if Some(node_id) == open => {
                    self.error = None;
                    self.status_line = "Drafter updated the obligations".into();
                }
                None => self.status_line = format!("Drafter updated {title}").into(),
            }
        }
        if let Some(status) = status {
            if status != self.status {
                self.status = status;
                changed = true;
            }
        }
        if self.reload() {
            changed = true;
        }
        changed
    }

    /// Re-read the open node; returns whether anything changed.
    fn reload(&mut self) -> bool {
        let Some(node_id) = self.node.as_ref().map(|n| n.id) else {
            return false;
        };
        let Ok(data) = self.fleet.read(|conn| {
            let repo = DraftingRepo::new(conn);
            let mut review: Vec<ReviewItem> = repo
                .list_choices(node_id, &[CHOICE_OPEN])?
                .into_iter()
                .map(ReviewItem::Choice)
                .collect();
            review.extend(repo.review_queue(node_id)?.into_iter().map(ReviewItem::Obligation));
            Ok(Snapshot {
                lifecycle: NodeRepo::new(conn).get_lifecycle(node_id)?.unwrap_or_default(),
                review,
                summaries: repo.recent_summaries(node_id, RECENT_SUMMARIES)?,
                dumps: repo.recent_dumps(node_id, RECENT_DUMPS)?,
                buildable: repo.buildable(node_id)?.map(|e| (e.outcome, e.detail)),
                pre_v3_node: repo.pre_v3_counts(node_id, false)?.iter().map(|(_, n)| n).sum(),
                pre_v3_subtree: repo.pre_v3_counts(node_id, true)?.iter().map(|(_, n)| n).sum(),
            })
        }) else {
            return false;
        };
        if data == self.data {
            return false;
        }
        if let Some(node) = self.node.as_mut() {
            if node.lifecycle != data.lifecycle {
                node.lifecycle = data.lifecycle.clone();
                if let Err(err) = self.ensure_driver(node_id) {
                    self.error = Some(err.into());
                }
            }
        }
        // Until the user picks a row, follow the top of the queue (it is
        // sorted by what most needs them). After that, keep their row, else
        // the one now at its place.
        let old_ix = self.selected_index();
        self.data = data;
        if !self.user_selected {
            self.selected = self.data.review.first().map(ReviewItem::key);
        } else if self.selected_index().is_none() {
            let ix = old_ix.unwrap_or(0).min(self.data.review.len().saturating_sub(1));
            self.selected = self.data.review.get(ix).map(ReviewItem::key);
        }
        if self
            .editing
            .is_some_and(|id| self.selected != Some(ReviewKey::Obligation(id)))
        {
            self.editing = None;
        }
        true
    }

    fn selected_index(&self) -> Option<usize> {
        let key = self.selected?;
        self.data.review.iter().position(|i| i.key() == key)
    }

    fn selected_item(&self) -> Option<&ReviewItem> {
        self.selected_index().map(|ix| &self.data.review[ix])
    }

    fn text_editing(&self) -> bool {
        self.dump_editing || self.editing.is_some() || self.picker.is_some()
    }

    fn obligations_focused(&self, window: &Window, cx: &App) -> bool {
        self.obligations.read(cx).focus_handle(cx).contains_focused(window, cx)
    }

    fn command(&mut self, command: InterviewCommand) -> Option<serde_json::Value> {
        match self.fleet.interview(ACTOR_USER, command) {
            Ok(value) => {
                self.error = None;
                Some(value)
            }
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                None
            }
        }
    }

    fn outline(&mut self, mutation: OutlineMutation) -> bool {
        self.command(InterviewCommand::Outline {
            mutation,
            target: None,
        })
        .is_some()
    }

    fn after_write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload();
        let obligations = self.obligations.clone();
        window.defer(cx, move |window, cx| {
            obligations.update(cx, |panel, cx| panel.reload(window, cx));
        });
        cx.notify();
    }

    // ----- review actions -------------------------------------------------

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.data.review.is_empty() {
            return;
        }
        let current = self.selected_index().unwrap_or(0) as isize;
        let ix = (current + delta).clamp(0, self.data.review.len() as isize - 1) as usize;
        self.selected = Some(self.data.review[ix].key());
        self.user_selected = true;
        self.editing = None;
        cx.notify();
    }

    fn select(&mut self, key: ReviewKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected != Some(key) {
            self.editing = None;
            self.picker = None;
        }
        self.selected = Some(key);
        self.user_selected = true;
        self.pane = Pane::Review;
        if !self.text_editing() {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    fn selected_obligation(&self) -> Option<MarkedObligation> {
        match self.selected_item()? {
            ReviewItem::Obligation(m) => Some(m.clone()),
            ReviewItem::Choice(_) => None,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.selected_obligation() else {
            return;
        };
        if self
            .command(InterviewCommand::ConfirmObligation {
                obligation_id: m.obligation.id,
            })
            .is_some()
        {
            self.status_line = "Confirmed".into();
        }
        self.after_write(window, cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.selected_obligation() else {
            return;
        };
        if self.outline(OutlineMutation::DeleteObligation {
            obligation_id: m.obligation.id,
        }) {
            self.status_line = "Deleted".into();
        }
        self.after_write(window, cx);
    }

    fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.selected_obligation() else {
            return;
        };
        self.picker = None;
        self.editing = Some(m.obligation.id);
        self.edit_input
            .update(cx, |input, cx| input.set_value(m.obligation.body.clone(), window, cx));
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.edit_input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(obligation_id) = self.editing else {
            return;
        };
        let body = self.edit_input.read(cx).value().trim().to_string();
        if body.is_empty() {
            self.error = Some("An obligation needs text; delete it instead".into());
            cx.notify();
            return;
        }
        if self.outline(OutlineMutation::UpdateObligationBody {
            obligation_id,
            body,
        }) {
            self.editing = None;
            self.status_line = "Saved".into();
            self.focus_handle.focus(window, cx);
        }
        self.after_write(window, cx);
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.selected_obligation() else {
            return;
        };
        let nodes = self
            .fleet
            .read(|conn| DraftingRepo::new(conn).node_picks())
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.id != m.obligation.node_id)
            .collect::<Vec<_>>();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Send to which node?"));
        let _subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.filter_picker(cx),
                InputEvent::PressEnter { .. } => this.apply_picker(window, cx),
                _ => {}
            });
        self.editing = None;
        self.picker = Some(Picker {
            obligation_id: m.obligation.id,
            input: input.clone(),
            matches: nodes.iter().take(PICKER_MATCHES).cloned().collect(),
            nodes,
            selected: 0,
            _subscription,
        });
        cx.notify();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn filter_picker(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let query = picker.input.read(cx).value().to_string();
        let mut scored: Vec<(i32, &NodePick)> = picker
            .nodes
            .iter()
            .filter_map(|n| {
                if query.trim().is_empty() {
                    return Some((0, n));
                }
                tod_core::fuzzy::fuzzy_score(&format!("{} {}", n.title, n.slug), &query)
                    .map(|s| (s, n))
            })
            .collect();
        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        picker.matches = scored
            .into_iter()
            .take(PICKER_MATCHES)
            .map(|(_, n)| n.clone())
            .collect();
        picker.selected = 0;
        cx.notify();
    }

    fn move_picker(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(picker) = self.picker.as_mut() {
            if !picker.matches.is_empty() {
                let max = picker.matches.len() as isize - 1;
                picker.selected = (picker.selected as isize + delta).clamp(0, max) as usize;
                cx.notify();
            }
        }
    }

    fn apply_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let Some(target) = picker.matches.get(picker.selected).cloned() else {
            return;
        };
        let obligation_id = picker.obligation_id;
        if self.outline(OutlineMutation::MoveObligation {
            obligation_id,
            target_node_id: target.id,
        }) {
            self.picker = None;
            self.status_line = format!("Sent to {}", target.title).into();
            self.focus_handle.focus(window, cx);
        }
        self.after_write(window, cx);
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Pick option `n` (1-based) of the selected choice; `None` is "You pick".
    fn answer(&mut self, option: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ReviewItem::Choice(choice)) = self.selected_item().cloned() else {
            return;
        };
        if option.is_some_and(|n| n < 1 || n as usize > choice.options.len()) {
            return;
        }
        if self
            .command(InterviewCommand::AnswerChoice {
                node_id: choice.node_id,
                seq: choice.seq,
                option,
            })
            .is_some()
        {
            self.status_line = match option {
                Some(n) => format!("Picked {n} for {}", choice.label()),
                None => format!("Left {} to the drafter", choice.label()),
            }
            .into();
        }
        self.after_write(window, cx);
    }

    fn on_digit(&mut self, n: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane == Pane::Review {
            self.answer(Some(n), window, cx);
        }
    }

    // ----- dump -----------------------------------------------------------

    fn enter_dump_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = Pane::Dump;
        self.dump_editing = true;
        self.editing = None;
        self.picker = None;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.dump_input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn exit_dump_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dump_editing = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn submit_dump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node.as_ref().map(|n| n.id) else {
            return;
        };
        let body = self.dump_input.read(cx).value().trim().to_string();
        if body.is_empty() {
            return;
        }
        if let Some(value) = self.command(InterviewCommand::AddDump {
            node_id: Some(node_id),
            body,
        }) {
            let id = value.get("id").and_then(|v| v.as_str()).unwrap_or("dump");
            self.status_line = format!("Sent {id} to the drafter").into();
            self.dump_input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.after_write(window, cx);
    }

    // ----- navigation -----------------------------------------------------

    fn focus_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = pane;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn focus_obligations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.obligations
            .update(cx, |panel, cx| panel.focus_handle(cx).focus(window, cx));
        cx.notify();
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.pane {
            Pane::Dump => self.enter_dump_edit(window, cx),
            Pane::Review => {
                if matches!(self.selected_item(), Some(ReviewItem::Obligation(_))) {
                    self.start_edit(window, cx);
                }
            }
        }
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.close_picker(window, cx);
        } else if self.editing.is_some() {
            self.cancel_edit(window, cx);
        } else if self.dump_editing {
            self.exit_dump_edit(window, cx);
        } else {
            cx.emit(DraftingViewEvent::ReturnToTaskList);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.save_edit(window, cx);
        } else if self.dump_editing {
            self.submit_dump(window, cx);
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        for driver in self.drivers.values() {
            if let Ok(mut driver) = driver.lock() {
                driver.retry();
            }
        }
        self.error = None;
        cx.notify();
    }

    fn proceed(&mut self, cx: &mut Context<Self>) {
        if let Some(ctx) = self.proceed.clone() {
            cx.emit(DraftingViewEvent::ProceedToLifecycle {
                task_id: ctx.task_id,
                lifecycle: ctx.lifecycle,
            });
        }
    }

    /// Wrap a nav-mode handler so it yields to text fields and the obligations panel.
    fn nav_guard(&self, window: &Window, cx: &App) -> bool {
        !self.text_editing() && !self.obligations_focused(window, cx)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl Focusable for DraftingView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl HasAppNav for DraftingView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        None
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

macro_rules! nav_action {
    ($el:expr, $cx:expr, $action:ty, |$this:ident, $window:ident, $c:ident| $body:expr) => {
        $el.on_action($cx.listener(|$this, _: &$action, $window, $c| {
            if !$this.nav_guard($window, $c) {
                $c.propagate();
                return;
            }
            $body;
            $c.stop_propagation();
        }))
    };
}

impl Render for DraftingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        set_input_tab_stop(&self.dump_input, self.dump_editing, cx);
        set_input_tab_stop(&self.edit_input, self.editing.is_some(), cx);
        // Deferred: render can run while the panel is itself mid-update.
        let obligations = self.obligations.clone();
        window.defer(cx, move |window, cx| {
            obligations.update(cx, |panel, cx| panel.reload(window, cx));
        });

        let theme = cx.theme();
        let (background, border, muted) = (theme.background, theme.border, theme.muted_foreground);

        let Some(node) = self.node.as_ref() else {
            return div()
                .key_context(CONTEXT)
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(background)
                .p_4()
                .text_color(muted)
                .child("Open a task in proposed or design to draft it.")
                .into_any_element();
        };
        let mode = node.mode();

        let root = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(background)
            .v_flex()
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .on_action(cx.listener(|this, _: &DraftSubmit, window, cx| this.submit(window, cx)))
            .on_action(cx.listener(|this, _: &DraftEscape, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|_, _: &DraftBack, _, cx| {
                cx.emit(DraftingViewEvent::ReturnToTaskList)
            }))
            .on_action(cx.listener(|this, _: &DraftPickerUp, _, cx| this.move_picker(-1, cx)))
            .on_action(cx.listener(|this, _: &DraftPickerDown, _, cx| this.move_picker(1, cx)))
            .on_action(cx.listener(|this, _: &PaneFocusLeft, window, cx| {
                if this.text_editing() || this.obligations_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                if this.pane == Pane::Dump {
                    this.focus_pane(Pane::Review, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PaneFocusRight, window, cx| {
                if this.text_editing() || this.obligations_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                match this.pane {
                    Pane::Review => this.focus_pane(Pane::Dump, window, cx),
                    Pane::Dump => this.focus_obligations(window, cx),
                }
            }));
        let root = nav_action!(root, cx, DraftUp, |this, window, cx| this.move_selection(-1, cx));
        let root = nav_action!(root, cx, DraftDown, |this, window, cx| this.move_selection(1, cx));
        let root = nav_action!(root, cx, DraftActivate, |this, window, cx| this.activate(window, cx));
        let root = nav_action!(root, cx, DraftConfirm, |this, window, cx| this.confirm(window, cx));
        let root = nav_action!(root, cx, DraftEdit, |this, window, cx| this.start_edit(window, cx));
        let root = nav_action!(root, cx, DraftDelete, |this, window, cx| this.delete(window, cx));
        let root = nav_action!(root, cx, DraftSendElsewhere, |this, window, cx| this
            .open_picker(window, cx));
        let root = nav_action!(root, cx, DraftYouPick, |this, window, cx| this
            .answer(None, window, cx));
        let root = nav_action!(root, cx, DraftFocusDump, |this, window, cx| this
            .enter_dump_edit(window, cx));
        let root = nav_action!(root, cx, DraftDigit1, |this, window, cx| this.on_digit(1, window, cx));
        let root = nav_action!(root, cx, DraftDigit2, |this, window, cx| this.on_digit(2, window, cx));
        let root = nav_action!(root, cx, DraftDigit3, |this, window, cx| this.on_digit(3, window, cx));
        let root = nav_action!(root, cx, DraftDigit4, |this, window, cx| this.on_digit(4, window, cx));
        let root = nav_action!(root, cx, DraftDigit5, |this, window, cx| this.on_digit(5, window, cx));
        let root = nav_action!(root, cx, DraftDigit6, |this, window, cx| this.on_digit(6, window, cx));
        let root = nav_action!(root, cx, DraftDigit7, |this, window, cx| this.on_digit(7, window, cx));
        let root = nav_action!(root, cx, DraftDigit8, |this, window, cx| this.on_digit(8, window, cx));
        let root = nav_action!(root, cx, DraftDigit9, |this, window, cx| this.on_digit(9, window, cx));

        let header = self.render_header(mode, border, muted, window, cx).into_any_element();
        let review = self.render_review(border, muted, window, cx).into_any_element();
        let dump = self.render_dump(mode, border, muted, window, cx).into_any_element();

        root.child(header)
            .when_some(self.error.clone(), |el, message| {
                el.child(
                    div()
                        .px_4()
                        .py_2()
                        .bg(gpui::red())
                        .border_b_1()
                        .border_color(border)
                        .child(
                            selectable_text("drafting-error", message, window, cx)
                                .text_sm()
                                .text_color(gpui::white()),
                        ),
                )
            })
            .child(
                div().flex_1().min_h_0().min_w_0().w_full().overflow_hidden().child(
                    h_resizable("drafting-columns")
                        .child(
                            resizable_panel()
                                .size(px(REVIEW_COLUMN_WIDTH))
                                .size_range(px(COLUMN_MIN)..Pixels::MAX)
                                .child(review),
                        )
                        .child(
                            resizable_panel()
                                .size(px(DUMP_COLUMN_WIDTH))
                                .size_range(px(COLUMN_MIN)..Pixels::MAX)
                                .child(dump),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(COLUMN_MIN)..Pixels::MAX)
                                .child(self.obligations.clone()),
                        ),
                ),
            )
            .into_any_element()
    }
}

impl DraftingView {
    fn render_header(
        &mut self,
        mode: DraftingMode,
        border: Hsla,
        muted: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let (danger, success, warning) = (theme.danger, theme.success, theme.warning);
        let title = self.node.as_ref().map(|n| n.title.clone()).unwrap_or_default();
        let agent = if self.status.running {
            "Drafter working…".to_string()
        } else if !self.status.summarizing.is_empty() {
            format!(
                "Summarizing {} for the drafter…",
                self.status.summarizing.join(", ")
            )
        } else if self.status.rewrite_pending {
            "Rewrite queued".to_string()
        } else if self.status.manual_required {
            "Drafter stopped after repeated failures".to_string()
        } else {
            self.status_line.to_string()
        };
        let buildable = (mode == DraftingMode::Drafting).then(|| match &self.data.buildable {
            Some((outcome, _)) if outcome == OUTCOME_PASS => ("Buildable", success),
            Some((outcome, _)) if outcome == OUTCOME_FAIL => ("Not buildable yet", danger),
            _ => ("Buildable: not judged", warning),
        });
        let buildable_detail = self
            .data
            .buildable
            .as_ref()
            .filter(|_| mode == DraftingMode::Drafting)
            .and_then(|(_, d)| d.clone());
        let show_retry = self.status.manual_required || self.error.is_some();
        let pre_node = self.data.pre_v3_node;
        let pre_subtree = self.data.pre_v3_subtree;

        v_flex()
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .child(self.render_app_nav(window, cx))
                    .child(
                        Button::new("drafting-back")
                            .label("Back")
                            .ghost()
                            .compact()
                            .tooltip("Back to tasks (Alt+Left)")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(DraftingViewEvent::ReturnToTaskList)
                            })),
                    )
                    .child(div().text_xs().text_color(muted).child(mode.label()))
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(title),
                    )
                    .when_some(buildable, |el, (label, color)| {
                        el.child(div().text_xs().text_color(color).child(label))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(320.))
                            .overflow_hidden()
                            .child(selectable_text("drafting-status", agent, window, cx).text_xs()),
                    )
                    .when(show_retry, |el| {
                        el.child(
                            Button::new("drafting-retry")
                                .label("Retry")
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                        )
                    })
                    .when(pre_node > 0, |el| {
                        el.child(
                            Button::new("drafting-rewrite")
                                .label(format!("Rewrite pre-v3 ({pre_node})"))
                                .compact()
                                .tooltip("Have the drafter rewrite this node's obligations written before drafting v3")
                                .on_click(cx.listener(|this, _, _, cx| this.request_rewrite(false, cx))),
                        )
                    })
                    .when(pre_subtree > pre_node, |el| {
                        el.child(
                            Button::new("drafting-rewrite-subtree")
                                .label(format!("Rewrite pre-v3 in subtree ({pre_subtree})"))
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| this.request_rewrite(true, cx))),
                        )
                    })
                    .when(self.proceed.is_some(), |el| {
                        el.child(
                            Button::new("drafting-proceed")
                                .label("Proceed")
                                .primary()
                                .compact()
                                .tooltip("Open the lifecycle panel to advance")
                                .on_click(cx.listener(|this, _, _, cx| this.proceed(cx))),
                        )
                    }),
            )
            .when_some(buildable_detail, |el, detail| {
                el.child(
                    div().px_4().pb_2().child(
                        selectable_text("drafting-buildable-detail", detail, window, cx)
                            .text_xs()
                            .text_color(muted),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_review(
        &mut self,
        border: Hsla,
        muted: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let focused = self.pane == Pane::Review;
        let items = self.data.review.clone();
        let mut list = v_flex().w_full();
        if items.is_empty() {
            let empty = if self.status.busy() {
                "The drafter is working…"
            } else {
                "Nothing needs your attention."
            };
            list = list.child(div().p_3().text_sm().text_color(muted).child(empty));
        }
        for (ix, item) in items.iter().enumerate() {
            let selected = self.selected == Some(item.key());
            list = list.child(self.render_review_item(ix, item, selected, border, muted, window, cx));
        }
        let heading = if focused { cx.theme().foreground } else { muted };
        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .text_xs()
                    .text_color(heading)
                    .child("Review")
                    .child(format!("({})", items.len())),
            )
            .child(
                div()
                    .text_xs()
                    .px_3()
                    .pb_1()
                    .text_color(muted)
                    .child("c confirm · e edit · m send elsewhere · d delete · 1-9 pick · y you pick"),
            )
            .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(list))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_review_item(
        &mut self,
        ix: usize,
        item: &ReviewItem,
        selected: bool,
        border: Hsla,
        muted: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let (primary, highlight) = (theme.primary, theme.muted);
        let (danger, warning) = (theme.danger, theme.warning);
        let key = item.key();
        let mut row = v_flex()
            .id(("drafting-review", ix))
            .relative()
            .w_full()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .when(selected, |el| {
                el.bg(highlight).child(
                    div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(primary),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.select(key, window, cx)),
            );
        match item {
            ReviewItem::Choice(choice) => {
                row = row
                    .child(
                        div()
                            .text_xs()
                            .text_color(warning)
                            .child(format!("{} · your call", choice.label())),
                    )
                    .child(
                        selectable_text(("drafting-choice-q", ix), choice.question.clone(), window, cx)
                            .text_sm(),
                    )
                    .when_some(choice.context.clone(), |el, context| {
                        el.child(
                            selectable_text(("drafting-choice-ctx", ix), context, window, cx)
                                .text_xs()
                                .text_color(muted),
                        )
                    });
                let mut options = v_flex().gap_1().pt_1();
                for (n, option) in choice.options.iter().enumerate() {
                    let pick = n as i64 + 1;
                    options = options.child(
                        Button::new(("drafting-choice-option", ix * 16 + n))
                            .label(format!("{pick}. {}", option.label))
                            .compact()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.selected = Some(key);
                                this.answer(Some(pick), window, cx);
                            })),
                    );
                }
                options = options.child(
                    Button::new(("drafting-choice-you-pick", ix))
                        .label("You pick (y)")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected = Some(key);
                            this.answer(None, window, cx);
                        })),
                );
                row = row.child(options);
            }
            ReviewItem::Obligation(m) => {
                let attention = m.mark.attention.as_deref().unwrap_or(ATTENTION_LOW);
                let color = match attention {
                    ATTENTION_HIGH => danger,
                    ATTENTION_MEDIUM => warning,
                    _ => muted,
                };
                let kind = match &m.obligation.section {
                    Some(section) => format!("{} · {section}", m.obligation.kind),
                    None => m.obligation.kind.clone(),
                };
                row = row.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(div().text_color(color).child(attention.to_string()))
                        .child(div().text_color(muted).child(kind)),
                );
                if self.editing == Some(m.obligation.id) {
                    row = row.child(Textarea::new(&self.edit_input).w_full()).child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(("drafting-save", ix))
                                    .label("Save")
                                    .primary()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| this.save_edit(window, cx))),
                            )
                            .child(
                                Button::new(("drafting-cancel", ix))
                                    .label("Cancel")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
                            ),
                    );
                } else {
                    row = row.child(
                        selectable_text(("drafting-body", ix), m.obligation.body.clone(), window, cx)
                            .text_sm(),
                    );
                }
                if let Some(why) = m.mark.attention_why.clone() {
                    row = row.child(
                        selectable_text(("drafting-why", ix), why, window, cx)
                            .text_xs()
                            .text_color(muted),
                    );
                }
                if selected && self.editing.is_none() {
                    row = row.child(
                        h_flex()
                            .gap_1()
                            .pt_1()
                            .child(
                                Button::new(("drafting-confirm", ix))
                                    .label("Confirm")
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                            )
                            .child(
                                Button::new(("drafting-edit", ix))
                                    .label("Edit")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| this.start_edit(window, cx))),
                            )
                            .child(
                                Button::new(("drafting-move", ix))
                                    .label("Send elsewhere")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
                            )
                            .child(
                                Button::new(("drafting-delete", ix))
                                    .label("Delete")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
                            ),
                    );
                }
                if let Some(picker) = self
                    .picker
                    .as_ref()
                    .filter(|p| p.obligation_id == m.obligation.id)
                {
                    let mut matches = v_flex().w_full();
                    for (n, pick) in picker.matches.iter().enumerate() {
                        let active = n == picker.selected;
                        matches = matches.child(
                            div()
                                .id(("drafting-pick", n))
                                .px_2()
                                .py_0p5()
                                .text_sm()
                                .cursor_pointer()
                                .when(active, |el| el.bg(primary.opacity(0.2)))
                                .child(format!("{}  [[{}]]", pick.title, pick.slug))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(p) = this.picker.as_mut() {
                                        p.selected = n;
                                    }
                                    this.apply_picker(window, cx);
                                })),
                        );
                    }
                    row = row.child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .pt_1()
                            .child(
                                div()
                                    .key_context(PICKER_TAG)
                                    .child(Input::new(&picker.input).w_full()),
                            )
                            .child(matches),
                    );
                }
            }
        }
        row.into_any_element()
    }

    fn render_dump(
        &mut self,
        mode: DraftingMode,
        border: Hsla,
        muted: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let (primary, foreground) = (theme.primary, theme.foreground);
        let focused = self.pane == Pane::Dump;
        let hint = match mode {
            DraftingMode::Capture => "What is this? Anything you know — goals, must-haves, worries.",
            DraftingMode::Drafting => "Anything that changes how this gets built. Questions welcome.",
        };
        let waiting: Vec<DraftingDump> = self
            .data
            .dumps
            .iter()
            .filter(|d| d.routed_at.is_none())
            .cloned()
            .collect();
        let summaries = self.data.summaries.clone();

        let mut changes = v_flex().w_full().gap_2();
        if summaries.is_empty() {
            changes = changes.child(div().text_xs().text_color(muted).child("No drafter turns yet."));
        }
        for (ix, summary) in summaries.iter().enumerate() {
            changes = changes.child(
                v_flex()
                    .w_full()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        selectable_markdown(("drafting-summary", ix), summary.body.clone(), window, cx)
                            .text_sm(),
                    ),
            );
        }

        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(if focused { foreground } else { muted })
                    .child("Dump"),
            )
            .child(div().px_3().pb_1().text_xs().text_color(muted).child(hint))
            .child(
                div()
                    .id("drafting-dump-field")
                    .mx_3()
                    .p_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(if focused && !self.dump_editing { primary } else { border })
                    .h(px(DUMP_HEIGHT))
                    .overflow_hidden()
                    .on_click(cx.listener(|this, _, window, cx| this.enter_dump_edit(window, cx)))
                    .child(
                        Textarea::new(&self.dump_input)
                            .disabled(!self.dump_editing)
                            .w_full()
                            .h(px(DUMP_HEIGHT - 4.)),
                    ),
            )
            .child(
                h_flex().px_3().py_2().gap_2().child(
                    Button::new("drafting-send-dump")
                        .label("Send (Ctrl+Enter)")
                        .primary()
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| this.submit_dump(window, cx))),
                ),
            )
            .when(!waiting.is_empty(), |el| {
                el.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_xs()
                        .text_color(muted)
                        .child(format!(
                            "{} dump{} waiting for the drafter: {}",
                            waiting.len(),
                            plural(waiting.len()),
                            waiting.iter().map(|d| d.label()).collect::<Vec<_>>().join(", ")
                        )),
                )
            })
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child("What changed"),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .overflow_y_scrollbar()
                    .child(changes),
            )
            .into_any_element()
    }
}
