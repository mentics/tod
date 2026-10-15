//! The problem channel: one JSON line per failure, appended by any tod
//! executable to `<data root>/problems/<process>.jsonl`, and tailed by the app
//! into its log panel (and, for errors, a toast).
//!
//! A line is `{"time": <unix seconds>, "level": "warn"|"error", "process":
//! <name>, "message": <text>, "label": <optional node/sandbox>}`. Reporting
//! never replaces the executable's own stderr output, and never fails
//! quietly: [`report`] returns the I/O error for the caller to print.
//!
//! Each file is bounded: past [`MAX_FILE_BYTES`] it is renamed to
//! `<process>.jsonl.1` (replacing the previous one) and a new file starts.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A file is rotated once it is this big.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// Subdirectory of the data root.
pub const DIR: &str = "problems";
/// The environment variable executables started by the app inherit.
pub const DATA_ROOT_ENV: &str = "TOD_DATA_ROOT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub time: u64,
    pub level: Level,
    pub process: String,
    pub message: String,
    /// The node or sandbox it concerns, when the reporter knows.
    pub label: Option<String>,
}

pub fn dir(data_root: &Path) -> PathBuf {
    data_root.join(DIR)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append a problem for `process` under `<data root>/problems`. The file
/// name comes from `process`, so it is reduced to safe characters.
pub fn report(
    data_root: &Path,
    process: &str,
    level: Level,
    label: Option<&str>,
    message: &str,
) -> std::io::Result<()> {
    write_line(&dir(data_root), process, level, label, message)
}

fn safe_name(process: &str) -> String {
    process
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn write_line(dir: &Path, process: &str, level: Level, label: Option<&str>, message: &str) -> std::io::Result<()> {
    let p = Problem {
        time: now_secs(),
        level,
        process: process.to_string(),
        message: message.to_string(),
        label: label.map(str::to_string),
    };
    write_json(dir, &safe_name(process), &to_json(&p))
}

fn write_json(dir: &Path, name: &str, line: &serde_json::Value) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.jsonl", safe_name(name)));
    if std::fs::metadata(&path).map(|m| m.len() >= MAX_FILE_BYTES).unwrap_or(false) {
        std::fs::rename(&path, dir.join(format!("{}.jsonl.1", safe_name(name))))?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    // One write call, so concurrent processes' lines do not interleave.
    file.write_all(format!("{line}\n").as_bytes())
}

/// [`report`] into the data root the app gave this process in
/// `TOD_DATA_ROOT`, or `explicit` when the caller has one. With neither there
/// is nowhere to report: `Ok(false)`.
pub fn report_to(
    explicit: Option<&Path>,
    process: &str,
    level: Level,
    label: Option<&str>,
    message: &str,
) -> std::io::Result<bool> {
    let root = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(DATA_ROOT_ENV).map(PathBuf::from));
    match root {
        Some(root) => report(&root, process, level, label, message).map(|()| true),
        None => Ok(false),
    }
}

/// An executable's last-resort reporter: tries, and says on stderr when it
/// could not, so a failure to report is itself visible.
pub fn report_or_warn(explicit: Option<&Path>, process: &str, level: Level, message: &str) {
    if let Err(err) = report_to(explicit, process, level, None, message) {
        eprintln!("{process}: could not record this in the problem log: {err}");
    }
}

fn parse(line: &str) -> Option<Problem> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(Problem {
        time: v.get("time")?.as_u64()?,
        level: match v.get("level")?.as_str()? {
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => return None,
        },
        process: v.get("process")?.as_str()?.to_string(),
        message: v.get("message")?.as_str()?.to_string(),
        label: v.get("label").and_then(|l| l.as_str()).map(str::to_string),
    })
}

/// How far each file in a problem directory has been read: file name to byte
/// offset. Serializable, so a reader that is not always running (a
/// supervisor, the app across syncs) keeps its place.
pub type Cursor = BTreeMap<String, u64>;

