//! The task panel's pull-request controls, one row per linked pull request,
//! under the ticket and PR links: **Labels**, **Open**, **Reviewer** and
//! **Update PR**.
//!
//! They show in the pull-request phases (`pr`, `pr_draft`, `pr_review`).
//! Nothing waits on GitHub: the lists the menus offer (the repository's
//! labels, the people a reviewer can be) are remembered on this machine and
//! only read again from a menu's last entry, "Refresh this list". A choice is
//! saved as it is made (`tod_core::pr_choices`) and shown again after a
//! restart; **Update PR** sends the ones GitHub does not have yet
//! (`tod_core::pr_actions::update`). Open toggles between open for review and
//! a draft. Opening a draft, and asking a reviewer of a draft, are refused at
//! Update PR until the code review is clean. Everything that talks to GitHub
//! runs off the UI thread.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::InteractiveElement as _;
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
use tod_core::pr_choices::Choice;
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

/// The last entry of each menu: not a choice, but a request to read the list again.
const REFRESH: &str = "\u{1}refresh";

#[derive(Clone)]
struct LabelChoice(RepoLabel);

impl SelectItem for LabelChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        if self.0.name == REFRESH { "Refresh this list".into() } else { self.0.name.clone().into() }
    }
    fn value(&self) -> &String {
        &self.0.name
    }
    fn matches(&self, query: &str) -> bool {
        self.0.name == REFRESH || tod_core::fuzzy::fuzzy_matches(query, &self.0.name)
    }
}

#[derive(Clone)]
struct ReviewerChoice(Reviewer);

impl ReviewerChoice {
    fn text(&self) -> String {
        if self.0.login == REFRESH {
            return "Refresh this list".to_string();
        }
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
        self.0.login == REFRESH || tod_core::fuzzy::fuzzy_matches(query, &self.text())
    }
}

type LabelPicker = Entity<ComboboxState<SearchableVec<LabelChoice>>>;
type ReviewerPicker = Entity<ComboboxState<SearchableVec<ReviewerChoice>>>;

fn label_items(labels: Vec<RepoLabel>) -> SearchableVec<LabelChoice> {
    let mut items: Vec<LabelChoice> = labels.into_iter().map(LabelChoice).collect();
    items.push(LabelChoice(RepoLabel { name: REFRESH.into(), color: String::new() }));
    SearchableVec::new(items)
}

fn reviewer_items(people: Vec<Reviewer>) -> SearchableVec<ReviewerChoice> {
    let mut items: Vec<ReviewerChoice> = people.into_iter().map(ReviewerChoice).collect();
    items.push(ReviewerChoice(Reviewer { login: REFRESH.into(), name: None, suggested: false }));
    SearchableVec::new(items)
}

/// Whether `choice` holds something GitHub does not have yet.
fn pending(choice: &Choice, facts: Option<&PrFacts>) -> bool {
    let Some(f) = facts else { return !choice.is_empty() };
    let labels = choice.labels.as_ref().is_some_and(|l| {
        let (mut want, mut have) = (l.clone(), f.labels.clone());
        want.sort();
        have.sort();
        want != have
    });
    let open = choice.open.is_some_and(|o| o != f.is_open());
    let reviewer = choice
        .reviewer
        .as_ref()
        .is_some_and(|r| !f.requested_reviewers.iter().any(|q| q.eq_ignore_ascii_case(r)));
    labels || open || reviewer
}

/// `choice` without the parts GitHub already has.
fn trimmed(choice: &Choice, facts: &PrFacts) -> Choice {
    let mut out = choice.clone();
    if !pending(&Choice { labels: choice.labels.clone(), ..Choice::default() }, Some(facts)) {
        out.labels = None;
    }
    if !pending(&Choice { open: choice.open, ..Choice::default() }, Some(facts)) {
        out.open = None;
    }
    if !pending(&Choice { reviewer: choice.reviewer.clone(), ..Choice::default() }, Some(facts)) {
        out.reviewer = None;
    }
    out
}

struct Row {
    pr: NodePr,
    facts: Option<PrFacts>,
    /// What the user chose and has not sent.
    choice: Choice,
    /// Why the last read or action failed.
    error: Option<String>,
    /// What is being read from GitHub now (nothing waits on it).
    busy: Option<&'static str>,
    /// An update is on its way to GitHub.
    sending: bool,
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

