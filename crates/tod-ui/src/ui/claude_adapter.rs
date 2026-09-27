//! What the app knows of Claude's ACP adapter, shared by every view: which
//! one is installed and whether it is the latest, and installing it for tod
//! (`tod_agent::claude_adapter`).
//!
//! It is checked once when the app starts. Then tod's own install is brought
//! up to date without asking, since nothing else uses it; a global install is
//! only reported out of date, with the command that updates it, since it is
//! the user's. Settings → Agents shows it and can install it for tod.

use gpui::{App, AppContext as _, Entity, Global, SharedString, Task};
use tod_agent::claude_adapter::{self, AdapterSource, AdapterStatus};

/// The adapter's state, and whatever is being done to it.
#[derive(Default)]
pub struct ClaudeAdapter {
    /// `None` until the first check is back.
    status: Option<AdapterStatus>,
    checking: bool,
    installing: bool,
    /// The last install or update that failed.
    error: Option<String>,
}

struct AdapterGlobal(Entity<ClaudeAdapter>);

impl Global for AdapterGlobal {}

/// The shared state, created on first use.
pub fn state(cx: &mut App) -> Entity<ClaudeAdapter> {
    if let Some(global) = cx.try_global::<AdapterGlobal>() {
        return global.0.clone();
    }
    let entity = cx.new(|_| ClaudeAdapter::default());
    cx.set_global(AdapterGlobal(entity.clone()));
    entity
}

/// Check what is installed and what the latest is, first updating tod's own
/// install when it is behind. Runs npm and asks the registry, on the
/// background executor; the task ends when the state has the answer.
pub fn check(cx: &mut App) -> Task<()> {
    let entity = state(cx);
    entity.update(cx, |adapter, cx| {
        adapter.checking = true;
        cx.notify();
    });
    let work = cx.background_spawn(async move {
        let updated = claude_adapter::update_local_if_stale();
        (updated, claude_adapter::status(true))
    });
    cx.spawn(async move |cx| {
        let (updated, status) = work.await;
        let _ = entity.update(cx, |adapter, cx| {
            adapter.checking = false;
            match updated {
                Ok(Some(installed)) => tracing::info!(
                    version = installed.version.as_deref().unwrap_or("?"),
                    "updated tod's Claude ACP adapter"
                ),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("updating tod's Claude ACP adapter: {err:#}");
                    adapter.error = Some(format!("Updating tod's adapter failed: {err:#}"));
                }
            }
            adapter.status = Some(status);
            cx.notify();
        });
    })
}

/// Install the latest adapter for tod only, then check again.
pub fn install_for_tod(cx: &mut App) {
    let entity = state(cx);
    let Some(dir) = claude_adapter::local_dir() else {
        entity.update(cx, |adapter, cx| {
            adapter.error = Some("There is no directory to install tod's adapter in.".into());
            cx.notify();
        });
        return;
    };
    if entity.read(cx).installing {
        return;
    }
    entity.update(cx, |adapter, cx| {
        adapter.installing = true;
        adapter.error = None;
        cx.notify();
    });
    let work = cx.background_spawn(async move {
        let installed = claude_adapter::install_local(&dir);
        (installed, claude_adapter::status(true))
    });
    cx.spawn(async move |cx| {
        let (installed, status) = work.await;
        let _ = entity.update(cx, |adapter, cx| {
            adapter.installing = false;
            if let Err(err) = installed {
                adapter.error = Some(format!("{err:#}"));
            }
            adapter.status = Some(status);
            cx.notify();
        });
    })
    .detach();
}

/// How much the adapter's state needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Fine,
    /// It runs, but should be updated.
    Warning,
    /// Claude cannot run.
    Error,
}

/// What Settings shows: a sentence, the command to run if any, and how
/// much it needs the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub text: SharedString,
    pub command: Option<SharedString>,
    pub severity: Severity,
}

impl ClaudeAdapter {
    pub fn busy(&self) -> bool {
        self.checking || self.installing
    }

    pub fn installing(&self) -> bool {
        self.installing
    }

    /// Whether tod's own install is the one in use.
    pub fn installed_for_tod(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|s| s.installed.as_ref())
            .is_some_and(|i| i.source == AdapterSource::Local)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// What to tell the user; `None` before the first check.
    pub fn summary(&self) -> Option<Summary> {
        self.status.as_ref().map(summary)
    }

    /// What the app says unasked when it starts: only what needs the user.
    pub fn startup_message(&self) -> Option<(Severity, String)> {
        startup_message(self.status.as_ref()?)
    }
}

