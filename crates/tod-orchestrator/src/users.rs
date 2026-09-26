//! Each user's data root, `<base>/users/<user>/`, opened as a `FleetStore` on
//! first use and serving its mutation socket, so every `tod-cli` run against
//! it writes through that one store's writer.

use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tod_store::fleet::FleetStore;
use tod_store::fleet::mutation_socket::{self, PortFileGuard};

pub struct UserData {
    pub root: PathBuf,
    pub store: Arc<FleetStore>,
    _socket: PortFileGuard,
}

pub struct Users {
    base: PathBuf,
    open: Mutex<HashMap<String, Arc<UserData>>>,
}

/// A user name is one path segment: ASCII letters, digits, `-`, `_`, `.`,
/// 1 to 64 long, not starting with `.` or `-`.
pub fn validate(user: &str) -> Result<()> {
    let ok = !user.is_empty()
        && user.len() <= 64
        && !user.starts_with(['.', '-'])
        && user.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !ok {
        bail!("invalid user {user:?}: use letters, digits, '-', '_', '.' (not first), at most 64");
    }
    Ok(())
}

impl Users {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into(), open: Mutex::new(HashMap::new()) }
    }

    pub fn root_of(&self, user: &str) -> Result<PathBuf> {
        validate(user)?;
        Ok(self.base.join("users").join(user))
    }

    /// The user's store, opening it (and creating the data root) on first use.
    pub fn get(&self, user: &str) -> Result<Arc<UserData>> {
        let root = self.root_of(user)?;
        // Held across the open so two first requests do not both open it.
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(data) = open.get(user) {
            return Ok(data.clone());
        }
        std::fs::create_dir_all(&root).map_err(|e| anyhow!("create {}: {e}", root.display()))?;
        let store = Arc::new(FleetStore::open(&root).map_err(|e| anyhow!("open {}: {e}", root.display()))?);
        let socket = mutation_socket::start(store.clone(), &root)?;
        eprintln!("tod-orchestrator: opened {}", root.display());
        let data = Arc::new(UserData { root, store, _socket: socket });
        open.insert(user.to_string(), data.clone());
        Ok(data)
    }

    /// Forget the user's open store (for replacing its database). It closes
    /// once no request still holds it.
    pub fn close(&self, user: &str) {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).remove(user);
    }

    pub fn base(&self) -> &Path {
        &self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names_are_one_safe_segment() {
        for good in ["alice", "a.b", "user_1", "x-y"] {
            assert!(validate(good).is_ok(), "{good}");
        }
        for bad in ["", "..", ".x", "-x", "a/b", "a\\b", "a b", "é", &"x".repeat(65)] {
            assert!(validate(bad).is_err(), "{bad}");
        }
    }
}
