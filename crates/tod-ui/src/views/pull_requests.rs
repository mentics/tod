//! The Pull requests view (the app menu's "Pull requests"): the pull requests
//! of the node selected in Tasks, one group per repository its work spans — the superproject its Files capability
//! names, then each submodule, which is its own GitHub repository with its
//! own pull requests.
//!
//! What a node's pull requests *are* is `tod_core::pull_requests`; this view
//! only shows them. The list — the cursor, the keys, the groups, the columns,
//! the row menu — is [`crate::ui::item_list`]. A pull request here cannot be
//! edited, created, reordered or marked: it lives on GitHub, and this panel
//! is a window onto it. What it affords is opening it there.
//!
//! Loading runs git and calls GitHub, so it happens off the UI thread: when
//! the view is shown, and on Refresh — never on a timer. The last result
//! stays on screen while the next one loads.

use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::item_list::keyboard::{
    ItemListCollapse, ItemListDown, ItemListEnd, ItemListExpand, ItemListHome, ItemListPageDown,
    ItemListPageUp, ItemListUp,
};
use crate::ui::item_list::{
    CollapseStep, GroupSpec, ItemList, ItemListEvent, ItemListKeys, ItemListRow, ItemRowState,
    bind_item_list_keys,
};
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_text;
use crate::ui::status_filter::{StatusFilter, render_status_filter, status_counts};
use crate::ui::style;
use crate::views::rows::pull_request_row::{pull_request_columns, pull_request_row};
use crate::views::rows::{RowAction, RowHost};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, Styled, Window, actions,
    div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use gpui_kit_assets::IconName;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tod_core::pull_requests::{
    NodePulls, PullScope, RepoPulls, RepoSection, load_node_pulls, read_target,
};
use tod_store::fleet::FleetStore;
use tod_store::github::{PullState, PullSummary};
use uuid::Uuid;

const PULL_REQUESTS_CONTEXT: &str = "PullRequests";

/// How long to wait before loading, so switching views back and forth asks
/// GitHub once, about where the user stops.
const SETTLE: Duration = Duration::from_millis(250);

actions!(
    pull_requests,
    [
        PullRequestsRefresh,
        PullRequestsOpen,
        PullRequestsToggleScope
    ]
);

pub fn register_pull_requests_keyboard_bindings(cx: &mut App) {
    // A pull request lives on GitHub: navigation, and the two things this
    // view does with one — open it, and ask again. Left/Right collapse and
    // expand the repositories.
    bind_item_list_keys(cx, PULL_REQUESTS_CONTEXT, ItemListKeys::default());
    let context = Some(key_context::excluding_input(PULL_REQUESTS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", PullRequestsOpen, context),
        KeyBinding::new("r", PullRequestsRefresh, context),
        KeyBinding::new("a", PullRequestsToggleScope, context),
    ]);
}

/// One row under a repository's heading: a pull request, or what there is to
/// say about a repository that has none to list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PullRow {
    Pull(PullSummary),
    Note { text: String, error: bool },
}

/// What the user did in the list, queued for the view to apply.
#[derive(Debug, Clone)]
enum PullAction {
    Select { row_ix: usize },
    ToggleGroup { key: String },
    Ignored,
}

impl From<ItemListEvent> for PullAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            ItemListEvent::ToggleGroup { key } => Self::ToggleGroup { key },
            ItemListEvent::ToggleMark { .. } | ItemListEvent::Drop(_) => Self::Ignored,
        }
    }
}

/// What a pull request affords: opening it on GitHub, and its link. Declared
/// once, so it is both the button on the row and its right-click menu.
fn pull_actions(row: &PullRow) -> Vec<RowAction> {
    let PullRow::Pull(pull) = row else {
        return Vec::new();
    };
    let open = pull.url.clone();
    let copy = pull.url.clone();
    vec![
        RowAction::new("open", "Open", move |_, cx| cx.open_url(&open))
            .icon(IconName::ExternalLink)
            .tooltip("Open this pull request on GitHub"),
        RowAction::new("copy-link", "Copy link", move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
        })
        .menu_only(),
    ]
}

fn row_text(row: &PullRow) -> String {
    match row {
        PullRow::Pull(pull) => format!("#{} {} {}", pull.number, pull.title, pull.url),
        PullRow::Note { text, .. } => text.clone(),
    }
}

/// A repository's heading: the submodule's path, then the GitHub repository.
fn section_label(section: &RepoSection) -> String {
    match (&section.github, section.path.is_empty()) {
        (Some(repo), true) => repo.to_string(),
        (Some(repo), false) => format!("{} · {repo}", section.path),
        (None, true) => "This repository".to_string(),
        (None, false) => section.path.clone(),
    }
}

