//! Service presets for the Environment capability, and the test call that
//! checks a credential works.
//!
//! Presets are data: a bundled file (`presets/environment.toml`) and the
//! user's `environment-presets.toml` in the data root, merged by `id` with the
//! user's winning. Adding a service is adding an entry to that file, which
//! Settings shows and edits, and which can be shared by copying it.

use crate::environment::{Auth, Entry, EntryKind, host_of};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BUNDLED: &str = include_str!("../presets/environment.toml");
pub const USER_FILE: &str = "environment-presets.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// `bearer`, `header:<Name>`, or `basic:<username>`.
    #[serde(default = "default_auth")]
    pub auth: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub env_var: String,
    #[serde(default)]
    pub test_url: Option<String>,
    /// Plain variables the service usually needs (`NAME = "default"`).
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, String>,
    /// Where it came from; not in the file.
    #[serde(skip)]
    pub user_defined: bool,
}

fn default_auth() -> String {
    "bearer".into()
}

#[derive(Deserialize)]
struct PresetFile {
    #[serde(default)]
    preset: Vec<Preset>,
}

impl Preset {
    pub fn auth(&self) -> Auth {
        match self.auth.split_once(':') {
            Some(("header", name)) if !name.trim().is_empty() => Auth::Header { header: name.trim().into() },
            Some(("basic", user)) => Auth::Basic { username: user.trim().into() },
            _ => Auth::Bearer,
        }
    }

    /// A new secret entry filled in from this preset; the user supplies the
    /// value (and changes the host for a self-hosted service).
    pub fn to_entry(&self) -> Entry {
        let name = if self.name.trim().is_empty() { self.id.clone() } else { self.name.clone() };
        let mut entry = Entry::secret(&name);
        entry.env_var = self.env_var.clone();
        entry.description = Some(self.description.clone()).filter(|d| !d.trim().is_empty());
        entry.hosts = self.hosts.iter().filter_map(|h| host_of(h)).collect();
        entry.auth = self.auth();
        entry.preset = Some(self.id.clone());
        entry.test_url = self.test_url.clone();
        entry
    }
}

pub fn user_file(data_root: &Path) -> PathBuf {
    data_root.join(USER_FILE)
}

fn parse(text: &str, user_defined: bool) -> Result<Vec<Preset>> {
    let file: PresetFile = toml::from_str(text).context("not a valid presets file")?;
    Ok(file
        .preset
        .into_iter()
        .map(|mut p| {
            p.user_defined = user_defined;
            p
        })
        .collect())
}

/// The bundled presets, then the user's: a user preset replaces a bundled one
/// with the same id. An unreadable user file is an error the caller shows;
/// [`bundled`] still has the shipped presets.
pub fn load(data_root: &Path) -> Result<Vec<Preset>> {
    let mut all = bundled();
    let path = user_file(data_root);
    if let Ok(text) = std::fs::read_to_string(&path) {
        for preset in parse(&text, true).with_context(|| format!("{}", path.display()))? {
            match all.iter_mut().find(|p| p.id == preset.id) {
                Some(existing) => *existing = preset,
                None => all.push(preset),
            }
        }
    }
    Ok(all)
}

pub fn bundled() -> Vec<Preset> {
    parse(BUNDLED, false).expect("the bundled presets file is valid")
}

/// The user's presets file as text, or a commented template to start from.
pub fn editable_text(data_root: &Path) -> String {
    std::fs::read_to_string(user_file(data_root)).unwrap_or_else(|_| {
        "# Your presets, in the same format as tod's bundled ones. A preset with the\n\
         # same `id` replaces the bundled one.\n\n\
         # [[preset]]\n\
         # id = \"my-service\"\n\
         # label = \"My service\"\n\
         # hosts = [\"api.example.com\"]\n\
         # auth = \"bearer\"\n\
         # env_var = \"MY_SERVICE_API_KEY\"\n\
         # test_url = \"https://{host}/v1/me\"\n"
            .to_string()
    })
}

