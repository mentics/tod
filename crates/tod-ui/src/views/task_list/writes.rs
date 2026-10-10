//! Outline writes from the task list without blocking the UI thread.
//!
//! `FleetStore::enqueue_outline` and `writer().flush()` wait on the writer
//! thread. Each write here goes to one serial worker (so writes commit in the
//! order the user made them) and its completion runs back on the view, with
//! the window, where it refreshes what is shown or reports the failure.

use std::sync::mpsc::{Sender, channel};

use gpui::{Context, Window};
use tod_store::outline::OutlineMutation;

use super::TaskListView;

type Job = Box<dyn FnOnce() + Send>;

/// One background thread running jobs in the order they were queued.
pub(super) struct WriteQueue {
    tx: Sender<Job>,
}

impl WriteQueue {
    pub(super) fn new() -> Self {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("task-list-writes".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            })
            .expect("spawn task list write thread");
        Self { tx }
    }
}

impl TaskListView {
    /// Queue `mutation`, commit it off the UI thread, then run `done` with
    /// the outcome (the error already worded as `err.to_string()`).
    pub(super) fn write_outline(
        &mut self,
        mutation: OutlineMutation,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let fleet = self.fleet.clone();
        let (result_tx, result_rx) = async_channel::bounded::<Result<(), String>>(1);
        let job: Job = Box::new(move || {
            let result = fleet
                .enqueue_outline(mutation)
                .and_then(|_| fleet.writer().flush())
                .map_err(|err| err.to_string());
            let _ = result_tx.send_blocking(result);
        });
        if self.writes.tx.send(job).is_err() {
            done(self, Err("the writer is not running".into()), window, cx);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = result_rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("the write was dropped".into()));
            let _ = this.update_in(cx, |this, window, cx| done(this, result, window, cx));
        })
        .detach();
    }
}
