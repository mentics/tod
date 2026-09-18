//! Cursor's session store: `<config>/acp-sessions/<session-id>/store.db`, a
//! SQLite file of content-addressed blobs (`blobs(id, data)`, `id` the hex
//! SHA-256 of `data`) and a `meta` row naming the latest root blob.
//!
//! The format is Cursor's own and undocumented; this reads the part the
//! agent itself replays a session from. The root blob (protobuf) lists the
//! session's turns (field 8), each a blob holding the prompt (field 1) and
//! the reply's steps in order (field 2). A step is a blob holding one of:
//! message text (1), a tool call (2), or thinking (3), each with its text in
//! field 1. A tool call holds its tool's own message — whose field 2 is the
//! result — its call id (57), and an error raised before the tool ran (71).
//! A tool's name and arguments are read from the model-facing JSON messages
//! the root also lists (field 1), matched by call id.
//!
//! Anything this reader does not recognize is skipped, so the rest still
//! reads, and reported as a [`FormatProblem`](super::FormatProblem).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use super::{Problems, a, Transcript, TranscriptRead, tool_title};

/// The Cursor agent's config directory, resolved the way the agent resolves
/// it: `CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`.
pub(super) fn config_dir() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = set("CURSOR_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = set("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("cursor"));
    }
    let home = if cfg!(windows) {
        set("USERPROFILE").or_else(|| set("HOME"))
    } else {
        set("HOME")
    };
    home.map(|home| PathBuf::from(home).join(".cursor"))
}

/// The session's latest root blob: it changes whenever the session gains
/// anything, and reading it touches nothing else.
pub(super) fn fingerprint(config_dir: &Path, session_id: &str) -> Option<String> {
    let store = Store::open(config_dir, session_id).ok()??;
    store.latest_root(&mut Problems::default()).ok()?
}

pub(super) fn read(config_dir: &Path, session_id: &str) -> Result<Option<TranscriptRead>> {
    let Some(store) = Store::open(config_dir, session_id)? else {
        return Ok(None);
    };
    let mut problems = Problems::default();
    let transcript = read_store(&store, &mut problems)?;
    Ok(Some(TranscriptRead {
        transcript,
        problems: problems.into_vec(),
    }))
}

/// Model-facing message roles and, for each, the content parts it may hold.
const MESSAGE_PARTS: &[(&str, &[&str])] = &[
    ("system", &[]),
    ("user", &["text", "image"]),
    ("assistant", &["text", "reasoning", "redacted-reasoning", "tool-call"]),
    ("tool", &["tool-result"]),
];

fn read_store(store: &Store, problems: &mut Problems) -> Result<Transcript> {
    let mut transcript = Transcript::default();
    let Some(root_id) = store.latest_root(problems)? else {
        problems.note("a session store that names no root");
        return Ok(transcript);
    };
    let Some(root_blob) = store.blob(&root_id, "root", problems)? else {
        return Ok(transcript);
    };
    let Some(root) = parse(&root_blob, "root", problems) else {
        return Ok(transcript);
    };

    let mut tools: HashMap<String, (String, Value)> = HashMap::new();
    for id in refs(&root, 1) {
        let Some(blob) = store.blob(&id, "model message", problems)? else {
            continue;
        };
        let Ok(message) = serde_json::from_slice::<Value>(&blob) else {
            problems.note("a model message that is not JSON");
            continue;
        };
        note_message_shape(&message, problems);
        for part in message.get("content").and_then(Value::as_array).into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) != Some("tool-call") {
                continue;
            }
            let (Some(call_id), Some(name)) = (
                part.get("toolCallId").and_then(Value::as_str),
                part.get("toolName").and_then(Value::as_str),
            ) else {
                problems.note("a model tool call with no toolCallId or toolName");
                continue;
            };
            let args = part.get("args").cloned().unwrap_or(Value::Null);
            tools.insert(call_id.to_string(), (name.to_string(), args));
        }
    }

    for id in refs(&root, 8) {
        let Some(turn_blob) = store.blob(&id, "turn", problems)? else {
            continue;
        };
        let Some(turn) = parse(&turn_blob, "turn", problems) else {
            continue;
        };
        let Some(turn) = sub_message(&turn, 1, "turn", problems) else {
            problems.note("a turn with no body");
            continue;
        };
        match refs(&turn, 1).next() {
            Some(prompt_id) => {
                if let Some(prompt_blob) = store.blob(&prompt_id, "prompt", problems)?
                    && let Some(prompt) = parse(&prompt_blob, "prompt", problems)
                {
                    match text_at(&prompt, 1) {
                        Some(text) => transcript.push_prompt(&text),
                        None => problems.note("a prompt with no text"),
                    }
                }
            }
            None => problems.note("a turn with no prompt"),
        }
        for step_id in refs(&turn, 2) {
            let Some(step_blob) = store.blob(&step_id, "step", problems)? else {
                continue;
            };
            let Some(step) = parse(&step_blob, "step", problems) else {
                continue;
            };
            push_step(&mut transcript, &step, &tools, problems);
        }
    }
    Ok(transcript)
}