/// The cursor at the end of every file now: "from here on".
pub fn cursor_at_end(dir: &Path) -> std::io::Result<Cursor> {
    let mut cursor = Cursor::new();
    for path in files(dir)? {
        cursor.insert(name_of(&path), std::fs::metadata(&path)?.len());
    }
    Ok(cursor)
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The `*.jsonl` files in `dir`; none when it does not exist.
fn files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Whole lines appended to the files in `dir` after `cursor`, oldest first;
/// `cursor` is advanced past them. A file the cursor does not know is read
/// from its start; one shorter than its offset was rotated, and is read
/// again from its start. A partial last line waits for its newline.
pub fn read_since(dir: &Path, cursor: &mut Cursor) -> std::io::Result<Vec<Problem>> {
    let mut out = Vec::new();
    for path in files(dir)? {
        let name = name_of(&path);
        let mut file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len();
        let mut offset = cursor.get(&name).copied().unwrap_or(0);
        if len < offset {
            offset = 0;
        }
        if len > offset {
            file.seek(SeekFrom::Start(offset))?;
            let mut buf = Vec::new();
            file.take(len - offset).read_to_end(&mut buf)?;
            let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            offset += end as u64;
            out.extend(String::from_utf8_lossy(&buf[..end]).lines().filter_map(parse));
        }
        cursor.insert(name, offset);
    }
    out.sort_by_key(|p| p.time);
    Ok(out)
}

/// Follows every file under `<data root>/problems`, yielding lines appended
/// since the last poll. The first poll starts at each file's end (problems
/// from before this run stay in the files); a file that appears later is read
/// from its start.
pub struct Tail {
    dir: PathBuf,
    cursor: Option<Cursor>,
}

impl Tail {
    pub fn new(data_root: &Path) -> Self {
        Self { dir: dir(data_root), cursor: None }
    }

    /// New problems. A missing directory is not an error; anything else is.
    pub fn poll(&mut self) -> std::io::Result<Vec<Problem>> {
        let cursor = match &mut self.cursor {
            Some(c) => c,
            None => self.cursor.insert(cursor_at_end(&self.dir)?),
        };
        read_since(&self.dir, cursor)
    }
}

/// The directory remote processes (relay, supervisor, and what they start)
/// report into inside a node's sandbox: `TOD_PROBLEMS_DIR`, else
/// [`NODE_DIR`]. The supervisor forwards it to the orchestrator whenever it
/// is already talking to it ([`Outbox`]).
pub fn node_dir() -> PathBuf {
    std::env::var_os("TOD_PROBLEMS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(NODE_DIR))
}

pub const NODE_DIR: &str = "/tmp/tod-problems";

/// Report into `dir` directly (a node's [`node_dir`]), labelled.
pub fn report_in(dir: &Path, process: &str, level: Level, label: Option<&str>, message: &str) -> std::io::Result<()> {
    write_line(dir, process, level, label, message)
}

/// What has not been forwarded yet from `dir`, with its place kept in
/// `<dir>/.forwarded.json` so it survives the forwarder exiting between
/// wakes. Take with [`Outbox::pending`]; call [`Outbox::sent`] only after the
/// problems arrived, so a failed send is retried, never lost.
pub struct Outbox {
    dir: PathBuf,
    cursor: Cursor,
    next: Cursor,
}

impl Outbox {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        let cursor = match std::fs::read(dir.join(".forwarded.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!(".forwarded.json: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Cursor::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { dir: dir.to_path_buf(), next: cursor.clone(), cursor })
    }

    pub fn pending(&mut self) -> std::io::Result<Vec<Problem>> {
        self.next = self.cursor.clone();
        read_since(&self.dir, &mut self.next)
    }

    /// The problems from the last [`Self::pending`] were delivered.
    pub fn sent(&mut self) -> std::io::Result<()> {
        self.cursor = self.next.clone();
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(
            self.dir.join(".forwarded.json"),
            serde_json::to_vec(&self.cursor).map_err(std::io::Error::other)?,
        )
    }
}

/// One problem as the JSON object the wire and the files use.
pub fn to_json(p: &Problem) -> serde_json::Value {
    let mut v = serde_json::json!({
        "time": p.time, "level": p.level.as_str(), "process": p.process, "message": p.message,
    });
    if let Some(label) = &p.label {
        v["label"] = label.clone().into();
    }
    v
}

/// Parse one JSON line (or the wire's array element).
pub fn from_json(v: &serde_json::Value) -> Option<Problem> {
    parse(&v.to_string())
}

/// Append an already-formed problem to `<dir>/<file>.jsonl`: how the
/// orchestrator stores what a node forwarded.
pub fn append(dir: &Path, p: &Problem, file: &str) -> std::io::Result<()> {
    write_json(dir, file, &to_json(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tod-problems-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reported_lines_are_tailed_with_their_fields() {
        let root = temp("append");
        let mut tail = Tail::new(&root);
        assert!(tail.poll().unwrap().is_empty());
        report(&root, "tod-cli", Level::Error, Some("node-1"), "boom \"quoted\"").unwrap();
        report(&root, "tod-sandbox", Level::Warn, None, "careful").unwrap();
        let got = tail.poll().unwrap();
        assert_eq!(got.len(), 2);
        let cli = got.iter().find(|p| p.process == "tod-cli").unwrap();
        assert_eq!(cli.level, Level::Error);
        assert_eq!(cli.message, "boom \"quoted\"");
        assert_eq!(cli.label.as_deref(), Some("node-1"));
        assert!(tail.poll().unwrap().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_first_poll_skips_what_was_there_before() {
        let root = temp("before");
        report(&root, "tod-cli", Level::Error, None, "old").unwrap();
        let mut tail = Tail::new(&root);
        assert!(tail.poll().unwrap().is_empty());
        report(&root, "tod-cli", Level::Error, None, "new").unwrap();
        let got = tail.poll().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message, "new");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_full_file_rotates_and_the_tail_follows() {
        let root = temp("rotate");
        let mut tail = Tail::new(&root);
        tail.poll().unwrap();
        let big = "x".repeat(4096);
        let mut sent = 0;
        while std::fs::metadata(dir(&root).join("tod-cli.jsonl.1")).is_err() {
            report(&root, "tod-cli", Level::Warn, None, &big).unwrap();
            sent += 1;
            assert!(sent < 1000, "never rotated");
            if sent % 20 == 0 {
                tail.poll().unwrap();
            }
        }
        report(&root, "tod-cli", Level::Error, None, "after rotation").unwrap();
        let got = tail.poll().unwrap();
        assert_eq!(got.last().unwrap().message, "after rotation");
        let size = std::fs::metadata(dir(&root).join("tod-cli.jsonl")).unwrap().len();
        assert!(size < MAX_FILE_BYTES);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_partial_line_waits_for_its_newline() {
        let root = temp("partial");
        let mut tail = Tail::new(&root);
        tail.poll().unwrap();
        let path = dir(&root).join("p.jsonl");
        std::fs::create_dir_all(dir(&root)).unwrap();
        std::fs::write(&path, "{\"time\":1,\"level\":\"error\",\"process\":\"p\",\"mess").unwrap();
        assert!(tail.poll().unwrap().is_empty());
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "age\":\"whole\"}}").unwrap();
        assert_eq!(tail.poll().unwrap()[0].message, "whole");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nowhere_to_report_is_said_not_swallowed() {
        // No explicit root and (in tests) no env: Ok(false), which callers print.
        if std::env::var_os(DATA_ROOT_ENV).is_none() {
            assert!(!report_to(None, "x", Level::Error, None, "m").unwrap());
        }
    }

    #[test]
    fn an_outbox_resends_until_it_is_told_it_arrived() {
        let dir = temp("outbox").join("node");
        report_in(&dir, "tod-relay", Level::Error, None, "one").unwrap();
        let mut outbox = Outbox::open(&dir).unwrap();
        assert_eq!(outbox.pending().unwrap().len(), 1);
        // The send failed: the same problem is pending again.
        assert_eq!(outbox.pending().unwrap().len(), 1);
        outbox.sent().unwrap();
        report_in(&dir, "tod-relay", Level::Warn, None, "two").unwrap();
        // A fresh outbox (the supervisor woke again) keeps its place.
        let mut again = Outbox::open(&dir).unwrap();
        let got = again.pending().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message, "two");
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }
}
