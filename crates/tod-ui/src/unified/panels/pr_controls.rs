//! The task panel's pull-request buttons, one row per linked pull request,
//! under the ticket and PR links: **Labels**, **Open**, **Reviewer**.
//!
//! They show in the pull-request phases (`pr`, `pr_draft`, `pr_review`).
//! Labels are the repository's own, picked with the standard searchable
//! multi-select. Open (draft → ready for review) waits for the code review to pass
//! clean (`tod_core::pr_actions`), and so does Reviewer while the pull request
//! is a draft; they say why while they wait. Reviewer lists GitHub's own suggestions first. Everything talks to
//! GitHub, so it runs off the UI thread.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};
use gpui_component::button::Button;
use gpui_component::combobox::{Combobox, ComboboxEvent, ComboboxState};
use gpui_component::select::{SearchableVec, SelectItem};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use gpui_component::button::ButtonVariants as _;
use tod_core::pr_actions::PrFacts;
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use tod_store::github::{NodePr, RepoLabel, Reviewer};
use uuid::Uuid;

use crate::ui::selectable_text::selectable_text;
use crate::ui::style;

/// The lifecycle states whose pull request these buttons act on.
pub fn is_pr_phase(lifecycle: &str) -> bool {
    matches!(lifecycle, "pr" | "pr_draft" | "pr_review")
}

#[derive(Clone)]
struct LabelChoice(RepoLabel);

impl SelectItem for LabelChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.0.name.clone().into()
    }
    fn value(&self) -> &String {
        &self.0.name
    }
    fn matches(&self, query: &str) -> bool {
        tod_core::fuzzy::fuzzy_matches(query, &self.0.name)
    }
}

#[derive(Clone)]
struct ReviewerChoice(Reviewer);

impl ReviewerChoice {
    fn text(&self) -> String {
        let mut text = match &self.0.name {
            Some(name) => format!("{} ({name})", self.0.login),
            None => self.0.login.clone(),
        };
        if self.0.suggested {
            text.push_str(" · suggested");
        }
        text
    }
}

impl SelectItem for ReviewerChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.text().into()
    }
    fn value(&self) -> &String {
        &self.0.login
    }
    fn matches(&self, query: &str) -> bool {
        tod_core::fuzzy::fuzzy_matches(query, &self.text())
    }
}

type LabelPicker = Entity<ComboboxState<SearchableVec<LabelChoice>>>;
type ReviewerPicker = Entity<ComboboxState<SearchableVec<ReviewerChoice>>>;

struct Row {
    pr: NodePr,
    facts: Option<PrFacts>,
    /// Why the last read or action failed.
    error: Option<String>,
    /// What is running against GitHub now.
    busy: Option<&'static str>,
    labels: LabelPicker,
    reviewers: ReviewerPicker,
    _subs: Vec<Subscription>,
}

pub struct PrControls {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    root: PathBuf,
    active: bool,
    rows: Vec<Row>,
    /// The links `rows` were made for.
    linked: Vec<NodePr>,
}

impl PrControls {
    pub fn new(node_id: Uuid, fleet: Arc<FleetStore>) -> Self {
        let root = fleet.paths().root().to_path_buf();
        Self { node_id, fleet, root, active: false, rows: Vec::new(), linked: Vec::new() }
    }

    pub fn set_node(&mut self, node_id: Uuid, cx: &mut Context<Self>) {
        if node_id != self.node_id {
            self.node_id = node_id;
            self.rows.clear();
            self.linked.clear();
            cx.notify();
        }
    }

    /// Whether the node is in a pull-request phase; the buttons show only
    /// then.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }

