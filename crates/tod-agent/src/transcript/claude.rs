//! Claude Code's session log: `<config>/projects/<project>/<session-id>.jsonl`,
//! one record per line. Prompts, replies and tool results are `user` and
//! `assistant` records carrying an API message; everything else (titles,
//! file snapshots, attachments, modes) is bookkeeping and skipped, as are
//! subagents' records and the ones Claude Code writes on the user's behalf.

use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context;
use serde_json::Value;

use super::{Problems, a, Transcript, TranscriptRead, tool_title};
use crate::run_state::find_claude_session_log;

/// Record types that carry no part of the conversation.
const BOOKKEEPING: &[&str] = &[
    "agent-name",
    "ai-title",
    "atis-latch",
    "attachment",
    "bridge-session",
    "cost-state",
    "custom-title",
    "file-history-delta",
    "file-history-snapshot",
    "last-prompt",
    "mode",
    "permission-mode",
    "progress",
    "queue-operation",
    "summary",
    "system",
];

pub(super) fn read(config_dir: &Path, session_id: &str) -> anyhow::Result<Option<TranscriptRead>> {
    let Some(path) = find_claude_session_log(config_dir, session_id) else {
        return Ok(None);
    };
    let file =
        std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut transcript = Transcript::default();
    let mut problems = Problems::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        let complete = line.ends_with(b"\n");
        let text = String::from_utf8_lossy(&line);
        if text.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(record) => push_record(&mut transcript, &mut problems, &record),
            // A last line without its newline is still being written.
            Err(_) if !complete => {}
            Err(_) => problems.note("a line that is not JSON"),
        }
    }
    Ok(Some(TranscriptRead {
        transcript,
        problems: problems.into_vec(),
    }))
}

fn push_record(transcript: &mut Transcript, problems: &mut Problems, record: &Value) {
    let flag = |key: &str| record.get(key).and_then(Value::as_bool).unwrap_or(false);
    let Some(kind) = record.get("type").and_then(Value::as_str) else {
        problems.note("a record with no type");
        return;
    };
    if kind != "user" && kind != "assistant" {
        if !BOOKKEEPING.contains(&kind) {
            problems.note(format!("an unknown record type \"{kind}\""));
        }
        return;
    }
    if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
        return;
    }
    let Some(content) = record.pointer("/message/content") else {
        problems.note(format!("{} record with no message content", a(kind)));
        return;
    };
    let blocks = match content {
        Value::String(text) if kind == "user" => {
            transcript.push_prompt(text);
            return;
        }
        Value::Array(blocks) => blocks,
        _ => {
            problems.note(format!("{} message whose content is not a list of blocks", a(kind)));
            return;
        }
    };
    for block in blocks {
        let block_kind = block.get("type").and_then(Value::as_str).unwrap_or_default();
        match (kind, block_kind) {
            ("user", "text") => transcript.push_prompt(str_at(block, "text")),
            ("user", "image" | "document") => {}
            ("user", "tool_result") => {
                let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                    problems.note("a tool result with no tool_use_id");
                    continue;
                };
                let failed = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                transcript.push_tool(id, "", if failed { "failed" } else { "completed" });
            }
            ("assistant", "text") => transcript.push_block(false, str_at(block, "text")),
            ("assistant", "thinking") => transcript.push_block(true, str_at(block, "thinking")),
            ("assistant", "redacted_thinking") => {}
            ("assistant", "tool_use") => {
                let (Some(id), Some(name)) = (
                    block.get("id").and_then(Value::as_str),
                    block.get("name").and_then(Value::as_str),
                ) else {
                    problems.note("a tool call with no id or name");
                    continue;
                };
                let args = block.get("input").cloned().unwrap_or(Value::Null);
                transcript.push_tool(id, &tool_title(name, &args), "pending");
            }
            _ => problems.note(format!("an unknown {kind} content block \"{block_kind}\"")),
        }
    }
}

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::ReplyPart;
    use crate::transcript::TranscriptTurn;

    #[test]
    fn a_session_log_reads_as_turns() {
        let dir = std::env::temp_dir().join(format!("tod-claude-transcript-{}", uuid::Uuid::new_v4()));
        let project = dir.join("projects").join("C--work-repo");
        std::fs::create_dir_all(&project).unwrap();
        let lines = [
            r#"{"type":"custom-title","customTitle":"Chat"}"#,
            r#"{"type":"user","message":{"role":"user","content":"Why does it fail?"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<caveat>"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"x"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Check the log."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"ci.log"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"..."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo check\n--all"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"exit 1"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"A renamed call."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Only on macOS."}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix it."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t3","name":"Edit","input":{"file_path":"a.rs"}}]}}"#,
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assi",
        ];
        std::fs::write(project.join("s1.jsonl"), lines.join("\n")).unwrap();

        let read = read(&dir, "s1").unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read.problems, vec![], "a line still being written is not one");
        let transcript = read.transcript;

        let tool = |id: &str, title: &str, status: &str| ReplyPart::Tool {
            id: id.into(),
            title: title.into(),
            status: status.into(),
        };
        assert_eq!(
            transcript.turns,
            vec![
                TranscriptTurn::User {
                    text: "Why does it fail?".into()
                },
                TranscriptTurn::Agent {
                    parts: vec![
                        ReplyPart::Thought {
                            text: "Check the log.".into()
                        },
                        tool("t1", "Read ci.log", "completed"),
                        tool("t2", "`cargo check`", "failed"),
                        ReplyPart::Text {
                            text: "A renamed call.\n\nOnly on macOS.".into()
                        },
                    ]
                },
                TranscriptTurn::User {
                    text: "Fix it.".into()
                },
                TranscriptTurn::Agent {
                    parts: vec![tool("t3", "Edit a.rs", "pending")]
                },
            ]
        );
    }

    #[test]
    fn what_the_reader_does_not_know_is_skipped_and_reported() {
        let dir = std::env::temp_dir().join(format!("tod-claude-transcript-{}", uuid::Uuid::new_v4()));
        let project = dir.join("projects").join("C--work-repo");
        std::fs::create_dir_all(&project).unwrap();
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"Hi."}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"hologram","x":1},{"type":"text","text":"Hello."}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"hologram","x":2}]}}"#,
            r#"{"type":"telepathy","data":1}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":{"text":"x"}}}"#,
            "not json at all",
            r#"{"type":"user","message":{"role":"user","content":"Bye."}}"#,
        ];
        std::fs::write(project.join("s1.jsonl"), lines.join("\n") + "\n").unwrap();

        let read = read(&dir, "s1").unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            read.problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "a line that is not JSON",
                "an assistant message whose content is not a list of blocks",
                "an unknown assistant content block \"hologram\" (2 times)",
                "an unknown record type \"telepathy\"",
            ]
        );
        assert_eq!(read.transcript.turns.len(), 3, "the rest still reads");
        assert_eq!(read.transcript.turns[1].text(), "Hello.");
    }

    #[test]
    fn a_session_with_no_log_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!("tod-claude-transcript-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("projects").join("p")).unwrap();
        assert!(read(&dir, "missing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
