//! Confirming a change that removes worktrees or sandboxes.
//!
//! Each node that works from a Files capability gets a worktree or sandbox
//! of its own, made from that capability's settings
//! (`tod_store::fleet::provision`). A change that would leave them made
//! from settings that no longer apply — where the files are, worktrees on or
//! off, Files turned on further down, or removing one outright — first shows
//! every one it affects, and removes them before the change is made.
//! Removing one pushes its branch first, so nothing committed is lost.
//!
//! Uncommitted work blocks that. Each row with some offers Commit (a work in
//! progress commit), Retry (check again), Shell (open one there; the dialog
//! stays open so the user can settle the files and come back), and Discard.
//! Confirm is offered once every row is clean.
//!
//! Every check and removal runs off the UI thread: they run git, and in a
//! sandbox, the network.

use crate::interview::TodPaths;
use crate::interview::settings::TodSettings;
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use tod_store::fleet::provision::{
    commit_location_changes, discard_location_changes, location_state, remove_location,
};
use std::sync::Arc;
use tod_store::fleet::{AffectedLocation, FleetStore, LocationState, open_shell_in_location};

/// What to do once every affected location is gone.
pub type OnConfirmed = Box<dyn FnOnce(&mut Window, &mut App)>;

/// Run `on_confirmed` now when `affected` is empty; otherwise show what it
/// would remove and run it only once they are removed.
pub fn confirm_files_change(
    window: &mut Window,
    cx: &mut App,
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    affected: Vec<AffectedLocation>,
    title: impl Into<SharedString>,
    intro: impl Into<String>,
    confirm_label: impl Into<SharedString>,
    on_confirmed: OnConfirmed,
) {
    if affected.is_empty() {
        on_confirmed(window, cx);
        return;
    }
    let intro = intro.into();
    let confirm_label = confirm_label.into();
    let dialog = cx.new(|cx| {
        let mut dialog = FilesImpact {
            fleet,
            paths,
            intro,
            confirm_label,
            rows: affected
                .into_iter()
                .map(|affected| Row {
                    affected,
                    state: RowState::Checking,
                })
                .collect(),
            removing: None,
            error: None,
            on_confirmed: Some(on_confirmed),
        };
        for index in 0..dialog.rows.len() {
            dialog.check(index, cx);
        }
        dialog
    });
    let title = title.into();
    window.open_dialog(cx, move |d, _, _| {
        d.title(title.clone())
            .w(px(680.))
            .overlay(true)
            .overlay_closable(false)
            .keyboard(true)
            .close_button(true)
            .child(dialog.clone())
    });
}

/// What a migrate ended with: `Err` is a message to show the user.
pub type OnMigrated = Box<dyn FnOnce(Result<(), String>, &mut Window, &mut App)>;

fn migrate_error(err: anyhow::Error) -> String {
    format!("{err:#}")
}

/// Migrate `node` to `place`: keep its sessions and runner state off the UI
/// thread, show the impact dialog for the worktree or sandbox it leaves
/// (removing it, each branch pushed first, on Confirm), then enable Files
/// on it if needed and pick the location, and finally drop its cloud row if
/// it left a sandbox. A node with children is refused with the store's own
/// message, through `on_done`. Nothing is started.
pub fn confirm_migrate(
    window: &mut Window,
    cx: &mut App,
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    node: String,
    place: tod_store::fleet::FilesPlace,
    on_done: OnMigrated,
) {
    let task = window.spawn(cx, async move |cx| {
        let prepared = {
            let (fleet, node, place_id) = (fleet.clone(), node.clone(), place.id.clone());
            cx.background_executor()
                .spawn(async move {
                    let affected = tod_store::fleet::places::migrate_impact(&fleet, &node, &place_id)?;
                    tod_store::fleet::session_log::keep_node_state(&fleet, &node)?;
                    anyhow::Ok(affected)
                })
                .await
        };
        let _ = cx.update(|window, cx| match prepared {
            Err(err) => on_done(Err(migrate_error(err)), window, cx),
            Ok(affected) => {
                // The node's Files location is the one record of its sandbox,
                // so the impact names it whenever there is one.
                let left_sandbox = affected.iter().any(|a| a.location.sandbox().is_some());
                let intro = format!(
                    "{} puts this node's files somewhere else. Its current files are removed \
                     first (each branch is pushed before it goes); its agent sessions are kept \
                     and resume in the new place.",
                    place.name
                );
                let (fleet_for_run, node_for_run, place_id) = (fleet.clone(), node.clone(), place.id.clone());
                confirm_files_change(
                    window,
                    cx,
                    fleet,
                    paths,
                    affected,
                    "Migrate this node?",
                    intro,
                    "Remove and migrate",
                    Box::new(move |window, cx| {
                        let task = window.spawn(cx, async move |cx| {
                            let result = cx
                                .background_executor()
                                .spawn(async move {
                                    tod_store::fleet::places::migrate(&fleet_for_run, &node_for_run, &place_id)?;
                                    if left_sandbox {
                                        // The impact dialog removed the location's
                                        // sandbox and the row's copy of it; the
                                        // orchestrator is told.
                                        tod_core::cloud_sync::lost::announce_left_cloud(
                                            &fleet_for_run,
                                            &node_for_run,
                                        )?;
                                    }
                                    anyhow::Ok(())
                                })
                                .await;
                            let _ = cx.update(|window, cx| on_done(result.map_err(migrate_error), window, cx));
                        });
                        task.detach();
                    }),
                );
            }
        });
    });
    task.detach();
}