/// What to say under a repository with no pull request to list, or `None`
/// when its rows say it.
fn section_note(section: &RepoSection, scope: PullScope, branch: Option<&str>) -> Option<PullRow> {
    let (text, error) = match &section.pulls {
        RepoPulls::Listed(pulls) if !pulls.is_empty() => return None,
        RepoPulls::Listed(_) => (
            match (scope, branch) {
                (PullScope::AllOpen, _) => "No open pull request".to_string(),
                (PullScope::Branch, Some(branch)) => format!("No pull request from {branch}"),
                (PullScope::Branch, None) => "No pull request".to_string(),
            },
            false,
        ),
        RepoPulls::NotGithub { remote: Some(url) } => (format!("Not on GitHub: {url}"), false),
        RepoPulls::NotGithub { remote: None } => {
            ("No remote to open pull requests on".into(), false)
        }
        RepoPulls::NoBranch => ("Not on a branch".into(), false),
        RepoPulls::Failed(err) => (err.clone(), true),
    };
    Some(PullRow::Note { text, error })
}

/// The rows: a heading per repository, then its pull requests that pass
/// `filter` — or, when it has none at all, what there is to say about it. A
/// collapsed repository contributes its heading alone.
fn build_rows(
    pulls: &NodePulls,
    filter: &StatusFilter,
    is_collapsed: impl Fn(&str) -> bool,
) -> Vec<ItemListRow<PullRow>> {
    let mut rows = Vec::new();
    for section in &pulls.sections {
        let key = section.key();
        let visible: Vec<&PullSummary> = match &section.pulls {
            RepoPulls::Listed(list) => list
                .iter()
                .filter(|p| filter.admits(p.state.as_str()))
                .collect(),
            _ => Vec::new(),
        };
        let collapsed = is_collapsed(&key);
        rows.push(ItemListRow::heading(
            GroupSpec::new(key.clone(), 0, section_label(section))
                .count(visible.len())
                .collapsed(collapsed),
        ));
        if collapsed {
            continue;
        }
        rows.extend(visible.into_iter().map(|pull| {
            ItemListRow::item(
                format!("{key}#{}", pull.number),
                PullRow::Pull(pull.clone()),
            )
        }));
        if let Some(note) = section_note(section, pulls.scope, pulls.branch.as_deref()) {
            rows.push(ItemListRow::item(format!("{key}:note"), note));
        }
    }
    rows
}

fn all_pulls(pulls: &NodePulls) -> impl Iterator<Item = &PullSummary> {
    pulls.sections.iter().flat_map(|s| match &s.pulls {
        RepoPulls::Listed(list) => list.as_slice(),
        _ => &[],
    })
}

const STATE_ORDER: [&str; 4] = [
    PullState::Open.as_str(),
    PullState::Draft.as_str(),
    PullState::Merged.as_str(),
    PullState::Closed.as_str(),
];

enum Load {
    /// Nothing asked yet.
    Idle,
    Loaded(NodePulls),
    /// Why there is nothing to show (no Files, no token, …).
    Failed(String),
}

pub struct PullRequestsView {
    fleet: Arc<FleetStore>,
    data_root: PathBuf,
    focus_handle: FocusHandle,
    node_id: Option<Uuid>,
    title: String,
    load: Load,
    loading: bool,
    /// Bumped on every load and on close: a result for an older one is
    /// dropped when it lands.
    generation: u64,
    filter: StatusFilter,
    /// The node's branch, or every open one.
    scope: PullScope,
    list: ItemList<PullRow>,
    host: RowHost<PullAction>,
    app_nav: AppNavMenu,
}

impl PullRequestsView {
    pub fn new(cx: &mut Context<Self>, fleet: Arc<FleetStore>, data_root: PathBuf) -> Self {
        Self {
            fleet,
            data_root,
            focus_handle: cx.focus_handle(),
            node_id: None,
            title: String::new(),
            load: Load::Idle,
            loading: false,
            generation: 0,
            filter: StatusFilter::default(),
            scope: PullScope::default(),
            list: ItemList::new()
                .with_columns(pull_request_columns())
                .with_row_actions(pull_actions)
                .with_row_text(row_text),
            host: RowHost::for_entity(cx.weak_entity()),
            app_nav: AppNavMenu::default(),
        }
    }