/// Note what in a model-facing message this reader does not know.
fn note_message_shape(message: &Value, problems: &mut Problems) {
    let role = message.get("role").and_then(Value::as_str).unwrap_or_default();
    let Some((_, known_parts)) = MESSAGE_PARTS.iter().find(|(known, _)| *known == role) else {
        problems.note(format!("a model message with an unknown role \"{role}\""));
        return;
    };
    match message.get("content") {
        Some(Value::String(_)) => {}
        Some(Value::Array(parts)) => {
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
                if !known_parts.contains(&kind) {
                    problems.note(format!("an unknown {role} message part \"{kind}\""));
                }
            }
        }
        _ => problems.note(format!("{} message whose content is neither text nor parts", a(role))),
    }
}

fn push_step(
    transcript: &mut Transcript,
    step: &[(u32, Field<'_>)],
    tools: &HashMap<String, (String, Value)>,
    problems: &mut Problems,
) {
    let Some((kind, _)) = step.first() else {
        problems.note("an empty step");
        return;
    };
    match kind {
        1 | 3 => {
            let (thought, what) = if *kind == 3 {
                (true, "thinking step")
            } else {
                (false, "message step")
            };
            let Some(body) = sub_message(step, *kind, what, problems) else {
                return;
            };
            match text_at(&body, 1) {
                Some(text) => transcript.push_block(thought, &text),
                None => problems.note(format!("a {what} with no text")),
            }
        }
        2 => {
            let Some(call) = sub_message(step, 2, "tool call", problems) else {
                return;
            };
            let Some(call_id) = text_at(&call, CALL_ID) else {
                problems.note("a tool call with no call id");
                return;
            };
            let status = tool_status(&call, problems);
            let title = match tools.get(&call_id) {
                Some((name, args)) => tool_title(name, args),
                // Seen in sessions whose model messages were cut back.
                None => "Tool call".to_string(),
            };
            transcript.push_tool(&call_id, &title, status);
        }
        other => problems.note(format!("an unknown step kind {other}")),
    }
}

/// A tool call's fields: the tool's own message (a field below this), the
/// call id, and an error raised before the tool ran.
const CALL_ID: u32 = 57;
const CALL_ERROR: u32 = 71;

/// A tool call's status from its result: the call holds a single message
/// for its tool, whose field 2 is the result.
fn tool_status(call: &[(u32, Field<'_>)], problems: &mut Problems) -> &'static str {
    let tool = call.iter().find_map(|(number, field)| match field {
        Field::Bytes(bytes) if *number < CALL_ID => Some(*bytes),
        _ => None,
    });
    let Some(tool) = tool else {
        if call.iter().any(|(number, _)| *number == CALL_ERROR) {
            return "failed";
        }
        problems.note("a tool call with neither a tool nor an error");
        return "pending";
    };
    let Some(tool) = parse(tool, "tool", problems) else {
        return "pending";
    };
    // No result yet: the run stopped while the tool was running.
    let Some(result) = sub_message(&tool, 2, "tool result", problems) else {
        return "pending";
    };
    // 1 success, 4 a terminal left running; 2 error, 6 rejected by the
    // user, 7 a file to delete not found. 102 rides along with the others.
    let outcomes: Vec<u32> = result
        .iter()
        .map(|(number, _)| *number)
        .filter(|number| *number != 102)
        .collect();
    match outcomes.as_slice() {
        [1 | 4] => "completed",
        [2 | 6 | 7] => "failed",
        other => {
            problems.note(format!("a tool result of unknown kind {other:?}"));
            "pending"
        }
    }
}

/// Parse a blob as protobuf, noting it when it is not.
fn parse<'a>(bytes: &'a [u8], what: &str, problems: &mut Problems) -> Option<Vec<(u32, Field<'a>)>> {
    match fields(bytes) {
        Ok(fields) => Some(fields),
        Err(_) => {
            problems.note(format!("a {what} that is not protobuf"));
            None
        }
    }
}

/// The sub-message at field `number`, if the message has one; noted when
/// it is there but not protobuf.
fn sub_message<'a>(
    message: &[(u32, Field<'a>)],
    number: u32,
    what: &str,
    problems: &mut Problems,
) -> Option<Vec<(u32, Field<'a>)>> {
    let bytes = message.iter().find_map(|(n, field)| match field {
        Field::Bytes(bytes) if *n == number => Some(*bytes),
        _ => None,
    })?;
    parse(bytes, what, problems)
}

struct Store {
    conn: Connection,
}

impl Store {
    /// Open the session's store read-only; `None` when it has none (a
    /// session that was never prompted has only its `meta.json`).
    fn open(config_dir: &Path, session_id: &str) -> Result<Option<Self>> {
        let path = config_dir
            .join("acp-sessions")
            .join(session_id)
            .join("store.db");
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        Ok(Some(Self { conn }))
    }

    /// The root the `meta` table names, if it names one.
    fn latest_root(&self, problems: &mut Problems) -> Result<Option<String>> {
        let mut statement = self.conn.prepare("SELECT value FROM meta")?;
        let values = statement.query_map([], |row| row.get::<_, String>(0))?;
        for value in values {
            let meta = hex_decode(&value?).and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            let Some(meta) = meta else {
                problems.note("a meta value that is not hex-encoded JSON");
                continue;
            };
            if let Some(root) = meta.get("latestRootBlobId").and_then(Value::as_str) {
                return Ok(Some(root.to_string()));
            }
        }
        Ok(None)
    }

    /// Blob `id`; noted when the store does not have it.
    fn blob(&self, id: &str, what: &str, problems: &mut Problems) -> Result<Option<Vec<u8>>> {
        let blob = self
            .conn
            .query_row("SELECT data FROM blobs WHERE id = ?1", [id], |row| row.get(0))
            .optional()?;
        if blob.is_none() {
            problems.note(format!("a {what} blob the session refers to but does not hold"));
        }
        Ok(blob)
    }
}

fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One protobuf field's value. Fixed-width values are kept only so they can
/// be stepped over.
enum Field<'a> {
    Varint,
    Bytes(&'a [u8]),
    Fixed,
}

/// A protobuf message's fields, in order, with their numbers.
fn fields(buf: &[u8]) -> Result<Vec<(u32, Field<'_>)>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < buf.len() {
        let key = varint(buf, &mut at)?;
        let number = u32::try_from(key >> 3).context("field number out of range")?;
        let field = match key & 7 {
            0 => {
                varint(buf, &mut at)?;
                Field::Varint
            }
            1 | 5 => {
                let width = if key & 7 == 1 { 8 } else { 4 };
                at = at.checked_add(width).filter(|end| *end <= buf.len()).context("truncated")?;
                Field::Fixed
            }
            2 => {
                let len = usize::try_from(varint(buf, &mut at)?)?;
                let end = at.checked_add(len).filter(|end| *end <= buf.len()).context("truncated")?;
                let bytes = &buf[at..end];
                at = end;
                Field::Bytes(bytes)
            }
            wire => return Err(anyhow!("unknown wire type {wire}")),
        };
        out.push((number, field));
    }
    Ok(out)
}

fn varint(buf: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(*at).context("truncated")?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(anyhow!("varint too long"))
}

/// The blob ids a message lists under field `number`, as hex.
fn refs<'a>(message: &'a [(u32, Field<'a>)], number: u32) -> impl Iterator<Item = String> + 'a {
    message.iter().filter_map(move |(n, field)| match field {
        Field::Bytes(bytes) if *n == number && bytes.len() == 32 => Some(hex_encode(bytes)),
        _ => None,
    })
}

/// The text at field `number`, if the message has it.
fn text_at(message: &[(u32, Field<'_>)], number: u32) -> Option<String> {
    message.iter().find_map(|(n, field)| match field {
        Field::Bytes(bytes) if *n == number => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::ReplyPart;
    use crate::transcript::TranscriptTurn;

    /// Builds a session store the way Cursor lays one out.
    struct Builder {
        conn: Connection,
    }

    fn put_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn bytes_field(number: u32, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, u64::from(number) << 3 | 2);
        put_varint(&mut out, bytes.len() as u64);
        out.extend_from_slice(bytes);
        out
    }

    fn varint_field(number: u32, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, u64::from(number) << 3);
        put_varint(&mut out, value);
        out
    }

    impl Builder {
        fn new(path: &Path) -> Self {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let conn = Connection::open(path).unwrap();
            conn.execute_batch(
                "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
            )
            .unwrap();
            Self { conn }
        }

        /// Store `data`, returning its raw 32-byte id.
        fn put(&self, data: &[u8]) -> Vec<u8> {
            let count: i64 = self
                .conn
                .query_row("SELECT count(*) FROM blobs", [], |row| row.get(0))
                .unwrap();
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(count as u64 + 1).to_be_bytes());
            self.conn
                .execute(
                    "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                    rusqlite::params![hex_encode(&id), data],
                )
                .unwrap();
            id.to_vec()
        }

        fn set_root(&self, root: &[u8]) {
            let meta = serde_json::json!({ "agentId": "s1", "latestRootBlobId": hex_encode(root) });
            let hex = hex_encode(meta.to_string().as_bytes());
            self.conn
                .execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('0', ?1)", [hex])
                .unwrap();
        }
    }

    fn tool_step(call_id: &str, tool: u32, result: Option<u32>) -> Vec<u8> {
        let mut body = bytes_field(1, &bytes_field(1, b"args"));
        if let Some(outcome) = result {
            body.extend(bytes_field(2, &bytes_field(outcome, &bytes_field(1, b"out"))));
        }
        let mut call = bytes_field(tool, &body);
        call.extend(bytes_field(57, call_id.as_bytes()));
        call.extend(varint_field(59, 1_787_843_627_927));
        bytes_field(2, &call)
    }

    #[test]
    fn a_session_store_reads_as_turns() {
        let dir = std::env::temp_dir().join(format!("tod-cursor-transcript-{}", uuid::Uuid::new_v4()));
        let store = Builder::new(&dir.join("acp-sessions").join("s1").join("store.db"));

        let call = serde_json::json!({
            "role": "assistant",
            "content": [
                { "type": "reasoning", "text": "", "signature": "x" },
                { "type": "tool-call", "toolCallId": "c1", "toolName": "Read", "args": { "path": "a.rs" } },
                { "type": "tool-call", "toolCallId": "c2", "toolName": "Shell", "args": { "command": "cargo check" } },
            ],
        });
        let message = store.put(call.to_string().as_bytes());

        let prompt = store.put(&bytes_field(1, b"Why does it fail?"));
        let mut thinking = bytes_field(1, b"Check the file.");
        thinking.extend(varint_field(2, 758));
        let steps = [
            store.put(&bytes_field(3, &thinking)),
            store.put(&tool_step("c1", 8, Some(1))),
            store.put(&tool_step("c2", 1, Some(2))),
            store.put(&tool_step("c3", 4, None)),
            store.put(&bytes_field(1, &bytes_field(1, b"A renamed call."))),
            store.put(&bytes_field(1, &bytes_field(1, b"Only on macOS."))),
        ];
        let mut turn = bytes_field(1, &prompt);
        for step in &steps {
            turn.extend(bytes_field(2, step));
        }
        turn.extend(bytes_field(3, b"request-id"));
        let turn = store.put(&bytes_field(1, &turn));

        let mut root = bytes_field(1, &message);
        root.extend(bytes_field(8, &turn));
        root.extend(bytes_field(22, b"cli"));
        let root = store.put(&root);
        store.set_root(&root);
        drop(store);

        let read = read(&dir, "s1").unwrap().unwrap();
        let fingerprint = fingerprint(&dir, "s1");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read.problems, vec![]);
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
                            text: "Check the file.".into()
                        },
                        tool("c1", "Read a.rs", "completed"),
                        tool("c2", "`cargo check`", "failed"),
                        tool("c3", "Tool call", "pending"),
                        ReplyPart::Text {
                            text: "A renamed call.\n\nOnly on macOS.".into()
                        },
                    ]
                },
            ]
        );
        assert_eq!(fingerprint, Some(hex_encode(&root)));
    }

    #[test]
    fn what_the_reader_does_not_know_is_skipped_and_reported() {
        let dir = std::env::temp_dir().join(format!("tod-cursor-transcript-{}", uuid::Uuid::new_v4()));
        let store = Builder::new(&dir.join("acp-sessions").join("s1").join("store.db"));

        let message = serde_json::json!({
            "role": "assistant",
            "content": [{ "type": "hologram" }, { "type": "text", "text": "x" }],
        });
        let message = store.put(message.to_string().as_bytes());
        let not_json = store.put(b"");

        let prompt = store.put(&bytes_field(1, b"Hi."));
        let steps = [
            store.put(&bytes_field(9, b"a step of a kind not known here")),
            store.put(&tool_step("c1", 8, Some(5))),
            store.put(&bytes_field(1, &bytes_field(1, b"Hello."))),
            store.put(&bytes_field(9, b"another")),
        ];
        let mut turn = bytes_field(1, &prompt);
        for step in &steps {
            turn.extend(bytes_field(2, step));
        }
        turn.extend(bytes_field(2, &[7u8; 32]));
        let turn = store.put(&bytes_field(1, &turn));

        let mut root = bytes_field(1, &message);
        root.extend(bytes_field(1, &not_json));
        root.extend(bytes_field(8, &turn));
        let root = store.put(&root);
        store.set_root(&root);
        drop(store);

        let read = read(&dir, "s1").unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            read.problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "a model message that is not JSON",
                "a step blob the session refers to but does not hold",
                "a tool result of unknown kind [5]",
                "an unknown assistant message part \"hologram\"",
                "an unknown step kind 9 (2 times)",
            ]
        );
        assert_eq!(read.transcript.turns.len(), 2, "the rest still reads");
        assert_eq!(read.transcript.turns[1].text(), "Hello.");
    }

    #[test]
    fn a_session_never_prompted_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!("tod-cursor-transcript-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("acp-sessions").join("s1")).unwrap();
        std::fs::write(dir.join("acp-sessions").join("s1").join("meta.json"), "{}").unwrap();
        assert!(read(&dir, "s1").unwrap().is_none());
        assert_eq!(fingerprint(&dir, "s1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
