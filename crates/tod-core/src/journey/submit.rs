//! Delivering sealed bundles to a relay (`doc/journeys/spec.md` §9.3-9.4).
//!
//! [`Relay`] is the one seam submission goes through: `put` pushes one part
//! of a sealed bundle, `acknowledgements` polls for what the receiver has
//! confirmed. [`NtfyRelay`] is the real implementation (ntfy.sh, via
//! `tod_integration::ntfy`); [`FolderRelay`] is a filesystem stand-in used by
//! every test in this module and in `worker.rs`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use uuid::Uuid;

use tod_journey::relay_code::RelayCode;

/// A cursor into a relay's acknowledgement feed. Opaque to callers beyond
/// passing it back into the next [`Relay::acknowledgements`] call.
pub type AckCursor = String;

/// The literal cursor value meaning "everything the relay still has".
pub const ACK_CURSOR_START: &str = "all";

/// Where sealed bundle parts go, and how the app learns the receiver got
/// them.
pub trait Relay {
    /// Pushes one named part (`put`'s `name` is the attachment filename,
    /// e.g. `<bundle-id>.journey.age` or `<bundle-id>.2-of-3.journey.age`).
    fn put(&self, name: &str, bytes: &[u8]) -> Result<()>;

    /// Returns every bundle id acknowledged since `since`, plus the cursor
    /// to pass next time. `since == ACK_CURSOR_START` reads everything the
    /// relay still holds.
    fn acknowledgements(&self, since: &AckCursor) -> Result<(Vec<Uuid>, AckCursor)>;
}

/// The real relay: ntfy.sh (or a self-hosted instance), addressed by a
/// parsed [`RelayCode`].
pub struct NtfyRelay {
    code: RelayCode,
}

impl NtfyRelay {
    pub fn new(code: RelayCode) -> Self {
        Self { code }
    }

    /// Parses `s` (the settings' pasted relay code) into an `NtfyRelay`.
    pub fn parse(s: &str) -> Result<Self> {
        Ok(Self::new(RelayCode::parse(s)?))
    }

    pub fn recipient(&self) -> &str {
        &self.code.recipient
    }
}

impl Relay for NtfyRelay {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        tod_integration::ntfy::publish_file(&self.code.server, &self.code.inbox, name, bytes)
    }

    fn acknowledgements(&self, since: &AckCursor) -> Result<(Vec<Uuid>, AckCursor)> {
        let messages = tod_integration::ntfy::poll(&self.code.server, &self.code.ack, since)?;
        let mut cursor = since.clone();
        let mut ids = Vec::new();
        for msg in messages {
            cursor = msg.id.clone();
            if let Some(rest) = msg.message.strip_prefix("got ") {
                if let Ok(id) = Uuid::parse_str(rest.trim()) {
                    ids.push(id);
                }
            }
        }
        Ok((ids, cursor))
    }
}

/// A filesystem-backed [`Relay`] for tests (and, per the spec, for the app
/// running on the receiving machine): `put` writes files into `dir`,
/// `acknowledgements` reads `got <id>` lines appended to `dir/acks` — a test
/// "receiver" appends to that file to simulate acknowledging a bundle.
pub struct FolderRelay {
    dir: PathBuf,
}

impl FolderRelay {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).context("creating folder relay directory")?;
        Ok(Self { dir })
    }

    fn acks_path(&self) -> PathBuf {
        self.dir.join("acks")
    }

    /// Test/receiver helper: appends `got <id>` to the acks file, simulating
    /// the receiving desktop acknowledging a bundle.
    pub fn simulate_ack(&self, bundle_id: Uuid) -> Result<()> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.acks_path())
            .context("opening acks file")?;
        writeln!(f, "got {bundle_id}").context("appending ack line")?;
        Ok(())
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

impl Relay for FolderRelay {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        fs::write(self.dir.join(name), bytes).context("writing folder relay part")
    }

    fn acknowledgements(&self, since: &AckCursor) -> Result<(Vec<Uuid>, AckCursor)> {
        let path = self.acks_path();
        if !path.exists() {
            return Ok((Vec::new(), since.clone()));
        }
        let contents = fs::read_to_string(&path).context("reading acks file")?;
        let lines: Vec<&str> = contents.lines().collect();
        // `since` is either "all" (start from the top) or the number of
        // lines already consumed, encoded as a string cursor.
        let start: usize = if since == ACK_CURSOR_START {
            0
        } else {
            since.parse().unwrap_or(0)
        };
        let mut ids = Vec::new();
        for line in lines.iter().skip(start) {
            if let Some(rest) = line.strip_prefix("got ") {
                if let Ok(id) = Uuid::parse_str(rest.trim()) {
                    ids.push(id);
                }
            }
        }
        Ok((ids, lines.len().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tod-submit-test-{name}-{}", Uuid::new_v4()))
    }

    #[test]
    fn folder_relay_put_writes_a_file() {
        let dir = tmp_dir("put");
        let relay = FolderRelay::new(&dir).unwrap();
        relay.put("bundle.journey.age", b"ciphertext").unwrap();
        let bytes = fs::read(dir.join("bundle.journey.age")).unwrap();
        assert_eq!(bytes, b"ciphertext");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_relay_acknowledgements_reads_got_lines_since_cursor() {
        let dir = tmp_dir("ack");
        let relay = FolderRelay::new(&dir).unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        let (ids, cursor) = relay.acknowledgements(&ACK_CURSOR_START.to_string()).unwrap();
        assert!(ids.is_empty());

        relay.simulate_ack(a).unwrap();
        let (ids, cursor) = relay.acknowledgements(&cursor).unwrap();
        assert_eq!(ids, [a]);

        relay.simulate_ack(b).unwrap();
        let (ids, cursor2) = relay.acknowledgements(&cursor).unwrap();
        assert_eq!(ids, [b]);

        // Re-polling from the same cursor sees nothing new.
        let (ids, _) = relay.acknowledgements(&cursor2).unwrap();
        assert!(ids.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_relay_acknowledgements_from_start_sees_everything() {
        let dir = tmp_dir("ack-all");
        let relay = FolderRelay::new(&dir).unwrap();
        let a = Uuid::new_v4();
        relay.simulate_ack(a).unwrap();
        let (ids, _) = relay.acknowledgements(&ACK_CURSOR_START.to_string()).unwrap();
        assert_eq!(ids, [a]);
        let _ = fs::remove_dir_all(&dir);
    }
}