    /// Show the pull requests of `node` (its id and title), or say to select
    /// one when there is none. The same node is asked about again: the user
    /// may have pushed since.
    pub fn show(&mut self, node: Option<(Uuid, String)>, cx: &mut Context<Self>) {
        let Some((node_id, title)) = node else {
            self.node_id = None;
            self.title.clear();
            self.load = Load::Idle;
            self.loading = false;
            self.generation += 1;
            self.rebuild_rows();
            cx.notify();
            return;
        };
        self.title = title;
        if self.node_id != Some(node_id) {
            self.node_id = Some(node_id);
            self.load = Load::Idle;
            self.list.set_cursor_key(None);
            self.list.expand_all();
            self.rebuild_rows();
        }
        self.reload(cx);
    }

    /// Ask again, off the UI thread, once the selection has settled.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        let fleet = self.fleet.clone();
        let data_root = self.data_root.clone();
        let scope = self.scope;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let current = this
                .read_with(cx, |this, _| this.generation == generation)
                .unwrap_or(false);
            if !current {
                return;
            }
            let result = cx
                .background_spawn(async move {
                    let target = fleet
                        .read(|conn| read_target(conn, node_id))
                        .map_err(|err| format!("{err:#}"))?;
                    load_node_pulls(target, scope, &data_root)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                this.load = match result {
                    Ok(pulls) => Load::Loaded(pulls),
                    Err(reason) => Load::Failed(reason),
                };
                this.rebuild_rows();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let rows = match &self.load {
            Load::Loaded(pulls) => {
                build_rows(pulls, &self.filter, |key| self.list.is_collapsed(key))
            }
            Load::Idle | Load::Failed(_) => Vec::new(),
        };
        self.list.set_rows(rows);
    }

    fn set_filter(&mut self, state: Option<&str>, cx: &mut Context<Self>) {
        match state {
            Some(state) => self.filter.toggle(state),
            None => {
                self.filter.clear();
            }
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn drain_row_actions(&mut self, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                PullAction::Select { row_ix } => {
                    if self.list.set_cursor(row_ix) {
                        cx.notify();
                    }
                }
                PullAction::ToggleGroup { key } => {
                    self.list.toggle_collapsed(&key);
                    self.rebuild_rows();
                    cx.notify();
                }
                PullAction::Ignored => {}
            }
        }
    }

    fn on_refresh(&mut self, _: &PullRequestsRefresh, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    fn set_scope(&mut self, scope: PullScope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.list.set_cursor_key(None);
        self.reload(cx);
        cx.notify();
    }

    fn on_toggle_scope(
        &mut self,
        _: &PullRequestsToggleScope,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = match self.scope {
            PullScope::Branch => PullScope::AllOpen,
            PullScope::AllOpen => PullScope::Branch,
        };
        self.set_scope(next, cx);
    }

    /// Enter: open the pull request under the cursor on GitHub; on a heading,
    /// collapse or expand it.
    fn on_open(&mut self, _: &PullRequestsOpen, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(PullRow::Pull(pull)) = self.list.cursor_item() {
            cx.open_url(&pull.url);
            return;
        }
        if let Some(key) = self
            .list
            .cursor_row()
            .and_then(|row| row.as_group())
            .map(|g| g.key.clone())
        {
            self.list.toggle_collapsed(&key);
            self.rebuild_rows();
            cx.notify();
        }
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
        let page = ItemList::<PullRow>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(-page, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<PullRow>::page_rows(window.viewport_size().height) as i32;
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

    /// A note under a repository: why it has no pull request to list.
    fn render_note(
        text: &str,
        error: bool,
        state: ItemRowState<'_>,
        host: &RowHost<PullAction>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let row_ix = state.row_ix;
        let select_host = host.clone();
        let body = selectable_text(
            format!("pull-note-{}", state.key),
            text.to_string(),
            window,
            cx,
        );
        style::row(h_flex())
            .w_full()
            .items_center()
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                select_host.push(PullAction::Select { row_ix }, cx);
            })
            .when(state.highlighted, style::highlighted)
            .child(state.column(
                crate::views::rows::pull_request_row::COLUMN_TITLE,
                if error {
                    style::text_error(div()).child(body)
                } else {
                    style::text_muted(div()).child(body)
                },
            ))
            .into_any_element()
    }

    fn render_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let branch = match &self.load {
            Load::Loaded(pulls) => pulls.branch.clone(),
            _ => None,
        };
        let subtitle = match branch {
            Some(branch) => format!("{} · {branch}", self.title),
            None => self.title.clone(),
        };
        let muted = cx.theme().muted_foreground;
        let app_nav = self
            .render_app_nav_without_badge(window, cx)
            .into_any_element();
        style::panel_header(h_flex())
            .flex_shrink_0()
            .w_full()
            .min_w_0()
            .items_center()
            .gap(style::space::RELATED)
            .bg(cx.theme().secondary)
            .child(app_nav)
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(div().text_sm().font_semibold().child("Pull requests"))
                    .child(
                        selectable_text("pull-requests-title", subtitle, window, cx)
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .child(
                Button::new("pull-requests-scope-branch")
                    .label("This branch")
                    .ghost()
                    .compact()
                    .selected(self.scope == PullScope::Branch)
                    .on_click(cx.listener(|this, _, _, cx| this.set_scope(PullScope::Branch, cx))),
            )
            .child(
                Button::new("pull-requests-scope-open")
                    .label("All open")
                    .ghost()
                    .compact()
                    .selected(self.scope == PullScope::AllOpen)
                    .on_click(cx.listener(|this, _, _, cx| this.set_scope(PullScope::AllOpen, cx))),
            )
            .child(
                Button::new("pull-requests-refresh")
                    .label(if self.loading {
                        "Loading…"
                    } else {
                        "Refresh"
                    })
                    .ghost()
                    .compact()
                    .disabled(self.loading || self.node_id.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: String| {
            style::empty_message(div())
                .p(style::space::INSET)
                .child(text)
                .into_any_element()
        };
        if self.node_id.is_none() {
            return message("Select a node in Tasks to see the pull requests of its work".into());
        }
        match &self.load {
            Load::Idle => message("Loading pull requests…".into()),
            Load::Failed(reason) => style::text_error(div())
                .p(style::space::INSET)
                .child(selectable_text(
                    "pull-requests-error",
                    reason.clone(),
                    window,
                    cx,
                ))
                .into_any_element(),
            Load::Loaded(pulls) if pulls.sections.is_empty() => {
                message("No repositories to look in".into())
            }
            Load::Loaded(_) => {
                let host = self.host.clone();
                self.list.render(
                    "pull-requests-list",
                    &self.host,
                    move |row, state, window, cx| match row {
                        PullRow::Pull(pull) => pull_request_row(pull, state, &host, window, cx),
                        PullRow::Note { text, error } => {
                            Self::render_note(text, *error, state, &host, window, cx)
                        }
                    },
                    window,
                    cx,
                )
            }
        }
    }
}

impl Focusable for PullRequestsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PullRequestsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_row_actions(cx);
        let background = cx.theme().background;
        let counts = match &self.load {
            Load::Loaded(pulls) => {
                status_counts(&STATE_ORDER, all_pulls(pulls).map(|p| p.state.as_str()))
            }
            _ => Vec::new(),
        };
        let warnings = match &self.load {
            Load::Loaded(pulls) => pulls.warnings.join("\n"),
            _ => String::new(),
        };
        let root = v_flex()
            .key_context(PULL_REQUESTS_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(background)
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_toggle_scope))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_collapse))
            .on_action(cx.listener(Self::on_expand))
            .child(self.render_header(window, cx))
            .children(render_status_filter(
                "pull-requests",
                &counts,
                &self.filter,
                |this: &mut Self, state, _, cx| this.set_filter(state, cx),
                cx,
            ))
            .child(v_flex().flex_1().min_h_0().child(self.render_body(window, cx)))
            .when(!warnings.is_empty(), |el| {
                el.child(
                    style::panel_footer(div()).flex_shrink_0().child(
                        style::text_dense_muted(div()).child(selectable_text(
                            "pull-requests-warnings",
                            warnings,
                            window,
                            cx,
                        )),
                    ),
                )
            })
            .child(
                style::panel_footer(div())
                    .flex_shrink_0()
                    .child(style::text_dense_muted(div()).child(
                        "↑/↓ navigate · Enter opens on GitHub · ←/→ collapse/expand · A this branch / all open · R refreshes",
                    )),
            );
        self.bind_app_nav_toggle(root, cx)
    }
}

