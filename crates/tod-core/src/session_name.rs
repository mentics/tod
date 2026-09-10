//! Human-readable names for agent chat sessions.
//!
//! People see these — in the chat window, the session list, and the agent's own
//! session picker — so they read like a label rather than an id: where the chat
//! was opened, what it is about, and when it started.

use chrono::{DateTime, Local};

/// Longest subject kept in a name before it is cut with an ellipsis.
const MAX_SUBJECT_CHARS: usize = 48;

/// Name a session, e.g. `Obligations · Ship the chat context fix · Sep 10, 2:41 PM`.
///
/// `context_key` is the agent-context key the chat was opened with (see
/// [`crate::agent_context`]), or `None` for a plain chat.
pub fn session_name(
    context_key: Option<&str>,
    subject: &str,
    started_at: DateTime<Local>,
) -> String {
    let mut parts = vec![surface_label(context_key)];
    let subject = shorten(subject);
    if !subject.is_empty() {
        parts.push(subject);
    }
    parts.push(started_at.format("%b %-d, %-I:%M %p").to_string());
    parts.join(" · ")
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
fn shorten(subject: &str) -> String {
    let collapsed = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SUBJECT_CHARS {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(MAX_SUBJECT_CHARS).collect();
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
    fn blank_subjects_are_omitted() {
        assert_eq!(
            session_name(None, "  \n ", at(9, 5)),
            "Chat · Sep 10, 9:05 AM"
        );
    }
}
