//! Store writes without blocking the UI thread.
//!
//! Most writes through `FleetStore` wait for the writer thread to commit
//! (`interview`, an immediate mutation, `writer().flush()`, undo), so called
//! from a view they freeze the window for as long as a commit takes. The
//! workbench runs them through [`off_thread`] instead: `work` runs on the
//! background executor, and `done` gets its result back on the entity, where
//! it updates what is shown. Anything shown optimistically is set before the
//! call and corrected in `done`.

use gpui::Context;

/// Run `work` on the background executor, then `done` on the view with its
/// result. `done` is skipped if the view is gone.
pub fn off_thread<V, R>(
    cx: &mut Context<V>,
    work: impl FnOnce() -> R + Send + 'static,
    done: impl FnOnce(&mut V, R, &mut Context<V>) + 'static,
) where
    V: 'static,
    R: Send + 'static,
{
    cx.spawn(async move |this, cx| {
        let result = cx.background_executor().spawn(async move { work() }).await;
        let _ = this.update(cx, |this, cx| done(this, result, cx));
    })
    .detach();
}