    /// Make a row (from what is remembered, then read GitHub) for every
    /// linked pull request not shown.
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
        let mut errors: Vec<String> = Vec::new();
        let labels_cached = tod_core::pr_choices::labels(&self.root, &pr)
            .map_err(|e| errors.push(format!("labels: {e}")))
            .ok()
            .flatten();
        let reviewers_cached = tod_core::pr_choices::reviewers(&self.root, &pr)
            .map_err(|e| errors.push(format!("reviewers: {e}")))
            .ok()
            .flatten();
        let choice = tod_core::pr_choices::choice(&self.root, &pr)
            .map_err(|e| errors.push(format!("saved choices: {e}")))
            .unwrap_or_default();
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        let (chosen_labels, chosen_reviewer) = (choice.labels.clone().unwrap_or_default(), choice.reviewer.clone());
        let labels: LabelPicker = cx.new(|cx| {
            let mut state = ComboboxState::new(label_items(labels_cached.unwrap_or_default()), vec![], window, cx)
                .multiple(true)
                .searchable(true);
            state.set_selected_values(&chosen_labels, window, cx);
            state
        });
        let reviewers: ReviewerPicker = cx.new(|cx| {
            let mut state =
                ComboboxState::new(reviewer_items(reviewers_cached.unwrap_or_default()), vec![], window, cx)
                    .searchable(true);
            state.set_selected_values(&chosen_reviewer.into_iter().collect::<Vec<_>>(), window, cx);
            state
        });
        let for_labels = pr.clone();
        let labels_sub = cx.subscribe_in(
            &labels,
            window,
            move |this: &mut Self, picker, event: &ComboboxEvent<SearchableVec<LabelChoice>>, window, cx| {
                let ComboboxEvent::Change(values) = event else { return };
                if values.iter().any(|v| v == REFRESH) {
                    // The last entry is a command, not a label: keep the
                    // real selection and read the list again.
                    let kept: Vec<String> = values.iter().filter(|v| *v != REFRESH).cloned().collect();
                    let picker = picker.clone();
                    window.defer(cx, move |window, cx| {
                        picker.update(cx, |state, cx| state.set_selected_values(&kept, window, cx));
                    });
                    this.refresh_lists(for_labels.clone(), true, cx);
                } else {
                    let values = values.clone();
                    this.set_choice(&for_labels, move |c| c.labels = Some(values), cx);
                }
            },
        );
        let for_reviewer = pr.clone();
        let reviewers_sub = cx.subscribe_in(
            &reviewers,
            window,
            move |this: &mut Self, picker, event: &ComboboxEvent<SearchableVec<ReviewerChoice>>, window, cx| {
                let ComboboxEvent::Change(values) = event else { return };
                let Some(login) = values.first().cloned() else { return };
                if login == REFRESH {
                    let back: Vec<String> = this
                        .rows
                        .iter()
                        .find(|r| r.pr.same_as(&for_reviewer))
                        .and_then(|r| {
                            r.choice
                                .reviewer
                                .clone()
                                .or_else(|| r.facts.as_ref().and_then(|f| f.requested_reviewers.first().cloned()))
                        })
                        .into_iter()
                        .collect();
                    let picker = picker.clone();
                    window.defer(cx, move |window, cx| {
                        picker.update(cx, |state, cx| state.set_selected_values(&back, window, cx));
                    });
                    this.refresh_lists(for_reviewer.clone(), false, cx);
                } else {
                    this.set_choice(&for_reviewer, move |c| c.reviewer = Some(login), cx);
                }
            },
        );
        Row {
            pr,
            facts: None,
            choice,
            error,
            busy: Some("Reading GitHub…"),
            sending: false,
            labels,
            reviewers,
            _subs: vec![labels_sub, reviewers_sub],
        }
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

    /// Change what the user chose for `pr`, and keep it.
    fn set_choice(&mut self, pr: &NodePr, change: impl FnOnce(&mut Choice), cx: &mut Context<Self>) {
        let root = self.root.clone();
        let Some(row) = self.row_mut(pr) else { return };
        change(&mut row.choice);
        row.error = tod_core::pr_choices::store_choice(&root, &row.pr, &row.choice).err();
        cx.notify();
    }

