//! A session's history, read after the fact from where its platform keeps
//! it: each prompt, and each reply as its parts — text, thoughts, tool calls —
//! the same shape a live turn streams ([`ReplyPart`]).
//!
//! Reading one is an administrative read of the platform's own files: no
//! agent is started, nothing is sent, and nothing is written. Claude Code
//! keeps a log per session ([`claude`]); Cursor keeps a store per session
//! ([`cursor`]). Thinking text is often not kept — only that a thought
//! happened — and an empty thought is dropped.

mod claude;
mod cursor;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::platform::AgentPlatform;
use crate::reply::{self, ReplyPart};

/// A transcript as read, and what in the platform's record this reader did
/// not expect. The formats are the platforms' own and change without notice:
/// anything unexpected is skipped so the rest still reads, and reported so
/// the reader can be brought up to date.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptRead {
    pub transcript: Transcript,
    pub problems: Vec<FormatProblem>,
}

/// One kind of thing a reader did not expect, and how often it met it in
/// one session. `what` names the kind — never the session or where in it —
/// so the same change in format reads the same across sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatProblem {
    pub what: String,
    pub count: usize,
}

impl std::fmt::Display for FormatProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.count {
            1 => write!(f, "{}", self.what),
            n => write!(f, "{} ({n} times)", self.what),
        }
    }
}

/// Collects a read's problems, counting repeats of the same kind.
#[derive(Default)]
struct Problems(std::collections::BTreeMap<String, usize>);

impl Problems {
    fn note(&mut self, what: impl Into<String>) {
        *self.0.entry(what.into()).or_default() += 1;
    }

    fn into_vec(self) -> Vec<FormatProblem> {
        self.0
            .into_iter()
            .map(|(what, count)| FormatProblem { what, count })
            .collect()
    }
}

/// `word` with its indefinite article, for a problem's description.
fn a(word: &str) -> String {
    let article = if word.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
    format!("{article} {word}")
}

/// Read session `agent_session_id`'s transcript. `Ok(None)` when the
/// platform has no record of the session (never prompted, or its record has
/// been cleaned up); `Err` only when the record cannot be opened at all.
pub fn read_transcript(
    platform: AgentPlatform,
    agent_session_id: &str,
) -> anyhow::Result<Option<TranscriptRead>> {
    match platform {
        AgentPlatform::Claude => match crate::run_state::claude_config_dir() {
            Some(dir) => claude::read(&dir, agent_session_id),
            None => Ok(None),
        },
        AgentPlatform::Cursor => match cursor::config_dir() {
            Some(dir) => cursor::read(&dir, agent_session_id),
            None => Ok(None),
        },
    }
}

/// A cheap mark of how far a session's history has got — its latest
/// message — to tell whether a stored transcript is still current without
/// reading it again. Take it before the read: history added during the read
/// then reads as new. `None` when there is no record to check.
pub fn transcript_fingerprint(platform: AgentPlatform, agent_session_id: &str) -> Option<String> {
    match platform {
        AgentPlatform::Claude => crate::run_state::claude_transcript_fingerprint(agent_session_id),
        AgentPlatform::Cursor => cursor::fingerprint(&cursor::config_dir()?, agent_session_id),
    }
}

/// A tool call's title from its name and arguments: what it acted on, where
/// the arguments say.
fn tool_title(name: &str, args: &Value) -> String {
    let arg = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| args.get(key).and_then(Value::as_str))
            .map(|value| value.lines().next().unwrap_or_default().trim().to_string())
            .filter(|value| !value.is_empty())
    };
    if let Some(command) = arg(&["command"]) {
        return format!("`{command}`");
    }
    if let Some(pattern) = arg(&["glob_pattern"]) {
        return format!("Find `{pattern}`");
    }
    if let Some(pattern) = arg(&["pattern"]) {
        return format!("{name} `{pattern}`");
    }
    if let Some(target) = arg(&["file_path", "path", "target_file", "notebook_path", "url"]) {
        return format!("{name} {target}");
    }
    match arg(&["description", "query", "prompt"]) {
        Some(what) => format!("{name}: {what}"),
        None => name.to_string(),
    }
}

/// Bumped when the stored shape changes; a stored transcript of any other
/// version (or the plain text stored before there was one) reads as absent,
/// so it is read again.
const VERSION: u32 = 1;

/// One turn of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum TranscriptTurn {
    /// What was sent to the agent.
    User { text: String },
    /// Everything the agent did in answer, in order.
    Agent { parts: Vec<ReplyPart> },
}

