//! The line under a conversation's title: which agent, model, and effort the
//! conversation runs on, then what it has spent.
//!
//! What the agent itself says wins: the model that served the latest request
//! (the session log), and the model and effort the agent says the session
//! runs (its ACP config options). Until it has said, the line shows what the
//! turn asks for — the settings and the node's Agent capability — marked
//! "expected", so a value that is not marked is known to be real.
//!
//! Reading the session logs, the settings file, and the store happens off the
//! main thread: a view asks [`SessionInfo::take_due`] what to read, runs
//! [`SessionRead::run`] on the background executor, and hands the result to
//! [`SessionInfo::apply`].

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tod_agent::{AgentLaunchOptions, TokenUsage};
use tod_core::conversation::{ConversationDriver, ConversationStatus};
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::token_usage;

/// How often what is shown is read again while nothing says it changed: the
/// settings and the node's Agent capability, which say nothing when they
/// change, and a running turn's usage.
const REFRESH: Duration = Duration::from_secs(10);

/// What is shown for one conversation, and when it was last read.
#[derive(Default)]
pub struct SessionInfo {
    shown: Option<Shown>,
    /// The usage the conversation's sessions' platform records say.
    recorded: Option<TokenUsage>,
    /// What the next turn launches with.
    expected: Option<AgentLaunchOptions>,
    read_at: Option<Instant>,
    /// When the usage was last read: the whole session log, so only while a
    /// turn runs, or when one ended.
    usage_read_at: Option<Instant>,
    /// A turn ended since the last read.
    stale: bool,
    reading: bool,
}

/// The conversation shown: an unsaved one is known by its focus and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    focus: Focus,
    kind: ProtocolKind,
    conversation: Option<Uuid>,
}

/// One read of what [`SessionInfo`] shows. Clone it to keep one to hand back
/// to [`SessionInfo::apply`].
#[derive(Debug, Clone)]
pub struct SessionRead {
    shown: Shown,
    settings_path: Option<PathBuf>,
    /// Read the usage too, not only what the next turn launches with.
    usage: bool,
}

/// What a [`SessionRead`] found.
pub struct SessionReadResult {
    /// `None` when the usage was not read.
    recorded: Option<Option<TokenUsage>>,
    expected: Option<AgentLaunchOptions>,
}

impl SessionRead {
    /// Read the session logs, the settings, and the store: never on the UI
    /// thread.
    pub fn run(&self, fleet: &FleetStore) -> SessionReadResult {
        let recorded = self.usage.then(|| {
            self.shown.conversation.and_then(|id| {
                tod_core::run_transcript::usage_for_key(fleet, &ConversationDriver::session_key(id))
            })
        });
        let expected = self.settings_path.as_deref().map(|path| {
            tod_core::conversation::launch::resolve_from(fleet, path, self.shown.focus, self.shown.kind)
        });
        SessionReadResult { recorded, expected }
    }
}

impl SessionInfo {
    /// Point at the conversation shown. What was read for another one is
    /// dropped, and it is read again.
    pub fn show(&mut self, focus: Focus, kind: ProtocolKind, conversation: Option<Uuid>) {
        let shown = Shown {
            focus,
            kind,
            conversation,
        };
        if self.shown != Some(shown) {
            *self = Self {
                shown: Some(shown),
                ..Self::default()
            };
        }
    }

    /// Read everything again on the next [`Self::take_due`]: a turn ended.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// What to read now, if anything is due: everything when never read or
    /// a turn ended; after [`REFRESH`], the launch, and the usage too while
    /// a turn is `running`. The caller runs it and hands the result to
    /// [`Self::apply`].
    pub fn take_due(&mut self, running: bool) -> Option<SessionRead> {
        let shown = self.shown?;
        if self.reading {
            return None;
        }
        let elapsed = |at: Option<Instant>| at.is_none_or(|at| at.elapsed() >= REFRESH);
        let usage = self.stale || self.usage_read_at.is_none() || (running && elapsed(self.usage_read_at));
        if !usage && !elapsed(self.read_at) {
            return None;
        }
        let now = Instant::now();
        self.stale = false;
        self.reading = true;
        self.read_at = Some(now);
        if usage {
            self.usage_read_at = Some(now);
        }
        let settings_path = crate::interview::TodPaths::discover()
            .ok()
            .map(|paths| paths.settings_path());
        Some(SessionRead {
            shown,
            settings_path,
            usage,
        })
    }

    /// Keep what `read` found, unless another conversation is shown by now.
    /// Returns whether what is shown changed.
    pub fn apply(&mut self, read: &SessionRead, result: SessionReadResult) -> bool {
        if self.shown != Some(read.shown) {
            return false;
        }
        self.reading = false;
        let mut changed = self.expected != result.expected;
        self.expected = result.expected;
        if let Some(recorded) = result.recorded {
            changed |= self.recorded != recorded;
            self.recorded = recorded;
        }
        changed
    }