    /// Read `pr` from GitHub, and the lists its menus offer if none has been
    /// read before. Nothing is disabled meanwhile.
    fn load(&mut self, pr: NodePr, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(&pr) {
            row.busy = Some("Reading GitHub…");
        }
        let root = self.root.clone();
        let fleet = self.fleet.clone();
        let node = self.node_id;
        cx.spawn(async move |this, cx| {
            let read = {
                let pr = pr.clone();
                cx.background_executor()
                    .spawn(async move {
                        let facts = tod_core::pr_actions::load(&root, &fleet, node, &pr)?;
                        let labels = match tod_core::pr_choices::labels(&root, &pr)? {
                            Some(_) => None,
                            None => Some(tod_core::pr_actions::refresh_labels(&root, &pr)?),
                        };
                        let reviewers = match tod_core::pr_choices::reviewers(&root, &pr)? {
                            Some(_) => None,
                            None => Some(tod_core::pr_actions::refresh_reviewers(&root, &pr)?),
                        };
                        Ok::<_, String>((facts, labels, reviewers))
                    })
                    .await
            };
            this.update(cx, |this, cx| this.loaded(pr, read, cx)).ok();
        })
        .detach();
    }

    /// Read one menu's list from GitHub again, remember it, and show it.
    fn refresh_lists(&mut self, pr: NodePr, labels: bool, cx: &mut Context<Self>) {
        let root = self.root.clone();
        if let Some(row) = self.row_mut(&pr) {
            row.busy = Some(if labels { "Reading the labels…" } else { "Reading the reviewers…" });
            row.error = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let read = {
                let pr = pr.clone();
                cx.background_executor()
                    .spawn(async move {
                        if labels {
                            tod_core::pr_actions::refresh_labels(&root, &pr).map(|l| (Some(l), None))
                        } else {
                            tod_core::pr_actions::refresh_reviewers(&root, &pr).map(|r| (None, Some(r)))
                        }
                    })
                    .await
            };
            this.update(cx, |this, cx| {
                let Some(row) = this.row_mut(&pr) else { return };
                row.busy = None;
                match read {
                    Ok((labels, reviewers)) => {
                        let (lp, rp) = (row.labels.clone(), row.reviewers.clone());
                        if let Some(window) = cx.active_window() {
                            cx.defer(move |cx| {
                                window
                                    .update(cx, |_, window, cx| {
                                        if let Some(labels) = labels {
                                            lp.update(cx, |state, cx| {
                                                let now = state.selected_values();
                                                state.set_items(label_items(labels), window, cx);
                                                state.set_selected_values(&now, window, cx);
                                            });
                                        }
                                        if let Some(people) = reviewers {
                                            rp.update(cx, |state, cx| {
                                                let now = state.selected_values();
                                                state.set_items(reviewer_items(people), window, cx);
                                                state.set_selected_values(&now, window, cx);
                                            });
                                        }
                                    })
                                    .ok();
                            });
                        }
                    }
                    Err(err) => row.error = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn loaded(
        &mut self,
        pr: NodePr,
        read: Result<(PrFacts, Option<Vec<RepoLabel>>, Option<Vec<Reviewer>>), String>,
        cx: &mut Context<Self>,
    ) {
        let root = self.root.clone();
        let Some(row) = self.row_mut(&pr) else { return };
        row.busy = None;
        match read {
            Ok((facts, labels, reviewers)) => {
                // What GitHub now has is no longer a pending choice.
                row.choice = trimmed(&row.choice, &facts);
                row.error = tod_core::pr_choices::store_choice(&root, &row.pr, &row.choice).err();
                let (label_picker, reviewer_picker) = (row.labels.clone(), row.reviewers.clone());
                let selected = row.choice.labels.clone().unwrap_or_else(|| facts.labels.clone());
                let requested: Vec<String> = row
                    .choice
                    .reviewer
                    .clone()
                    .or_else(|| facts.requested_reviewers.first().cloned())
                    .into_iter()
                    .collect();
                row.facts = Some(facts);
                // Items and selection need a window.
                if let Some(window) = cx.active_window() {
                    cx.defer(move |cx| {
                        window
                            .update(cx, |_, window, cx| {
                                label_picker.update(cx, |state, cx| {
                                    if let Some(labels) = labels {
                                        state.set_items(label_items(labels), window, cx);
                                    }
                                    state.set_selected_values(&selected, window, cx);
                                });
                                reviewer_picker.update(cx, |state, cx| {
                                    if let Some(people) = reviewers {
                                        state.set_items(reviewer_items(people), window, cx);
                                    }
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
        let action = |id: &str, label: &str| PresentedAction {
            id: id.to_string(),
            label: label.to_string(),
            primary: false,
            disabled: false,
        };
        let presented = Presented {
            actions: vec![
                action("pr_labels", "Labels"),
                action("pr_open", "Open"),
                action("pr_reviewer", "Reviewer"),
                action("pr_update", "Update PR"),
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

    /// Switch between open for review and a draft (sent with Update PR).
    fn toggle_open(&mut self, pr: NodePr, cx: &mut Context<Self>) {
        self.record("pr_open", cx);
        let current = self
            .rows
            .iter()
            .find(|r| r.pr.same_as(&pr))
            .map(|r| r.choice.open.unwrap_or_else(|| r.facts.as_ref().is_some_and(|f| f.is_open())))
            .unwrap_or(false);
        self.set_choice(&pr, move |c| c.open = Some(!current), cx);
    }

    /// Send the choices to GitHub off the UI thread, then read the pull
    /// request again; a failure is said on its row.
    fn update_pr(&mut self, pr: NodePr, cx: &mut Context<Self>) {
        self.record("pr_update", cx);
        let fleet = self.fleet.clone();
        let node = self.node_id;
        let root = self.root.clone();
        let Some(row) = self.row_mut(&pr) else { return };
        row.sending = true;
        row.error = None;
        let choice = row.choice.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = {
                let pr = pr.clone();
                cx.background_executor()
                    .spawn(async move { tod_core::pr_actions::update(&root, &fleet, node, &pr, &choice) })
                    .await
            };
            this.update(cx, |this, cx| {
                if let Some(row) = this.row_mut(&pr) {
                    row.sending = false;
                    match result {
                        Ok(()) => row.choice = Choice::default(),
                        Err(err) => row.error = Some(err),
                    }
                }
                tod_core::pr_readiness::forget_snapshot(&pr);
                // A failed update keeps its error: the read below only
                // replaces it when it fails too.
                let kept = this.row_mut(&pr).and_then(|r| r.error.clone());
                this.load(pr.clone(), cx);
                if let Some(row) = this.row_mut(&pr) {
                    row.error = kept;
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_row(&self, ix: usize, row: &Row, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let facts = row.facts.as_ref();
        let merged = facts.is_some_and(|f| f.merged);
        let want_open = row.choice.open.unwrap_or_else(|| facts.is_some_and(|f| f.is_open()));
        let unsent = pending(&row.choice, facts);
        let pr = row.pr.clone();
        let pr_update = row.pr.clone();
        let open_label = if want_open { "Open ✓" } else { "Open" };
        let reason = facts
            .filter(|f| !f.merged && !f.review_clean)
            .map(|f| format!("Not ready to open or send to a reviewer. Waiting on:\n{}", f.not_clean_because));
        let (ready_label, ready_tip) = match facts {
            None => ("Checking review…".to_string(), "Reading the pull request and its review from GitHub.".to_string()),
            Some(f) if f.merged => ("Merged".to_string(), "This pull request is merged.".to_string()),
            Some(f) if f.review_clean => (
                "✓ Ready to open and assign".to_string(),
                "The code review is clean: all feedback is answered and resolved, and nothing is left but a person's review.".to_string(),
            ),
            Some(f) => (format!("⏳ {}", f.waiting_on), reason.clone().unwrap_or_default()),
        };
        let open_tip = match (&reason, want_open) {
            (Some(r), _) if !facts.is_some_and(|f| f.is_open()) => r.clone(),
            (_, true) => "Open for review. Click to switch back to a draft (sent with Update PR).".to_string(),
            _ => "A draft. Click to open it for review (sent with Update PR).".to_string(),
        };
        let status = row
            .error
            .clone()
            .map(|e| (e, true))
            .or_else(|| row.sending.then(|| ("Updating GitHub…".to_string(), false)))
            .or_else(|| unsent.then(|| ("Not sent to GitHub yet".to_string(), false)))
            .or_else(|| row.busy.map(|b| (b.to_string(), false)))
            .or_else(|| {
                facts.filter(|f| !f.requested_reviewers.is_empty()).map(|f| {
                    (format!("Review requested from {}", f.requested_reviewers.join(", ")), false)
                })
            });
        crate::ui::key_context::forward_menu_arrows(div().id(("pr-controls-row", ix)))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(style::text_muted(div()).child(format!("{}#{}", row.pr.repo, row.pr.pr_number)))
            .child(
                div().w(gpui::px(240.)).child(
                    Combobox::new(&row.labels)
                        .placeholder("Labels…")
                        .search_placeholder("Type to search labels")
                        .disabled(merged),
                ),
            )
            .child(
                Button::new(("pr-open", ix))
                    .small()
                    .label(open_label)
                    .disabled(merged)
                    .tooltip(open_tip)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_open(pr.clone(), cx))),
            )
            .child(
                div().w(gpui::px(240.)).child(
                    Combobox::new(&row.reviewers)
                        .placeholder("Reviewer…")
                        .search_placeholder("Type to search people")
                        .disabled(merged),
                ),
            )
            .child({
                let button = Button::new(("pr-update", ix))
                    .small()
                    .label("Update PR")
                    .disabled(!unsent || row.sending || merged)
                    .on_click(cx.listener(move |this, _, _, cx| this.update_pr(pr_update.clone(), cx)));
                if unsent { button.primary() } else { button }
            })
            .child(
                Button::new(("pr-ready", ix)).small().ghost().label(ready_label).tooltip(ready_tip),
            )
            .child(
                Button::new(("pr-refresh", ix))
                    .small()
                    .ghost()
                    .icon(IconName::Redo2)
                    .tooltip("Check GitHub again")
                    .disabled(row.busy.is_some())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(draft: bool, labels: &[&str], requested: &[&str]) -> PrFacts {
        PrFacts {
            pr: NodePr::new("a", "b", 1),
            draft,
            merged: false,
            review_clean: false,
            not_clean_because: String::new(),
            waiting_on: String::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            requested_reviewers: requested.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_choice_is_pending_only_where_it_differs_from_github() {
        let f = facts(true, &["bug", "docs"], &["ann"]);
        assert!(!pending(&Choice::default(), Some(&f)));
        assert!(!pending(&Choice { labels: Some(vec!["docs".into(), "bug".into()]), ..Choice::default() }, Some(&f)));
        assert!(pending(&Choice { labels: Some(vec!["bug".into()]), ..Choice::default() }, Some(&f)));
        assert!(pending(&Choice { open: Some(true), ..Choice::default() }, Some(&f)));
        assert!(!pending(&Choice { open: Some(false), ..Choice::default() }, Some(&f)));
        assert!(!pending(&Choice { reviewer: Some("Ann".into()), ..Choice::default() }, Some(&f)));
        assert!(pending(&Choice { reviewer: Some("bo".into()), ..Choice::default() }, Some(&f)));
        // Before GitHub has been read, any choice is unsent.
        assert!(pending(&Choice { open: Some(false), ..Choice::default() }, None));
        let all = Choice { labels: Some(vec!["bug".into(), "docs".into()]), open: Some(true), reviewer: Some("ann".into()) };
        assert_eq!(trimmed(&all, &f), Choice { open: Some(true), ..Choice::default() });
    }
    use crate::unified::requests::{DecisionsLinkNext, REQUESTS_CONTEXT};
    use gpui::{TestAppContext, px};
    use std::cell::Cell;
    use std::rc::Rc;

    /// Hosts the labels picker the way the task panel does: under the
    /// requests' key context, whose Up/Down bindings must not take the keys
    /// of the open menu.
    struct Host {
        labels: LabelPicker,
        stolen: Rc<Cell<usize>>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let stolen = self.stolen.clone();
            crate::ui::key_context::forward_menu_arrows(div().id("host"))
                .key_context(REQUESTS_CONTEXT)
                .on_action(move |_: &DecisionsLinkNext, _, _| stolen.set(stolen.get() + 1))
                .size_full()
                .child(div().w(px(240.)).child(Combobox::new(&self.labels).placeholder("Labels…")))
        }
    }

    #[gpui::test]
    fn down_in_the_open_labels_menu_moves_through_the_choices(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::app::register_main_keyboard_bindings(cx);
        });
        let stolen = Rc::new(Cell::new(0));
        let mut picker = None;
        let stolen_in = stolen.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let labels: LabelPicker = cx.new(|cx| {
                ComboboxState::new(
                    SearchableVec::new(
                        ["bug", "docs", "feature"]
                            .map(|n| LabelChoice(RepoLabel { name: n.into(), color: String::new() }))
                            .to_vec(),
                    ),
                    vec![],
                    window,
                    cx,
                )
                .multiple(true)
                .searchable(true)
            });
            picker = Some(labels.clone());
            gpui_component::Root::new(cx.new(|_| Host { labels, stolen: stolen_in }), window, cx)
        });
        let picker = picker.unwrap();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_click(gpui::point(px(40.), px(15.)), gpui::Modifiers::none());
        cx.run_until_parked();
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        let chosen = cx.update(|_, cx| picker.read(cx).selected_values());
        assert_eq!(stolen.get(), 0, "the panel's Down took the menu's key");
        assert_eq!(chosen, vec!["docs".to_string()], "two Downs then Enter should choose the second label");
    }
}
