//! Human-readable names for agent chat sessions.
//!
//! People see these — in the chat window, the session list, and the agent's own
//! session picker — so they read like a label rather than an id: where the chat
//! was opened, what it is about, and when it started.

use chrono::{DateTime, Local};

/// Longest subject kept in a name before it is cut with an ellipsis.
const MAX_SUBJECT_CHARS: usize = 48;

/// Hard cap on the whole name. Every caller — chat windows, fleet runs,
/// anything else that names an agent session — must stay under this so the
/// name renders cleanly in the platform's own session picker.
const MAX_TOTAL_CHARS: usize = 100;

/// The surface label conversation-view sessions are named with
/// (`Conversation · <focus title> · <time>`).
pub const CONVERSATION_SURFACE: &str = "conversation";

/// The surface label a plain node chat is named with.
pub const CHAT_SURFACE: &str = "chat";

/// The surface label an implementation session is named with.
pub const IMPLEMENT_SURFACE: &str = "implement";

/// The surface label a verification session is named with.
pub const VERIFY_SURFACE: &str = "verify";

/// Name a session, e.g. `Obligations · Ship the chat context fix · Sep 10, 2:41 PM`.
///
/// `context_key` is the agent-context key the chat was opened with (see
/// [`crate::agent_context`]), or `None` for a plain chat. Used for every
/// surface that launches or talks to an agent (chat windows, fleet runs,
/// interview sessions, …) so names are consistent no matter how the session
/// was started. The result is always at most [`MAX_TOTAL_CHARS`] characters.
pub fn session_name(
    context_key: Option<&str>,
    subject: &str,
    started_at: DateTime<Local>,
) -> String {
    let surface = surface_label(context_key);
    let timestamp = started_at.format("%b %-d, %-I:%M %p").to_string();
    let subject = shorten(subject, MAX_SUBJECT_CHARS);

    let fixed_len = surface.chars().count()
        + " · ".chars().count()
        + timestamp.chars().count()
        + if subject.is_empty() {
            0
        } else {
            " · ".chars().count()
        };
    let subject = if fixed_len + subject.chars().count() > MAX_TOTAL_CHARS {
        let budget = MAX_TOTAL_CHARS.saturating_sub(fixed_len);
        shorten(&subject, budget)
    } else {
        subject
    };

    let mut parts = vec![surface];
    if !subject.is_empty() {
        parts.push(subject);
    }
    parts.push(timestamp);
    let name = parts.join(" · ");
    truncate_chars(&name, MAX_TOTAL_CHARS)
}

/// Last-resort truncation if the fixed parts alone somehow exceed the cap.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

/// `obligations` → `Obligations`, `tasks/edit` → `Tasks edit`.
fn surface_label(context_key: Option<&str>) -> String {
    let label = context_key
        .unwrap_or("chat")
        .split(['/', '-', '_'])
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Chat".to_string(),
    }
}

/// Collapse whitespace, and cut a long subject at a word boundary.
fn shorten(subject: &str, max_chars: usize) -> String {
    let collapsed = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    if max_chars == 0 {
        return String::new();
    }
    let cut: String = collapsed.chars().take(max_chars).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > cut.len() / 2 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 9, 10, hour, minute, 0)
            .single()
            .expect("unambiguous local time")
    }

    #[test]
    fn names_surface_subject_and_start_time() {
        assert_eq!(
            session_name(Some("obligations"), "Ship the chat context fix", at(14, 41)),
            "Obligations · Ship the chat context fix · Sep 10, 2:41 PM"
        );
    }

    #[test]
    fn plain_chats_and_nested_keys_get_readable_surfaces() {
        assert_eq!(
            session_name(None, "Fix login", at(9, 5)),
            "Chat · Fix login · Sep 10, 9:05 AM"
        );
        assert!(session_name(Some("tasks/edit"), "x", at(9, 5)).starts_with("Tasks edit · "));
    }

    #[test]
    fn conversation_sessions_are_labeled_conversation() {
        assert_eq!(
            session_name(Some(CONVERSATION_SURFACE), "Auth", at(9, 5)),
            "Conversation · Auth · Sep 10, 9:05 AM"
        );
    }

    #[test]
    fn long_subjects_are_cut_at_a_word_boundary() {
        let name = session_name(
            Some("obligations"),
            "Make the agent chat window keep its session alive between turns and resume it",
            at(9, 5),
        );
        let subject = name.split(" · ").nth(1).expect("subject part");
        assert_eq!(subject, "Make the agent chat window keep its session…");
    }

    #[test]
    fn name_never_exceeds_the_hard_cap() {
        let long_surface_key = "a-very-long-nested-surface-key-that-eats-into-the-budget";
        let long_subject = "An extremely long task title that on its own would already blow the character budget for a session name";
        let name = session_name(Some(long_surface_key), long_subject, at(9, 5));
        assert!(
            name.chars().count() <= MAX_TOTAL_CHARS,
            "{name:?} ({} chars)",
            name.chars().count()
        );
        assert!(name.ends_with("Sep 10, 9:05 AM"));
    }

    #[test]
    fn blank_subjects_are_omitted() {
        assert_eq!(
            session_name(None, "  \n ", at(9, 5)),
            "Chat · Sep 10, 9:05 AM"
        );
    }
}