    /// Make a row (and read GitHub) for every linked pull request not shown.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let node = self.node_id;
        let linked = self
            .fleet
            .read(|conn| Ok(tod_store::github::NodePrRepo::new(conn).read(node)?))
            .map(|links| links.prs)
            .unwrap_or_default();
        if linked == self.linked {
            return;
        }
        self.linked = linked.clone();
        self.rows = linked.iter().map(|pr| self.new_row(pr.clone(), window, cx)).collect();
        for pr in linked {
            self.load(pr, cx);
        }
    }

    fn new_row(&self, pr: NodePr, window: &mut Window, cx: &mut Context<Self>) -> Row {
        let labels: LabelPicker = cx.new(|cx| {
            ComboboxState::new(SearchableVec::new(Vec::<LabelChoice>::new()), vec![], window, cx)
                .multiple(true)
                .searchable(true)
        });
        let reviewers: ReviewerPicker = cx.new(|cx| {
            ComboboxState::new(SearchableVec::new(Vec::<ReviewerChoice>::new()), vec![], window, cx)
                .searchable(true)
        });
        let for_labels = pr.clone();
        let labels_sub = cx.subscribe(
            &labels,
            move |this: &mut Self, _, event: &ComboboxEvent<SearchableVec<LabelChoice>>, cx| {
                // Each toggle is applied as it is made; the closed menu's
                // confirm carries the same set.
                if let ComboboxEvent::Change(values) = event {
                    this.apply_labels(for_labels.clone(), values.clone(), cx);
                }
            },
        );
        let for_reviewer = pr.clone();
        let reviewers_sub = cx.subscribe(
            &reviewers,
            move |this: &mut Self, _, event: &ComboboxEvent<SearchableVec<ReviewerChoice>>, cx| {
                if let ComboboxEvent::Change(values) = event {
                    if let Some(login) = values.first() {
                        this.apply_reviewer(for_reviewer.clone(), login.clone(), cx);
                    }
                }
            },
        );
        Row { pr, facts: None, error: None, busy: Some("Reading GitHub…"), labels, reviewers, _subs: vec![labels_sub, reviewers_sub] }
    }

    /// When the node's pull request agent last answered; comments before it
    /// are not feedback still to do.
    fn answered_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        tod_core::pr_readiness::agent_answered_at(&self.fleet, self.node_id).unwrap_or(None)
    }

    /// Read every linked pull request from GitHub again (the cache is dropped).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        for pr in self.linked.clone() {
            tod_core::pr_readiness::forget_snapshot(&pr);
            self.load(pr, cx);
        }
    }

    fn row_mut(&mut self, pr: &NodePr) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.pr.same_as(pr))
    }

    /// Read `pr`, its repository's labels, and (once its review is clean) the
    /// reviewers it can be sent to.
    fn load(&mut self, pr: NodePr, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(&pr) {
            row.busy = Some("Reading GitHub…");
        }
        let root = self.root.clone();
        let answered_at = self.answered_at();
        cx.spawn(async move |this, cx| {
            let read = {
                let pr = pr.clone();
                cx.background_executor()
                    .spawn(async move {
                        let facts = tod_core::pr_actions::load(&root, &pr, answered_at)?;
                        let labels = tod_core::pr_actions::repo_labels(&root, &pr)?;
                        let reviewers = if facts.review_clean && !facts.merged {
                            tod_core::pr_actions::reviewer_choices(&root, &pr)?
                        } else {
                            Vec::new()
                        };
                        Ok::<_, String>((facts, labels, reviewers))
                    })
                    .await
            };
            this.update(cx, |this, cx| this.loaded(pr, read, cx)).ok();
        })
        .detach();
    }

    fn loaded(
        &mut self,
        pr: NodePr,
        read: Result<(PrFacts, Vec<RepoLabel>, Vec<Reviewer>), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row_mut(&pr) else { return };
        row.busy = None;
        match read {
            Ok((facts, labels, reviewers)) => {
                row.error = None;
                let (label_picker, reviewer_picker) = (row.labels.clone(), row.reviewers.clone());
                let (selected, requested) = (facts.labels.clone(), facts.requested_reviewers.clone());
                row.facts = Some(facts);
                // Items and selection need a window.
                if let Some(window) = cx.active_window() {
                    cx.defer(move |cx| {
                        window
                            .update(cx, |_, window, cx| {
                                label_picker.update(cx, |state, cx| {
                                    state.set_items(
                                        SearchableVec::new(labels.into_iter().map(LabelChoice).collect::<Vec<_>>()),
                                        window,
                                        cx,
                                    );
                                    state.set_selected_values(&selected, window, cx);
                                });
                                reviewer_picker.update(cx, |state, cx| {
                                    state.set_items(
                                        SearchableVec::new(reviewers.into_iter().map(ReviewerChoice).collect::<Vec<_>>()),
                                        window,
                                        cx,
                                    );
                                    state.set_selected_values(&requested, window, cx);
                                });
                            })
                            .ok();
                    });
                }
            }
            Err(err) => row.error = Some(err),
        }
        cx.notify();
    }

    fn record(&self, id: &str, cx: &mut Context<Self>) {
        let action = |id: &str, label: &str, disabled: bool| PresentedAction {
            id: id.to_string(),
            label: label.to_string(),
            primary: false,
            disabled,
        };
        let clean = self.rows.iter().all(|r| r.facts.as_ref().is_some_and(|f| f.review_clean));
        let reviewer_ready = self.rows.iter().all(|r| r.facts.as_ref().is_some_and(|f| !f.draft || f.review_clean));
        let presented = Presented {
            actions: vec![
                action("pr_labels", "Labels", false),
                action("pr_open", "Open", !clean),
                action("pr_reviewer", "Reviewer", !reviewer_ready),
            ],
            focused: None,
            notices: Vec::new(),
        };
        crate::ui::journey::record_action(
            cx,
            Focus::Node(self.node_id),
            id.to_string(),
            crate::ui::journey::Source::Click,
            "task_panel",
            presented,
        );
    }

    fn apply_labels(&mut self, pr: NodePr, labels: Vec<String>, cx: &mut Context<Self>) {
        self.record("pr_labels", cx);
        self.run(pr, "Setting labels…", cx, move |root, pr| tod_core::pr_actions::set_labels(root, pr, &labels));
    }

    fn open(&mut self, pr: NodePr, cx: &mut Context<Self>) {
        self.record("pr_open", cx);
        let answered_at = self.answered_at();
        self.run(pr, "Opening for review…", cx, move |root, pr| {
            tod_core::pr_actions::open_for_review(root, pr, answered_at)
        });
    }

    fn apply_reviewer(&mut self, pr: NodePr, login: String, cx: &mut Context<Self>) {
        self.record("pr_reviewer", cx);
        let answered_at = self.answered_at();
        self.run(pr, "Asking for review…", cx, move |root, pr| {
            tod_core::pr_actions::request_reviewer(root, pr, &login, answered_at)
        });
    }

    /// Run `job` against GitHub off the UI thread, then read the pull request
    /// again; a failure is said on its row.
    fn run(
        &mut self,
        pr: NodePr,
        what: &'static str,
        cx: &mut Context<Self>,
        job: impl FnOnce(&std::path::Path, &NodePr) -> Result<(), String> + Send + 'static,
    ) {
        if let Some(row) = self.row_mut(&pr) {
            row.busy = Some(what);
            row.error = None;
        }
        cx.notify();
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let result = {
                let pr = pr.clone();
                cx.background_executor().spawn(async move { job(&root, &pr) }).await
            };
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    if let Some(row) = this.row_mut(&pr) {
                        row.error = Some(err);
                    }
                }
                this.load(pr, cx);
            })
            .ok();
        })
        .detach();
    }

    fn render_row(&self, ix: usize, row: &Row, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let facts = row.facts.as_ref();
        let clean = facts.is_some_and(|f| f.review_clean);
        let merged = facts.is_some_and(|f| f.merged);
        let draft = facts.is_some_and(|f| f.draft);
        let idle = row.busy.is_none();
        let wait_reason = match facts {
            Some(f) if f.merged => "This pull request is merged.".to_string(),
            Some(f) if f.draft && !f.review_clean => {
                format!("Waiting for the code review to pass clean:\n{}", f.not_clean_because)
            }
            _ => String::new(),
        };
        let pr = row.pr.clone();
        let open_label = match facts {
            Some(f) if f.is_open() => "Open ✓",
            _ => "Open",
        };
        let status = row
            .error
            .clone()
            .map(|e| (e, true))
            .or_else(|| row.busy.map(|b| (b.to_string(), false)))
            .or_else(|| {
                facts.filter(|f| !f.requested_reviewers.is_empty()).map(|f| {
                    (format!("Review requested from {}", f.requested_reviewers.join(", ")), false)
                })
            });
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(style::text_muted(div()).child(format!("{}#{}", row.pr.repo, row.pr.pr_number)))
            .child(
                div()
                    .w(gpui::px(240.))
                    .child(
                        Combobox::new(&row.labels)
                            .placeholder("Labels…")
                            .search_placeholder("Type to search labels")
                            .disabled(!idle || facts.is_none() || merged),
                    ),
            )
            .child(
                Button::new(("pr-open", ix))
                    .small()
                    .label(open_label)
                    .disabled(!idle || !clean || !draft)
                    .when_some((!wait_reason.is_empty()).then(|| wait_reason.clone()), |b, reason| {
                        b.tooltip(reason)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.open(pr.clone(), cx))),
            )
            .child(
                div()
                    .w(gpui::px(240.))
                    .child(
                        Combobox::new(&row.reviewers)
                            .placeholder("Reviewer…")
                            .search_placeholder("Type to search people")
                            .disabled(!idle || (draft && !clean) || merged),
                    ),
            )
            .child(
                Button::new(("pr-refresh", ix))
                    .small()
                    .ghost()
                    .icon(IconName::Redo2)
                    .tooltip("Check GitHub again")
                    .disabled(!idle)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .children(status.map(|(text, is_error)| {
                let el = if is_error { style::text_error(div()) } else { style::text_muted(div()) };
                el.child(selectable_text(("pr-controls-status", ix), text, window, cx))
            }))
            .into_any_element()
    }
}

impl Render for PrControls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.active {
            return div().into_any_element();
        }
        self.sync(window, cx);
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(ix, row)| self.render_row(ix, row, window, cx))
            .collect::<Vec<_>>();
        div().flex().flex_col().gap_1().children(rows).into_any_element()
    }
}
