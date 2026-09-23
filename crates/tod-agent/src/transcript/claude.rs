//! Claude Code's session log: `<config>/projects/<project>/<session-id>.jsonl`,
//! one record per line. Prompts, replies and tool results are `user` and
//! `assistant` records carrying an API message; everything else (titles,
//! file snapshots, attachments, modes) is bookkeeping and skipped, as are
//! subagents' records and the ones Claude Code writes on the user's behalf.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context;
use serde_json::Value;

use super::{Problems, a, Transcript, TranscriptRead, tool_title};
use crate::run_state::find_claude_session_log;
use crate::usage::{
    ClaudeCostState, TokenCounts, TokenUsage, anthropic_counts, apply_cost_states,
    claude_cost_state,
};

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
    "pr-link",
    "progress",
    "queue-operation",
    "summary",
    "system",
];

pub(super) fn read(config_dir: &Path, session_id: &str) -> anyhow::Result<Option<TranscriptRead>> {
    let Some(path) = find_claude_session_log(config_dir, session_id) else {
        return Ok(None);
    };
    let mut transcript = Transcript::default();
    let mut problems = Problems::default();
    let mut tally = Tally::default();
    for_each_record(&path, &mut problems, |record, problems| {
        tally.note(record, false);
        push_record(&mut transcript, problems, record);
    })?;
    // Subagents keep logs of their own beside the session's.
    let subagents = path.with_extension("").join("subagents");
    if let Ok(entries) = std::fs::read_dir(&subagents) {
        for entry in entries.flatten() {
            let log = entry.path();
            if log.extension().is_some_and(|ext| ext == "jsonl") {
                for_each_record(&log, &mut problems, |record, _| tally.note(record, true))?;
            }
        }
    }
    transcript.set_usage(tally.into_usage());
    Ok(Some(TranscriptRead {
        transcript,
        problems: problems.into_vec(),
    }))
}

/// Call `each` with every record in the log at `path`.
fn for_each_record(
    path: &Path,
    problems: &mut Problems,
    mut each: impl FnMut(&Value, &mut Problems),
) -> anyhow::Result<()> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
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
            Ok(record) => each(&record, problems),
            // A last line without its newline is still being written.
            Err(_) if !complete => {}
            Err(_) => problems.note("a line that is not JSON"),
        }
    }
    Ok(())
}

/// The usage a log's records carry. Claude Code writes an API response as
/// a record per content block, each carrying the response's usage, so a
/// response is counted once, by its message id, as its latest record says.
#[derive(Default)]
struct Tally {
    /// Message id → (model, counts, from a subagent), in first-seen order.
    responses: Vec<(String, String, TokenCounts, bool)>,
    index: HashMap<String, usize>,
    /// The latest main-chain response's whole prompt.
    context_tokens: Option<u64>,
    cost_states: Vec<ClaudeCostState>,
}

impl Tally {
    fn note(&mut self, record: &Value, subagent_log: bool) {
        match record.get("type").and_then(Value::as_str) {
            Some("assistant") => {}
            Some("cost-state") if !subagent_log => {
                self.cost_states.push(claude_cost_state(record));
                return;
            }
            _ => return,
        }
        let (Some(usage), Some(id)) = (
            record.pointer("/message/usage"),
            record.pointer("/message/id").and_then(Value::as_str),
        ) else {
            return;
        };
        let model = record
            .pointer("/message/model")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        // Claude Code's own stand-in replies (an API error, an interrupt)
        // never reached a model.
        if model == "<synthetic>" {
            return;
        }
        let sidechain = subagent_log
            || record
                .get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let counts = anthropic_counts(usage);
        if !sidechain {
            self.context_tokens = Some(counts.prompt());
        }
        let response = (id.to_string(), model.to_string(), counts, sidechain);
        match self.index.get(id) {
            Some(&at) => self.responses[at] = response,
            None => {
                self.index.insert(id.to_string(), self.responses.len());
                self.responses.push(response);
            }
        }
    }

    fn into_usage(self) -> TokenUsage {
        let mut usage = TokenUsage {
            context_tokens: self.context_tokens,
            ..TokenUsage::default()
        };
        for (_, model, counts, sidechain) in &self.responses {
            usage.total.add(counts);
            usage.by_model.entry(model.clone()).or_default().add(counts);
            if *sidechain {
                usage.subagents.add(counts);
            }
        }
        apply_cost_states(&mut usage, self.cost_states);
        usage
    }
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
    fn usage_counts_each_response_once_with_its_subagents() {
        let dir = std::env::temp_dir().join(format!("tod-claude-transcript-{}", uuid::Uuid::new_v4()));
        let project = dir.join("projects").join("C--work-repo");
        std::fs::create_dir_all(project.join("s1").join("subagents")).unwrap();
        let usage = |input, output, read, write| {
            format!(
                r#"{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":{read},"cache_creation_input_tokens":{write},"cache_creation":{{"ephemeral_1h_input_tokens":{write}}},"output_tokens_details":{{"thinking_tokens":1}},"server_tool_use":{{"web_search_requests":1}}}}"#
            )
        };
        let assistant = |id: &str, model: &str, usage: &str, block: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","role":"assistant","usage":{usage},"content":[{block}]}}}}"#
            )
        };
        let text = r#"{"type":"text","text":"x"}"#;
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"Hi."}}"#.to_string(),
            // One response, written as a record per block.
            assistant("m1", "opus", &usage(2, 5, 100, 10), r#"{"type":"thinking","thinking":"t"}"#),
            assistant("m1", "opus", &usage(2, 9, 100, 10), text),
            assistant("m2", "haiku", &usage(3, 4, 200, 0), text),
            assistant("m3", "<synthetic>", &usage(0, 0, 0, 0), text),
            r#"{"type":"cost-state","startTime":1,"totalCostUSD":0.5,"totalAPIDuration":900}"#
                .to_string(),
        ];
        std::fs::write(project.join("s1.jsonl"), lines.join("\n") + "\n").unwrap();
        std::fs::write(
            project.join("s1").join("subagents").join("agent-a.jsonl"),
            assistant("m4", "haiku", &usage(1, 1, 0, 0), text) + "\n",
        )
        .unwrap();

        let read = read(&dir, "s1").unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read.problems, vec![]);
        let usage = read.transcript.usage.unwrap();
        assert_eq!(usage.total.requests, 3);
        assert_eq!(usage.total.input, 2 + 3 + 1);
        assert_eq!(usage.total.output, 9 + 4 + 1);
        assert_eq!(usage.total.cache_read, 300);
        assert_eq!(usage.total.cache_write, 10);
        assert_eq!(usage.total.cache_write_1h, 10);
        assert_eq!(usage.total.thinking, 3);
        assert_eq!(usage.total.web_searches, 3);
        assert_eq!(usage.by_model["opus"].output, 9);
        assert_eq!(usage.by_model["haiku"].requests, 2);
        assert_eq!(usage.subagents.requests, 1);
        assert_eq!(usage.context_tokens, Some(3 + 200));
        assert_eq!(usage.cost.as_ref().map(crate::Cost::amount), Some(0.5));
        assert_eq!(usage.api_duration_ms, Some(900));
    }

    #[test]
    fn a_session_with_no_log_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!("tod-claude-transcript-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("projects").join("p")).unwrap();
        assert!(read(&dir, "missing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