/// Save the user's presets file after checking it parses.
pub fn save_user_file(data_root: &Path, text: &str) -> Result<()> {
    parse(text, true)?;
    std::fs::create_dir_all(data_root)?;
    std::fs::write(user_file(data_root), text)?;
    Ok(())
}

/// What a test call found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
    pub ok: bool,
    pub message: String,
}

/// Call the entry's `test_url` with `value` applied as the entry's auth says.
/// Nothing is sent unless the URL is https and its host is one of the entry's
/// hosts; the secret never goes anywhere else.
pub fn test_call(entry: &Entry, value: &str) -> TestOutcome {
    let fail = |message: String| TestOutcome { ok: false, message };
    if entry.kind != EntryKind::Secret {
        return fail("only secrets can be tested".into());
    }
    let Some(url) = entry.test_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
        return fail("this entry has no test URL".into());
    };
    let Some(first_host) = entry.hosts.first() else {
        return fail("add the service's host first".into());
    };
    let url = url.replace("{host}", first_host);
    if let Err(err) = entry.validate() {
        return fail(err.to_string());
    }
    let host = host_of(&url).unwrap_or_default();
    if !url.starts_with("https://") || !entry.hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        return fail("the test URL must be https and on one of the entry's hosts".into());
    }
    let (header, template) = entry.auth.header_template();
    let header_value = template.replace("{value}", &entry.auth.proxy_secret(value));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    match agent.get(&url).header(&header, &header_value).call() {
        Ok(response) => {
            let status = response.status().as_u16();
            match status {
                200..=299 => TestOutcome { ok: true, message: format!("works (HTTP {status} from {host})") },
                401 | 403 => fail(format!("{host} refused the credential (HTTP {status})")),
                _ => fail(format!(
                    "{host} answered HTTP {status}; the credential may be fine but this URL is not a good test"
                )),
            }
        }
        Err(err) => fail(format!("could not reach {host}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_presets_parse_and_make_valid_entries() {
        let presets = bundled();
        assert!(presets.iter().any(|p| p.id == "growthbook"));
        for preset in &presets {
            let entry = preset.to_entry();
            entry.validate().unwrap_or_else(|e| panic!("{}: {e}", preset.id));
            assert!(entry.proxied(), "{} should be applied by the proxy", preset.id);
        }
    }

    #[test]
    fn a_user_preset_replaces_a_bundled_one_and_adds_new_ones() {
        let dir = std::env::temp_dir().join(format!("tod-presets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            user_file(&dir),
            "[[preset]]\nid = \"growthbook\"\nlabel = \"Our GrowthBook\"\n\
             hosts = [\"gb.internal.example\"]\nauth = \"header:X-Key\"\n\
             [[preset]]\nid = \"mine\"\nlabel = \"Mine\"\n",
        )
        .unwrap();
        let all = load(&dir).unwrap();
        let gb = all.iter().find(|p| p.id == "growthbook").unwrap();
        assert_eq!(gb.label, "Our GrowthBook");
        assert!(gb.user_defined);
        assert_eq!(gb.auth(), Auth::Header { header: "X-Key".into() });
        assert!(all.iter().any(|p| p.id == "mine"));
        assert_eq!(all.iter().filter(|p| p.id == "growthbook").count(), 1);
        std::fs::write(user_file(&dir), "[[preset]]\nid = ").unwrap();
        assert!(load(&dir).is_err());
        assert!(save_user_file(&dir, "not toml ===").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_test_never_goes_to_a_host_the_entry_does_not_list() {
        let mut entry = Entry::secret("gb");
        entry.hosts = vec!["api.example.com".into()];
        entry.test_url = Some("https://evil.example.net/x".into());
        let outcome = test_call(&entry, "secret");
        assert!(!outcome.ok);
        assert!(outcome.message.contains("host"), "{}", outcome.message);
    }
}
