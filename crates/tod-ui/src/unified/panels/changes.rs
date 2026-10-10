//! The changes column panel (`PanelKind::Changes`): the files changed on a
//! node's branch against its base, with lines added and removed. Clicking a
//! file opens it in the code editor.
//!
//! [`ChangesWatch`] is the background job the task panel's **Changes** link
//! shares with this panel: git runs through `tod_store::fleet::Workdir` on
//! the background executor, never on the UI thread. It is recomputed when
//! the panel opens, when a turn on the node ends, and when the node's Files
//! settings or directory change (a store change moved its
//! [`ChangesTrigger`]), not on a timer. The count once was only redone on
//! ended turns, so a node whose Files directory was set (or became ready)
//! after the first count kept showing nothing. While a
//! computation is in flight, or there is no Files directory, nothing is
//! known: no stale count is ever shown as current.

use std::sync::Arc;

use gpui::{
    App, AsyncApp, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, WeakEntity, Window, div,
};
use gpui_component::Disableable;
use gpui_component::button::Button;
use tod_store::fleet::FleetStore;
use tod_store::fleet::changes::{BranchChanges, ChangesTrigger, changes_trigger, node_branch_changes};
use uuid::Uuid;

use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};

/// What is known about a node's changes.
#[derive(Debug, Clone, Default)]
pub(crate) enum ChangesState {
    /// Not back yet (or being recomputed).
    #[default]
    Unknown,
    /// No ready Files directory, or git failed: nothing to count.
    Unavailable(String),
    Known(Arc<BranchChanges>),
}

impl ChangesState {
    /// The artifact strip's label, only when a current count is known.
    pub(crate) fn label(&self) -> Option<String> {
        match self {
            Self::Known(changes) => Some(format!("Changes {}", changes.files.len())),
            _ => None,
        }
    }
}

/// A view that shows a node's changes and owns a [`ChangesWatch`].
pub(crate) trait HasChangesWatch: 'static + Sized {
    fn changes_watch(&mut self) -> &mut ChangesWatch;
}

/// The background count for one node: recomputed on open, on retarget, when
/// a turn on the node ends, and when its Files settings or directory change.
pub(crate) struct ChangesWatch {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    pub(crate) state: ChangesState,
    /// What the count depended on when it was last started.
    trigger: Option<ChangesTrigger>,
    /// Bumped on every recompute, so a stale result is dropped.
    generation: u64,
    _job: Option<gpui::Task<()>>,
    _watch: gpui::Task<()>,
}