    /// The usage to show: the platform records', filled in with what the
    /// agent reported live where the records say nothing.
    pub fn usage(&self, status: &ConversationStatus) -> Option<TokenUsage> {
        match (self.recorded.clone(), status.live_usage.as_ref()) {
            (Some(mut recorded), Some(live)) => {
                recorded.fill_from_live(live);
                Some(recorded)
            }
            (recorded, live) => recorded.or_else(|| live.cloned()),
        }
    }

    /// The line under the title and every figure behind it, for a
    /// conversation whose driver reports `status`.
    pub fn line(&self, status: &ConversationStatus) -> Option<(String, String)> {
        // What the latest turn asked for; what the next one will before any.
        let asked = status.launch.as_ref().or(self.expected.as_ref());
        line(asked, self.usage(status).as_ref())
    }
}

/// Whether `usage` says anything of tokens or cost, beyond what it runs.
fn has_spending(usage: &TokenUsage) -> bool {
    !usage.total.is_empty() || usage.context_tokens.is_some() || usage.cost.is_some()
}

/// The line: `Claude · claude-sonnet-5 · effort medium · Tokens: …`, each of
/// the agent's values marked "expected" until the agent has said it.
pub fn line(asked: Option<&AgentLaunchOptions>, usage: Option<&TokenUsage>) -> Option<(String, String)> {
    if asked.is_none() && usage.is_none() {
        return None;
    }
    let expected = |value: &str| format!("{value} (expected)");
    let mut parts = Vec::new();
    if let Some(asked) = asked {
        parts.push(asked.platform.label().to_string());
    }
    let answered = usage.and_then(|u| u.model.clone());
    let said_model = usage.and_then(|u| u.agent_model.clone());
    let said_effort = usage.and_then(|u| u.agent_effort.clone());
    match (&answered, &said_model, asked) {
        (Some(model), _, _) | (None, Some(model), _) => parts.push(model.clone()),
        (None, None, Some(asked)) => parts.push(expected(&asked.model)),
        (None, None, None) => {}
    }
    match (&said_effort, asked) {
        (Some(effort), _) => parts.push(format!("effort {effort}")),
        (None, Some(asked)) => parts.push(format!("effort {}", expected(&asked.effort))),
        (None, None) => {}
    }
    if let Some(usage) = usage.filter(|u| has_spending(u)) {
        parts.push(token_usage::summary(usage));
    }

    let mut details = Vec::new();
    if let Some(asked) = asked {
        details.push(format!(
            "Asked for: {} · {} · effort {}",
            asked.platform.label(),
            asked.model,
            asked.effort
        ));
    }
    match (&said_model, &said_effort) {
        (None, None) => details.push("The agent has not said what it runs yet".to_string()),
        (model, effort) => details.push(format!(
            "The agent says it runs: {} · effort {}",
            model.as_deref().unwrap_or("(not said)"),
            effort.as_deref().unwrap_or("(not said)")
        )),
    }
    if let Some(model) = &answered {
        details.push(format!("Latest request answered by: {model} (session log)"));
    }
    if let Some(usage) = usage.filter(|u| has_spending(u)) {
        details.push(String::new());
        details.push(token_usage::details(usage));
    }
    Some((parts.join(" · "), details.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_agent::AgentPlatform;

    fn asked() -> AgentLaunchOptions {
        AgentLaunchOptions::from_settings(AgentPlatform::Claude, "sonnet", "medium")
    }

    #[test]
    fn before_the_agent_says_anything_the_line_is_marked_expected() {
        let (line, details) = line(Some(&asked()), None).unwrap();
        assert_eq!(line, "Claude · sonnet (expected) · effort medium (expected)");
        assert!(details.contains("has not said"), "{details}");
    }

    #[test]
    fn what_the_agent_says_replaces_what_was_expected() {
        let mut usage = TokenUsage {
            agent_model: Some("sonnet (Sonnet 5)".into()),
            agent_effort: Some("medium".into()),
            ..TokenUsage::default()
        };
        assert_eq!(
            line(Some(&asked()), Some(&usage)).unwrap().0,
            "Claude · sonnet (Sonnet 5) · effort medium"
        );
        // The model that answered, from the session log, is the last word.
        usage.model = Some("claude-sonnet-5".into());
        usage.total.input = 1200;
        usage.total.output = 30;
        let (line, details) = line(Some(&asked()), Some(&usage)).unwrap();
        assert_eq!(line, "Claude · claude-sonnet-5 · effort medium · Tokens: 1.2k in · 30 out");
        assert!(details.contains("answered by: claude-sonnet-5"), "{details}");
    }

    #[test]
    fn nothing_known_shows_nothing() {
        assert_eq!(line(None, None), None);
    }
}
