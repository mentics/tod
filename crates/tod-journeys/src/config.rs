//! `<home>/config.toml`: the relay server, the two topic names, and the last
//! inbox message id `pull` has processed (spec §9.5, §9.3).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
    pub inbox: String,
    pub ack: String,
    #[serde(default)]
    pub last_seen: Option<String>,
}

impl Config {
    pub fn path(home: &Path) -> PathBuf {
        home.join("config.toml")
    }

    pub fn load(home: &Path) -> Result<Self> {
        let path = Self::path(home);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} — run `tod-journeys init` first", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        let path = Self::path(home);
        let text = toml::to_string_pretty(self).context("encoding config.toml")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// Where the age identity is kept, never shown again after `init`.
pub fn identity_path(home: &Path) -> PathBuf {
    home.join("identity.txt")
}

/// Where completed bundles land, named `<bundle-id>.journey`.
pub fn received_dir(home: &Path) -> PathBuf {
    home.join("received")
}

/// Where in-progress split-bundle parts are buffered until all have arrived.
pub fn parts_dir(home: &Path) -> PathBuf {
    home.join("parts")
}

/// The OS data dir, `tod-journeys/` subdirectory — the default `--home`.
pub fn default_home() -> Result<PathBuf> {
    let base = dirs::data_dir().context("could not determine the OS data directory")?;
    Ok(base.join("tod-journeys"))
}

/// A 32-character random topic name (alphanumeric, from a CSPRNG). Knowing
/// the name is what grants access to an ntfy topic, so this needs no
/// separate secret (spec §9.3).
pub fn random_topic() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}