impl HasAppNav for PullRequestsView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::PullRequests)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::github::GithubRepo;

    fn pull(number: i64, state: PullState) -> PullSummary {
        PullSummary {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/app/pull/{number}"),
            state,
            author: None,
            head: "tod/x".into(),
            base: "main".into(),
            updated_at: String::new(),
        }
    }

    fn section(path: &str, repo: Option<(&str, &str)>, pulls: RepoPulls) -> RepoSection {
        RepoSection {
            path: path.into(),
            github: repo.map(|(owner, name)| GithubRepo {
                owner: owner.into(),
                repo: name.into(),
            }),
            pulls,
        }
    }

    fn node_pulls() -> NodePulls {
        NodePulls {
            scope: PullScope::Branch,
            branch: Some("tod/x".into()),
            sections: vec![
                section(
                    "",
                    Some(("acme", "app")),
                    RepoPulls::Listed(vec![pull(1, PullState::Open), pull(2, PullState::Merged)]),
                ),
                section(
                    "vendor/lib",
                    Some(("acme", "lib")),
                    RepoPulls::Listed(Vec::new()),
                ),
                section(
                    "vendor/gl",
                    None,
                    RepoPulls::NotGithub {
                        remote: Some("https://gitlab.com/x/gl.git".into()),
                    },
                ),
                section(
                    "vendor/err",
                    Some(("acme", "err")),
                    RepoPulls::Failed("GitHub: boom".into()),
                ),
            ],
            warnings: Vec::new(),
        }
    }

    fn describe(rows: &[ItemListRow<PullRow>]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ItemListRow::Group { spec, .. } => {
                    format!("## {} ({})", spec.label, spec.count.unwrap_or(0))
                }
                ItemListRow::Item {
                    item: PullRow::Pull(p),
                    ..
                } => {
                    format!("#{} {}", p.number, p.state.as_str())
                }
                ItemListRow::Item {
                    item: PullRow::Note { text, error },
                    ..
                } => format!("{}{text}", if *error { "! " } else { "- " }),
            })
            .collect()
    }

    #[test]
    fn a_heading_per_repository_with_its_pull_requests_or_why_it_has_none() {
        let rows = build_rows(&node_pulls(), &StatusFilter::default(), |_| false);
        assert_eq!(
            describe(&rows),
            vec![
                "## acme/app (2)",
                "#1 open",
                "#2 merged",
                "## vendor/lib · acme/lib (0)",
                "- No pull request from tod/x",
                "## vendor/gl (0)",
                "- Not on GitHub: https://gitlab.com/x/gl.git",
                "## vendor/err · acme/err (0)",
                "! GitHub: boom",
            ]
        );
    }

    #[test]
    fn the_filter_narrows_the_pull_requests_but_not_the_repositories() {
        let mut filter = StatusFilter::default();
        filter.toggle("open");
        let rows = build_rows(&node_pulls(), &filter, |_| false);
        let described = describe(&rows);
        assert_eq!(&described[..2], &["## acme/app (1)", "#1 open"]);
        assert!(!described.iter().any(|r| r == "#2 merged"));
        // A repository whose pull requests are all filtered out shows no
        // note: it has pull requests, just none in this state.
        let pulls = NodePulls {
            scope: PullScope::Branch,
            branch: Some("b".into()),
            sections: vec![section(
                "",
                Some(("acme", "app")),
                RepoPulls::Listed(vec![pull(2, PullState::Merged)]),
            )],
            warnings: Vec::new(),
        };
        assert_eq!(
            describe(&build_rows(&pulls, &filter, |_| false)),
            vec!["## acme/app (0)"]
        );
    }

    #[test]
    fn listing_every_open_one_an_empty_repository_says_so_not_the_branch() {
        let mut pulls = node_pulls();
        pulls.scope = PullScope::AllOpen;
        let rows = build_rows(&pulls, &StatusFilter::default(), |_| false);
        let described = describe(&rows);
        assert!(
            described.contains(&"- No open pull request".to_string()),
            "{described:?}"
        );
    }

    #[test]
    fn a_collapsed_repository_is_its_heading_alone() {
        let rows = build_rows(&node_pulls(), &StatusFilter::default(), |key| {
            key == "repo:acme/app"
        });
        let described = describe(&rows);
        assert_eq!(
            &described[..2],
            &["## acme/app (2)", "## vendor/lib · acme/lib (0)"]
        );
    }

    #[test]
    fn keys_are_stable_per_repository_and_number() {
        let rows = build_rows(&node_pulls(), &StatusFilter::default(), |_| false);
        let keys: Vec<&str> = rows.iter().take(3).map(|r| r.key()).collect();
        assert_eq!(
            keys,
            vec!["repo:acme/app", "repo:acme/app#1", "repo:acme/app#2"]
        );
    }

    #[test]
    fn a_pull_request_affords_opening_it_and_its_link_a_note_nothing() {
        let actions = pull_actions(&PullRow::Pull(pull(1, PullState::Open)));
        let labels: Vec<&str> = actions.iter().map(|a| a.label.as_ref()).collect();
        assert_eq!(labels, vec!["Open", "Copy link"]);
        assert!(!actions[0].menu_only);
        assert!(actions[1].menu_only);
        let note = PullRow::Note {
            text: "x".into(),
            error: false,
        };
        assert!(pull_actions(&note).is_empty());
    }
}