impl ChangesWatch {
    /// Watch `node_id`; the owner calls [`ChangesWatch::recompute`] once it
    /// is built.
    pub(crate) fn new<V: HasChangesWatch>(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        cx: &mut Context<V>,
    ) -> Self {
        let weak = cx.weak_entity();
        let fleet_rx = fleet.clone();
        // A store change is when a turn may have ended or the Files settings
        // changed: each is checked with a cheap read of the trigger (no git),
        // and only a changed trigger recomputes. The channel is
        // drained on a timer (as the task panel's own poll does) rather than
        // awaited, so a store write never wakes this task from the store's
        // thread.
        let watch = cx.spawn(async move |_, cx: &mut AsyncApp| {
            use tokio::sync::broadcast::error::TryRecvError;
            let mut rx = fleet_rx.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                loop {
                    match rx.try_recv() {
                        Ok(()) | Err(TryRecvError::Lagged(_)) => changed = true,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => return,
                    }
                }
                if !changed {
                    continue;
                }
                let Ok(()) = weak.update(cx, |view: &mut V, cx| {
                    let watch = view.changes_watch();
                    if watch.read_trigger() != watch.trigger {
                        let node = watch.node_id;
                        Self::recompute(view, node, cx);
                    }
                }) else {
                    break;
                };
            }
        });
        Self {
            node_id,
            fleet,
            state: ChangesState::Unknown,
            trigger: None,
            generation: 0,
            _job: None,
            _watch: watch,
        }
    }

    fn read_trigger(&self) -> Option<ChangesTrigger> {
        let node_id = self.node_id;
        self.fleet.read(|conn| changes_trigger(conn, node_id)).ok()
    }

    /// Start (or restart) the count for `node_id`, dropping what was known.
    pub(crate) fn recompute<V: HasChangesWatch>(view: &mut V, node_id: Uuid, cx: &mut Context<V>) {
        let watch = view.changes_watch();
        watch.node_id = node_id;
        watch.trigger = watch.read_trigger();
        watch.generation += 1;
        watch.state = ChangesState::Unknown;
        let generation = watch.generation;
        let fleet = watch.fleet.clone();
        let weak: WeakEntity<V> = cx.weak_entity();
        watch._job = Some(cx.spawn(async move |_, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move { node_branch_changes(&fleet, node_id) })
                .await;
            let _ = weak.update(cx, |view: &mut V, cx| {
                let watch = view.changes_watch();
                if watch.generation != generation {
                    return;
                }
                watch.state = match result {
                    Ok(Some(changes)) => ChangesState::Known(Arc::new(changes)),
                    Ok(None) => ChangesState::Unavailable("No Files directory".into()),
                    Err(err) => ChangesState::Unavailable(format!("{err:#}")),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
}

pub struct ChangesPanel {
    fleet: Arc<FleetStore>,
    watch: ChangesWatch,
    /// The last open-in-editor failure, shown under the list.
    open_error: Option<String>,
    /// Set when the failure was a container with no OpenSSH server, which
    /// the button under the error installs.
    sshd_missing: Option<String>,
    installing_sshd: bool,
    focus_handle: FocusHandle,
}

impl HasChangesWatch for ChangesPanel {
    fn changes_watch(&mut self) -> &mut ChangesWatch {
        &mut self.watch
    }
}

impl ChangesPanel {
    pub fn new(node_id: Uuid, fleet: Arc<FleetStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let watch = ChangesWatch::new(node_id, fleet.clone(), cx);
        let mut this = Self {
            fleet,
            watch,
            open_error: None,
            sshd_missing: None,
            installing_sshd: false,
            focus_handle: cx.focus_handle(),
        };
        ChangesWatch::recompute(&mut this, node_id, cx);
        this
    }

    /// Open `rel` in the code editor, off the UI thread.
    fn open_file(&mut self, rel: String, cx: &mut Context<Self>) {
        let ChangesState::Known(changes) = &self.watch.state else {
            return;
        };
        let dir = changes.dir.clone();
        let fleet = self.fleet.clone();
        self.open_error = None;
        self.sshd_missing = None;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tod_store::fleet::code_editor::open_file_in_code_editor(&fleet, &dir, &rel)
                })
                .await;
            if let Err(err) = result {
                let _ = this.update(cx, |this: &mut ChangesPanel, cx| {
                    this.sshd_missing = err
                        .downcast_ref::<tod_agent::devcontainer::NoSshd>()
                        .map(|missing| missing.container.clone());
                    this.open_error = Some(format!("{err:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Install OpenSSH in the container, off the UI thread, then reopen
    /// nothing: the user clicks the file again.
    fn install_sshd(&mut self, container: String, cx: &mut Context<Self>) {
        if self.installing_sshd {
            return;
        }
        self.installing_sshd = true;
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move { tod_agent::devcontainer::install_sshd(&container) })
                .await;
            let _ = this.update(cx, |this: &mut ChangesPanel, cx| {
                this.installing_sshd = false;
                match result {
                    Ok(()) => {
                        this.sshd_missing = None;
                        this.open_error = Some("OpenSSH installed. Open the file again.".into());
                    }
                    Err(err) => this.open_error = Some(format!("{err:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl ColumnPanel for ChangesPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Changes".into()
    }

}

impl EventEmitter<PanelOpenRequest> for ChangesPanel {}

impl Focusable for ChangesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.watch.state.clone() {
            ChangesState::Unknown => style::text_muted(div())
                .child("Counting changes…")
                .into_any_element(),
            ChangesState::Unavailable(reason) => style::text_muted(div())
                .child(selectable_text("unified-changes-unavailable", reason, window, cx))
                .into_any_element(),
            ChangesState::Known(changes) => {
                let mut list = div().flex().flex_col().gap_1().child(style::text_muted(div()).child(
                    selectable_text(
                        "unified-changes-base",
                        format!("{} files changed against {}", changes.files.len(), changes.base),
                        window,
                        cx,
                    ),
                ));
                for (ix, file) in changes.files.iter().enumerate() {
                    let rel = file.path.clone();
                    let counts = match (file.added, file.removed) {
                        (Some(a), Some(r)) => format!("+{a} −{r}"),
                        _ => "binary".to_string(),
                    };
                    list = list.child(
                        div()
                            .id(("unified-changes-file", ix))
                            .flex()
                            .justify_between()
                            .gap_2()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.open_file(rel.clone(), cx)),
                            )
                            .child(style::text_link(div().min_w_0()).child(file.path.clone()))
                            .child(style::text_muted(div().flex_none()).child(counts)),
                    );
                }
                list.into_any_element()
            }
        };
        div()
            .id("unified-changes-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .p_3()
            .overflow_y_scroll()
            .child(body)
            .children(self.open_error.clone().map(|err| {
                style::text_error(div().mt_2()).child(selectable_text(
                    "unified-changes-open-error",
                    err,
                    window,
                    cx,
                ))
            }))
            .children(self.sshd_missing.clone().map(|container| {
                let installing = self.installing_sshd;
                div().mt_2().child(
                    Button::new("unified-changes-install-sshd")
                        .label(if installing {
                            "Installing OpenSSH…"
                        } else {
                            "Install OpenSSH in the container"
                        })
                        .disabled(installing)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.install_sshd(container.clone(), cx);
                        })),
                )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_only_when_known() {
        assert_eq!(ChangesState::Unknown.label(), None);
        assert_eq!(ChangesState::Unavailable("x".into()).label(), None);
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// The panel opened before the node had a Files directory counts once
    /// the directory is set: a Files settings change recounts, not only an
    /// ended turn.
    #[gpui::test]
    fn recounts_when_the_files_directory_is_set_after_opening(cx: &mut gpui::TestAppContext) {
        use crate::views::rows::fixture::Fixture;
        use gpui::AppContext as _;
        use std::cell::RefCell;
        use std::rc::Rc;
        use tod_store::fleet::FleetMutation;
        use tod_store::outline::OutlineMutation;
        use tod_store::outline::types::Capability;

        let repo = std::env::temp_dir().join(format!("tod-changes-ui-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(repo.join("a.txt"), "a\nb\n").unwrap();
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "work"]);

        let fixture = Fixture::new();
        cx.update(gpui_component::init);
        let (node, fleet) = (fixture.node_id, fixture.store.clone());
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| ChangesPanel::new(node, fleet, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view: gpui::Entity<ChangesPanel> = slot.borrow_mut().take().unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(matches!(view.watch.state, ChangesState::Unavailable(_)), "{:?}", view.watch.state);
        });

        fixture
            .store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        fixture
            .store
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: node.to_string(),
                repo: Some(repo.display().to_string()),
            })
            .unwrap();
        // A Files node gets its own worktree by default, which is not made
        // here; the test counts changes in the workspace directory itself.
        fixture
            .store
            .enqueue(FleetMutation::SetNodeUseWorktree {
                node_id: node.to_string(),
                use_worktree: false,
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        // What the app's store watcher does after a commit: the change is
        // announced. The writer commits on its own thread, so this repeats
        // until the projection sees it.
        for _ in 0..50 {
            if fixture.store.reload_if_stale().unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        cx.executor().advance_clock(std::time::Duration::from_millis(500));
        cx.run_until_parked();
        let label = view.read_with(cx, |view, _| view.watch.state.label());
        let state = view.read_with(cx, |view, _| format!("{:?}", view.watch.state));
        let _ = std::fs::remove_dir_all(&repo);
        assert_eq!(label.as_deref(), Some("Changes 2"), "{state}");
    }
}