/// [`ClaudeAdapter::startup_message`] for `status`.
fn startup_message(status: &AdapterStatus) -> Option<(Severity, String)> {
    let command = claude_adapter::GLOBAL_INSTALL;
    let Some(installed) = &status.installed else {
        return Some((
            Severity::Error,
            format!(
                "Claude's ACP adapter (claude-agent-acp) is not installed, so tod cannot run \
                 Claude. Install it for tod only in Settings → Agents, or for everyone on this \
                 machine:\n{command}"
            ),
        ));
    };
    if !status.out_of_date() {
        return None;
    }
    let versions = format!(
        "{} installed, {} is out",
        installed.version.as_deref().unwrap_or("?"),
        status.latest.as_deref().unwrap_or("?")
    );
    match installed.source {
        AdapterSource::Global => Some((
            Severity::Warning,
            format!(
                "Claude's ACP adapter is out of date ({versions}). Update it:\n{command}\n\
                 Or install it for tod only in Settings → Agents, which tod keeps up to date."
            ),
        )),
        // tod's own failed to update; Settings says why.
        AdapterSource::Local => Some((
            Severity::Warning,
            format!(
                "tod could not update its Claude ACP adapter ({versions}). See Settings → Agents."
            ),
        )),
        AdapterSource::Override => None,
    }
}

/// What `status` means for the user.
pub fn summary(status: &AdapterStatus) -> Summary {
    let Some(installed) = &status.installed else {
        return Summary {
            text: format!(
                "Not installed, so tod cannot run Claude. Install it for tod only, \
                 or for everyone on this machine with the command below. \
                 (The older claude-code-acp is not used: it runs an outdated Claude Code \
                 and ignores the model and effort settings.)"
            )
            .into(),
            command: Some(claude_adapter::GLOBAL_INSTALL.into()),
            severity: Severity::Error,
        };
    };
    let version = installed.version.as_deref().unwrap_or("version unknown");
    let latest_note = match (&status.latest, &status.latest_error) {
        (Some(latest), _) if !status.out_of_date() => format!(" It is the latest ({latest})."),
        (Some(_), _) => String::new(),
        (None, Some(_)) => " Could not check for a newer version.".to_string(),
        (None, None) => String::new(),
    };
    match installed.source {
        AdapterSource::Local => Summary {
            text: format!(
                "Installed for tod only ({version}); tod keeps it up to date.{latest_note}"
            )
            .into(),
            command: None,
            severity: if status.out_of_date() {
                Severity::Warning
            } else {
                Severity::Fine
            },
        },
        AdapterSource::Global if status.out_of_date() => Summary {
            text: format!(
                "Installed for everyone on this machine ({version}), but {} is out. \
                 Update it with the command below, or install it for tod only, \
                 which tod keeps up to date.",
                status.latest.as_deref().unwrap_or("a newer version")
            )
            .into(),
            command: Some(claude_adapter::GLOBAL_INSTALL.into()),
            severity: Severity::Warning,
        },
        AdapterSource::Global => Summary {
            text: format!("Installed for everyone on this machine ({version}).{latest_note}")
                .into(),
            command: None,
            severity: Severity::Fine,
        },
        AdapterSource::Override => Summary {
            text: format!(
                "Using {} from {} ({version}).",
                installed.bin.display(),
                claude_adapter::BIN_ENV
            )
            .into(),
            command: None,
            severity: Severity::Fine,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tod_agent::claude_adapter::InstalledAdapter;

    fn installed(source: AdapterSource, version: &str) -> Option<InstalledAdapter> {
        Some(InstalledAdapter {
            bin: PathBuf::from("claude-agent-acp"),
            source,
            version: Some(version.into()),
        })
    }

    #[test]
    fn nothing_installed_says_what_to_run() {
        let summary = summary(&AdapterStatus::default());
        assert_eq!(summary.severity, Severity::Error);
        assert_eq!(summary.command.as_deref(), Some(claude_adapter::GLOBAL_INSTALL));
        let (severity, message) = startup_message(&AdapterStatus::default()).unwrap();
        assert_eq!(severity, Severity::Error);
        assert!(message.contains("claude-agent-acp) is not installed"), "{message}");
        assert!(message.contains(claude_adapter::GLOBAL_INSTALL), "{message}");
    }

    #[test]
    fn an_old_global_install_is_a_warning_with_the_update_command() {
        let summary = summary(&AdapterStatus {
            installed: installed(AdapterSource::Global, "0.29.0"),
            latest: Some("0.30.0".into()),
            latest_error: None,
        });
        assert_eq!(summary.severity, Severity::Warning);
        assert!(summary.text.contains("0.30.0 is out"), "{}", summary.text);
        assert_eq!(summary.command.as_deref(), Some(claude_adapter::GLOBAL_INSTALL));
    }

    #[test]
    fn a_current_install_needs_nothing() {
        let status = AdapterStatus {
            installed: installed(AdapterSource::Global, "0.30.0"),
            latest: Some("0.30.0".into()),
            latest_error: None,
        };
        assert_eq!(summary(&status).severity, Severity::Fine);
        let adapter = ClaudeAdapter {
            status: Some(status),
            ..ClaudeAdapter::default()
        };
        assert_eq!(adapter.startup_message(), None);
    }
}