impl TranscriptTurn {
    /// The turn's text: a prompt, or a reply's message text, a paragraph
    /// for each stretch the agent wrote between its other work.
    pub fn text(&self) -> String {
        match self {
            Self::User { text } => text.clone(),
            Self::Agent { parts } => parts
                .iter()
                .filter_map(|part| match part {
                    ReplyPart::Text { text } if !text.trim().is_empty() => Some(text.trim()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}

/// A session's history, oldest turn first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub turns: Vec<TranscriptTurn>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    turns: Vec<TranscriptTurn>,
}

impl Transcript {
    /// The form kept in the database.
    pub fn to_stored(&self) -> String {
        serde_json::to_string(&Stored {
            version: VERSION,
            turns: self.turns.clone(),
        })
        .unwrap_or_default()
    }

    /// Read a stored transcript; `None` for anything this version did not
    /// write, which the caller treats as not read yet.
    pub fn from_stored(stored: &str) -> Option<Self> {
        let stored: Stored = serde_json::from_str(stored).ok()?;
        (stored.version == VERSION).then_some(Self {
            turns: stored.turns,
        })
    }

    /// Plain text, for a view or a clipboard that has no use for the parts.
    pub fn to_text(&self) -> String {
        self.turns
            .iter()
            .map(|turn| {
                let label = match turn {
                    TranscriptTurn::User { .. } => "User",
                    TranscriptTurn::Agent { .. } => "Assistant",
                };
                format!("{label}:\n{}", turn.text())
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub(crate) fn push_user(&mut self, chunk: &str) {
        match self.turns.last_mut() {
            Some(TranscriptTurn::User { text }) => text.push_str(chunk),
            _ => self.turns.push(TranscriptTurn::User {
                text: chunk.to_string(),
            }),
        }
    }

    /// A whole prompt; one that follows another in the same turn starts a
    /// new paragraph.
    pub(crate) fn push_prompt(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if matches!(self.turns.last(), Some(TranscriptTurn::User { .. })) {
            self.push_user("\n\n");
        }
        self.push_user(text);
    }

    pub(crate) fn push_text(&mut self, thought: bool, chunk: &str) {
        reply::push_text(self.agent_parts(), thought, chunk);
    }

    /// A whole block of text or thought; one that follows another of the
    /// same kind starts a new paragraph.
    pub(crate) fn push_block(&mut self, thought: bool, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let continues = match self.turns.last() {
            Some(TranscriptTurn::Agent { parts }) => matches!(
                (parts.last(), thought),
                (Some(ReplyPart::Text { .. }), false) | (Some(ReplyPart::Thought { .. }), true)
            ),
            _ => false,
        };
        if continues {
            self.push_text(thought, "\n\n");
        }
        self.push_text(thought, text);
    }

    pub(crate) fn push_tool(&mut self, id: &str, title: &str, status: &str) {
        reply::push_tool(self.agent_parts(), id, title, status);
    }

    fn agent_parts(&mut self) -> &mut Vec<ReplyPart> {
        if !matches!(self.turns.last(), Some(TranscriptTurn::Agent { .. })) {
            self.turns.push(TranscriptTurn::Agent { parts: Vec::new() });
        }
        match self.turns.last_mut() {
            Some(TranscriptTurn::Agent { parts }) => parts,
            _ => unreachable!("an agent turn was just pushed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_group_into_turns() {
        let mut transcript = Transcript::default();
        transcript.push_user("Find ");
        transcript.push_user("it.");
        transcript.push_text(true, "Where is it?");
        transcript.push_tool("t1", "Read a.rs", "pending");
        transcript.push_tool("t1", "", "completed");
        transcript.push_text(false, "Found it.");
        transcript.push_user("Thanks.");

        assert_eq!(
            transcript.turns,
            vec![
                TranscriptTurn::User {
                    text: "Find it.".into()
                },
                TranscriptTurn::Agent {
                    parts: vec![
                        ReplyPart::Thought {
                            text: "Where is it?".into()
                        },
                        ReplyPart::Tool {
                            id: "t1".into(),
                            title: "Read a.rs".into(),
                            status: "completed".into(),
                        },
                        ReplyPart::Text {
                            text: "Found it.".into()
                        },
                    ]
                },
                TranscriptTurn::User {
                    text: "Thanks.".into()
                },
            ]
        );
    }

    #[test]
    fn a_reply_reads_as_a_paragraph_per_stretch_of_writing() {
        let mut transcript = Transcript::default();
        transcript.push_text(false, "Looking.");
        transcript.push_tool("t1", "Read a.rs", "completed");
        transcript.push_text(false, "Found it.");
        assert_eq!(transcript.turns[0].text(), "Looking.\n\nFound it.");
    }

    #[test]
    fn an_empty_thought_leaves_no_part() {
        let mut transcript = Transcript::default();
        transcript.push_text(true, "");
        transcript.push_text(false, "Done.");
        assert_eq!(
            transcript.turns,
            vec![TranscriptTurn::Agent {
                parts: vec![ReplyPart::Text {
                    text: "Done.".into()
                }]
            }]
        );
    }

    #[test]
    fn it_round_trips_and_older_forms_read_as_absent() {
        let mut transcript = Transcript::default();
        transcript.push_user("Hi.");
        transcript.push_text(false, "Hello.");
        assert_eq!(
            Transcript::from_stored(&transcript.to_stored()),
            Some(transcript)
        );
        assert_eq!(
            Transcript::from_stored("User:\nHi.\n\nAssistant:\nHello."),
            None
        );
        assert_eq!(
            Transcript::from_stored(r#"{"version":0,"turns":[]}"#),
            None
        );
    }
}
