//! Sync between a user's app and their copy here: the seed snapshot, the
//! app's changes up, and the agents' changes down.
//!
//! TODO(W2): wire to `tod_store::sync` once it lands (`snapshot`/`restore`,
//! `export_changes`/`apply_changes`). Every function below answers 501 until
//! then; the routes, user checks, and store opening in `lib.rs` are done, so
//! wiring it is a change to this file only.
//!
//! Expected wiring:
//! - [`seed`]: `restore(root, body)`. The store must not be open while it
//!   replaces the database: close the user's `FleetStore` (see
//!   `Users::close`) first, restore, and let the next request reopen it.
//! - [`apply_changes`]: `apply_changes(&store, body)`; reply with the last
//!   number accepted.
//! - [`export_changes`]: `export_changes(&store, after)`, the encoded changes
//!   as the body.

use crate::http::Response;
use crate::users::{UserData, Users};

pub fn seed(users: &Users, user: &str, snapshot: &[u8]) -> Response {
    let _ = (users, user, snapshot);
    not_yet("seed")
}

pub fn apply_changes(user: &UserData, body: &[u8]) -> Response {
    let _ = (user, body);
    not_yet("changes (up)")
}

pub fn export_changes(user: &UserData, after: i64) -> Response {
    let _ = (user, after);
    not_yet("changes (down)")
}

fn not_yet(what: &str) -> Response {
    Response::text(501, format!("{what}: sync is not implemented in this orchestrator yet"))
}