struct FilesImpact {
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    intro: String,
    confirm_label: SharedString,
    rows: Vec<Row>,
    /// What removing is doing, while it runs.
    removing: Option<String>,
    error: Option<String>,
    on_confirmed: Option<OnConfirmed>,
}

struct Row {
    affected: AffectedLocation,
    state: RowState,
}

enum RowState {
    Checking,
    Known(LocationState),
    /// A row action running: what it is doing.
    Working(&'static str),
    Failed(String),
}

impl Row {
    fn ready(&self) -> bool {
        matches!(
            self.state,
            RowState::Known(LocationState::Clean | LocationState::Gone)
        )
    }
}

impl FilesImpact {
    /// Run `job` on row `index`'s node off the UI thread, then check the row
    /// again (or show why the job failed).
    fn row_job(
        &mut self,
        index: usize,
        doing: &'static str,
        job: impl FnOnce(&FleetStore, &TodPaths, &str) -> anyhow::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        row.state = RowState::Working(doing);
        let node = row.affected.node_id.clone();
        self.error = None;
        cx.notify();
        let fleet = self.fleet.clone();
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    job(&fleet, &paths, &node)?;
                    location_state(&fleet, &paths, &node)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(row) = this.rows.get_mut(index) {
                    row.state = match result {
                        Ok(state) => RowState::Known(state),
                        Err(err) => RowState::Failed(format!("{err:#}")),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn check(&mut self, index: usize, cx: &mut Context<Self>) {
        self.row_job(index, "Checking…", |_, _, _| Ok(()), cx);
    }

    fn commit(&mut self, index: usize, cx: &mut Context<Self>) {
        self.row_job(
            index,
            "Committing…",
            |fleet, _, node| commit_location_changes(fleet, node),
            cx,
        );
    }

    fn discard(&mut self, index: usize, cx: &mut Context<Self>) {
        self.row_job(
            index,
            "Discarding…",
            |fleet, _, node| discard_location_changes(fleet, node),
            cx,
        );
    }

    /// Open a shell there. The dialog stays: the user settles the files,
    /// closes the shell, and comes back to Retry.
    fn shell(&mut self, index: usize, cx: &mut Context<Self>) {
        self.row_job(
            index,
            "Opening a shell…",
            |fleet, paths, node| {
                let settings = TodSettings::load(paths).unwrap_or_default();
                open_shell_in_location(fleet, paths, &settings, node).map(|_| ())
            },
            cx,
        );
    }

    /// Remove every location (pushing each branch first), then make the
    /// change. The first that fails stops it, and the rows are checked again.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.removing.is_some() || !self.rows.iter().all(Row::ready) {
            return;
        }
        self.removing = Some("Removing…".into());
        self.error = None;
        cx.notify();
        let fleet = self.fleet.clone();
        let paths = self.paths.clone();
        let nodes: Vec<(String, String)> = self
            .rows
            .iter()
            .map(|row| (row.affected.node_id.clone(), row.affected.node_title.clone()))
            .collect();
        let (tx, rx) = async_channel::unbounded::<String>();
        let task = cx.background_spawn(async move {
            let settings = TodSettings::load(&paths).unwrap_or_default();
            for (node, title) in nodes {
                let _ = tx.try_send(format!("{title}: pushing its branch…"));
                remove_location(&fleet, &paths, &settings, &node, &mut |step| {
                    let _ = tx.try_send(format!("{title}: {step}"));
                })
                .map_err(|err| err.context(format!("Could not remove {title}'s files")))?;
            }
            anyhow::Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(step) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.removing = Some(step);
                    cx.notify();
                });
            }
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.removing = None;
                match result {
                    Ok(()) => {
                        window.close_dialog(cx);
                        if let Some(on_confirmed) = this.on_confirmed.take() {
                            on_confirmed(window, cx);
                        }
                    }
                    Err(err) => {
                        this.error = Some(format!("{err:#}"));
                        for index in 0..this.rows.len() {
                            this.check(index, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_row(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row = &self.rows[index];
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let busy = self.removing.is_some() || matches!(row.state, RowState::Working(_) | RowState::Checking);
        let (status, color, dirty) = match &row.state {
            RowState::Checking => ("Checking…".to_string(), muted, None),
            RowState::Working(doing) => (doing.to_string(), muted, None),
            RowState::Failed(err) => (err.clone(), danger, None),
            RowState::Known(LocationState::Clean) => (
                "Nothing uncommitted: its branch is pushed, then it is removed.".into(),
                muted,
                None,
            ),
            RowState::Known(LocationState::Gone) => {
                ("Already gone: only the record is removed.".into(), muted, None)
            }
            RowState::Known(LocationState::Busy(reason)) => (
                format!("{reason} Close it, then Retry."),
                danger,
                None,
            ),
            RowState::Known(LocationState::Dirty(lines)) => (
                format!("{} uncommitted change(s):", lines.len()),
                danger,
                Some(lines.join("\n")),
            ),
        };
        let offers_fix = matches!(
            row.state,
            RowState::Known(LocationState::Dirty(_) | LocationState::Busy(_)) | RowState::Failed(_)
        );
        let can_change = matches!(row.state, RowState::Known(LocationState::Dirty(_)));
        v_flex()
            .gap_1()
            .py_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().flex_none().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(
                        selectable_text(
                            SharedString::from(format!("files-impact-node-{index}")),
                            row.affected.node_title.clone(),
                            window,
                            cx,
                        ),
                    ))
                    .child(div().flex_1().min_w_0().text_xs().text_color(muted).child(selectable_text(
                        SharedString::from(format!("files-impact-where-{index}")),
                        row.affected.location.describe(),
                        window,
                        cx,
                    ))),
            )
            .child(div().text_xs().text_color(color).child(selectable_text(
                SharedString::from(format!("files-impact-status-{index}")),
                status,
                window,
                cx,
            )))
            .when_some(dirty, |el, lines| {
                el.child(div().text_xs().font_family("monospace").text_color(muted).child(
                    selectable_text(
                        SharedString::from(format!("files-impact-dirty-{index}")),
                        lines,
                        window,
                        cx,
                    ),
                ))
            })
            .when(offers_fix, |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(("files-impact-commit", index))
                                .label("Commit")
                                .small()
                                .outline()
                                .disabled(busy || !can_change)
                                .on_click(cx.listener(move |this, _, _, cx| this.commit(index, cx))),
                        )
                        .child(
                            Button::new(("files-impact-retry", index))
                                .label("Retry")
                                .small()
                                .outline()
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.check(index, cx))),
                        )
                        .child(
                            Button::new(("files-impact-shell", index))
                                .label("Shell")
                                .small()
                                .outline()
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.shell(index, cx))),
                        )
                        .child(
                            Button::new(("files-impact-discard", index))
                                .label("Discard")
                                .small()
                                .danger()
                                .disabled(busy || !can_change)
                                .on_click(cx.listener(move |this, _, _, cx| this.discard(index, cx))),
                        ),
                )
            })
    }
}

impl Render for FilesImpact {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let ready = self.rows.iter().all(Row::ready);
        let removing = self.removing.clone();
        let mut rows = v_flex().gap_2();
        for index in 0..self.rows.len() {
            rows = rows.child(self.render_row(index, window, cx));
        }
        v_flex()
            .gap_3()
            .child(div().text_sm().child(selectable_text(
                "files-impact-intro",
                self.intro.clone(),
                window,
                cx,
            )))
            .child(rows)
            .when_some(self.error.clone(), |el, err| {
                el.child(div().text_xs().text_color(danger).child(selectable_text(
                    "files-impact-error",
                    err,
                    window,
                    cx,
                )))
            })
            .when_some(removing.clone(), |el, step| {
                el.child(div().text_xs().text_color(muted).child(selectable_text(
                    "files-impact-progress",
                    step,
                    window,
                    cx,
                )))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("files-impact-cancel")
                            .label("Cancel")
                            .disabled(removing.is_some())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("files-impact-confirm")
                            .label(self.confirm_label.clone())
                            .primary()
                            .disabled(!ready || removing.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}
