//! A session turn's reply as the agent streamed it: text, thoughts, and tool
//! calls, in order.
//!
//! The provider's `Success` text is only the agent's message text, run
//! together. A viewer that wants to tell the answer apart from the narration
//! and the work in between reads the parts instead
//! ([`crate::AgentProvider::session_reply_parts`]).

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// One piece of a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyPart {
    /// Message text the agent wrote.
    Text { text: String },
    /// The agent's reasoning.
    Thought { text: String },
    /// A tool call; later updates to the same call replace its title and
    /// status in place.
    Tool {
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        status: String,
    },
}

/// The parts of the turn in progress, shared between the connection that
/// streams them and the provider that reports them.
pub(crate) type SharedReplyParts = Arc<Mutex<Vec<ReplyPart>>>;

/// Append streamed text of one kind, coalescing with the previous part when
/// it is the same kind.
pub(crate) fn push_text(parts: &mut Vec<ReplyPart>, thought: bool, chunk: &str) {
    match (parts.last_mut(), thought) {
        (Some(ReplyPart::Text { text }), false) | (Some(ReplyPart::Thought { text }), true) => {
            text.push_str(chunk)
        }
        _ => parts.push(if thought {
            ReplyPart::Thought {
                text: chunk.to_string(),
            }
        } else {
            ReplyPart::Text {
                text: chunk.to_string(),
            }
        }),
    }
}

/// Record a `tool_call` or `tool_call_update`. An update to a call already
/// seen changes it in place; empty fields keep what the call had.
pub(crate) fn push_tool(parts: &mut Vec<ReplyPart>, id: &str, title: &str, status: &str) {
    let existing = parts.iter_mut().rev().find_map(|part| match part {
        ReplyPart::Tool {
            id: part_id,
            title,
            status,
        } if !id.is_empty() && part_id == id => Some((title, status)),
        _ => None,
    });
    match existing {
        Some((old_title, old_status)) => {
            if !title.is_empty() {
                *old_title = title.to_string();
            }
            if !status.is_empty() {
                *old_status = status.to_string();
            }
        }
        None => parts.push(ReplyPart::Tool {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_of_one_kind_coalesce_and_tools_update_in_place() {
        let mut parts = Vec::new();
        push_text(&mut parts, false, "Let me ");
        push_text(&mut parts, false, "look.");
        push_text(&mut parts, true, "Hmm");
        push_tool(&mut parts, "t1", "Read file", "pending");
        push_tool(&mut parts, "t1", "", "completed");
        push_text(&mut parts, false, "Done.");
        assert_eq!(
            parts,
            vec![
                ReplyPart::Text {
                    text: "Let me look.".into()
                },
                ReplyPart::Thought { text: "Hmm".into() },
                ReplyPart::Tool {
                    id: "t1".into(),
                    title: "Read file".into(),
                    status: "completed".into()
                },
                ReplyPart::Text {
                    text: "Done.".into()
                },
            ]
        );
    }
}
