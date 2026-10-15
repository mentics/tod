//! The in-memory log the app's log panel reads: a bounded ring of everything
//! worth showing the user — WARN/ERROR from the app's own tracing (a layer,
//! [`RingLayer`]), the status bar's messages, error toasts, and the WARN/ERROR
//! lines tailed from `tod-agentd`'s log file ([`AgentdTail`]).
//!
//! No GPUI here: the UI polls [`snapshot`] / [`generation`] (a cheap atomic)
//! and renders. Nothing in this module blocks on I/O except [`AgentdTail::poll`],
//! which callers run on a background thread.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// The most entries the ring keeps.
pub const CAPACITY: usize = 1000;

/// Severity, ordered so `>= Warn` means "needs attention".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level3 {
    Info,
    Warn,
    Error,
}

impl Level3 {
    pub fn label(self) -> &'static str {
        match self {
            Level3::Info => "INFO",
            Level3::Warn => "WARN",
            Level3::Error => "ERROR",
        }
    }
    fn from_tracing(level: &Level) -> Self {
        match *level {
            Level::ERROR => Level3::Error,
            Level::WARN => Level3::Warn,
            _ => Level3::Info,
        }
    }
}

/// Which part of tod an entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSource {
    App,
    Agentd,
    Orchestrator,
    TodCli,
    Status,
    TodSandbox,
    ZedShim,
    Supervisor,
    Relay,
    /// Any other executable that reports through the problem channel.
    Other,
}

impl LogSource {
    pub fn label(self) -> &'static str {
        match self {
            LogSource::App => "app",
            LogSource::Agentd => "agentd",
            LogSource::Orchestrator => "orchestrator",
            LogSource::TodCli => "tod-cli",
            LogSource::Status => "status",
            LogSource::TodSandbox => "tod-sandbox",
            LogSource::ZedShim => "zed-shim",
            LogSource::Supervisor => "supervisor",
            LogSource::Relay => "relay",
            LogSource::Other => "other",
        }
    }

    /// The source for a process name in the problem channel.
    pub fn for_process(process: &str) -> Self {
        match process {
            "tod-cli" => LogSource::TodCli,
            "tod-sandbox" => LogSource::TodSandbox,
            "tod-zed-shim" => LogSource::ZedShim,
            "tod-agentd" => LogSource::Agentd,
            "tod-orchestrator" => LogSource::Orchestrator,
            "tod-supervisor" => LogSource::Supervisor,
            "tod-relay" => LogSource::Relay,
            _ => LogSource::Other,
        }
    }

    /// The source for a tracing target (module path).
    pub fn for_target(target: &str) -> Self {
        if target.starts_with("tod_cli") {
            LogSource::TodCli
        } else if target.starts_with("tod_sandbox")
            || target.contains("autopilot")
            || target.contains("sandbox")
            || target.contains("cloud")
        {
            LogSource::Orchestrator
        } else if target.starts_with("tod_agentd") {
            LogSource::Agentd
        } else {
            LogSource::App
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    /// Monotonic; survives `clear` so a reader can tell what is new.
    pub seq: u64,
    pub at: SystemTime,
    pub level: Level3,
    pub source: LogSource,
    pub message: String,
    /// Already shown to the user (an error toast wrote it): no second toast.
    pub notified: bool,
}

/// Which entries a view shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFilter {
    All,
    ErrorsAndWarnings,
}

impl LogFilter {
    pub fn admits(self, level: Level3) -> bool {
        match self {
            LogFilter::All => true,
            LogFilter::ErrorsAndWarnings => level >= Level3::Warn,
        }
    }
}

/// A bounded ring. Own type (not just a static) so tests get their own.
#[derive(Default)]
pub struct LogRing {
    entries: VecDeque<LogEntry>,
    next_seq: u64,
}

impl LogRing {
    pub fn push(&mut self, level: Level3, source: LogSource, message: String) {
        self.push_with(level, source, message, false);
    }

    pub fn push_with(&mut self, level: Level3, source: LogSource, message: String, notified: bool) {
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(LogEntry {
            seq: self.next_seq,
            at: SystemTime::now(),
            level,
            source,
            message,
            notified,
        });
        self.next_seq += 1;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Oldest first, only what `filter` admits.
    pub fn filtered(&self, filter: LogFilter) -> Vec<LogEntry> {
        self.entries
            .iter()
            .filter(|e| filter.admits(e.level))
            .cloned()
            .collect()
    }

    /// Warnings and errors with `seq >= since` (the unread count).
    pub fn unread_since(&self, since: u64) -> usize {
        self.entries
            .iter()
            .filter(|e| e.seq >= since && e.level >= Level3::Warn)
            .count()
    }

    /// Errors only, same rule.
    pub fn unread_errors_since(&self, since: u64) -> usize {
        self.entries
            .iter()
            .filter(|e| e.seq >= since && e.level == Level3::Error)
            .count()
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Errors with `seq >= since` that nobody has been told about.
    pub fn unnotified_errors_since(&self, since: u64) -> Vec<LogEntry> {
        self.entries
            .iter()
            .filter(|e| e.seq >= since && e.level == Level3::Error && !e.notified)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

static RING: Mutex<LogRing> = Mutex::new(LogRing {
    entries: VecDeque::new(),
    next_seq: 0,
});
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn with_ring<R>(f: impl FnOnce(&mut LogRing) -> R) -> R {
    // A panic while holding the lock leaves the ring usable.
    let mut guard = RING.lock().unwrap_or_else(|e| e.into_inner());
    let out = f(&mut guard);
    GENERATION.fetch_add(1, Ordering::Release);
    out
}

/// Record an entry from anywhere (toasts, status messages).
pub fn record(level: Level3, source: LogSource, message: impl Into<String>) {
    let message = message.into();
    with_ring(|ring| ring.push(level, source, message));
}

/// Record something the user has just been shown (an error toast): it is in
/// the log, but must not raise a toast of its own.
pub fn record_notified(level: Level3, source: LogSource, message: impl Into<String>) {
    let message = message.into();
    with_ring(|ring| ring.push_with(level, source, message, true));
}

/// Errors from `since` on that still need a toast.
pub fn unnotified_errors_since(since: u64) -> Vec<LogEntry> {
    RING.lock().unwrap_or_else(|e| e.into_inner()).unnotified_errors_since(since)
}

pub fn clear() {
    with_ring(|ring| ring.clear());
}

pub fn snapshot(filter: LogFilter) -> Vec<LogEntry> {
    RING.lock().unwrap_or_else(|e| e.into_inner()).filtered(filter)
}

/// Changes on every write: a cheap "anything new?" check.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn next_seq() -> u64 {
    RING.lock().unwrap_or_else(|e| e.into_inner()).next_seq()
}

/// (warnings and errors, errors) with `seq >= since`.
pub fn unread_since(since: u64) -> (usize, usize) {
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    (ring.unread_since(since), ring.unread_errors_since(since))
}

/// A tracing layer feeding WARN and ERROR events into the ring.
pub struct RingLayer;

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = Level3::from_tracing(meta.level());
        if level < Level3::Warn {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = format!("{}{}", visitor.message, visitor.fields);
        with_ring(|ring| ring.push(level, LogSource::for_target(meta.target()), message));
    }
}

/// Follows `daemon/agentd.log`, recording its WARN/ERROR lines. The file is
/// plain `fmt` output (`<timestamp> LEVEL target: message`); continuation
/// lines belong to the previous entry's message and are skipped.
pub struct AgentdTail {
    path: PathBuf,
    offset: u64,
    started: bool,
}

impl AgentdTail {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, started: false }
    }

    /// Read what was appended since the last call. The first call starts at
    /// the end (earlier runs' lines are not this session's). A missing file
    /// is not an error (the daemon may not have started); any other failure
    /// is returned for the caller to log.
    pub fn poll(&mut self) -> std::io::Result<usize> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if !self.started {
            self.started = true;
            self.offset = len;
            return Ok(0);
        }
        if len < self.offset {
            self.offset = 0; // truncated or replaced
        }
        if len == self.offset {
            return Ok(0);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.take(len - self.offset).read_to_end(&mut buf)?;
        // Only whole lines; keep a trailing partial one for next time.
        let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        self.offset += end as u64;
        let mut n = 0;
        for line in String::from_utf8_lossy(&buf[..end]).lines() {
            if let Some((level, text)) = parse_fmt_line(line) {
                record(level, LogSource::Agentd, text);
                n += 1;
            }
        }
        Ok(n)
    }
}

/// `2026-10-14T10:00:00Z  WARN target: message` → (Warn, "target: message").
fn parse_fmt_line(line: &str) -> Option<(Level3, String)> {
    let mut tokens = line.split_whitespace();
    let _timestamp = tokens.next()?;
    let level = match tokens.next()? {
        "WARN" => Level3::Warn,
        "ERROR" => Level3::Error,
        _ => return None,
    };
    let rest: Vec<&str> = tokens.collect();
    Some((level, rest.join(" ")))
}

/// The text an executable's problem is shown as: `[label] message`.
fn problem_text(p: &tod_problems::Problem) -> String {
    match &p.label {
        Some(label) => format!("[{label}] {}", p.message),
        None => p.message.clone(),
    }
}

/// Feed the problems `tail` finds into the ring.
pub fn drain_problems(tail: &mut tod_problems::Tail) -> std::io::Result<usize> {
    let problems = tail.poll()?;
    for p in &problems {
        let level = match p.level {
            tod_problems::Level::Warn => Level3::Warn,
            tod_problems::Level::Error => Level3::Error,
        };
        record(level, LogSource::for_process(&p.process), problem_text(p));
    }
    Ok(problems.len())
}

static PROBLEM_TAIL_STARTED: OnceLock<()> = OnceLock::new();

/// Start the thread that follows `<data root>/problems` (once), every 3 seconds.
pub fn start_problem_tail(data_root: PathBuf) {
    if PROBLEM_TAIL_STARTED.set(()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("problem-tail".into()).spawn(move || {
        let mut tail = tod_problems::Tail::new(&data_root);
        loop {
            if let Err(err) = drain_problems(&mut tail) {
                record(
                    Level3::Error,
                    LogSource::App,
                    format!("could not read the problem log in {}: {err}", data_root.display()),
                );
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
    if let Err(err) = spawned {
        record(Level3::Error, LogSource::App, format!("could not start the problem-log tail: {err}"));
    }
}

/// How long a repeated error stays folded into its earlier toast.
pub const TOAST_REPEAT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Decides which errors raise a toast: the same source and message within
/// [`TOAST_REPEAT_WINDOW`] is one toast, and the toast that does appear says
/// how many were folded into it.
#[derive(Default)]
pub struct ToastGate {
    cursor: u64,
    seen: std::collections::HashMap<(&'static str, String), std::time::Instant>,
}

impl ToastGate {
    /// Toast texts for `entries` (new errors, oldest first), at time `now`.
    pub fn texts(&mut self, entries: &[LogEntry], now: std::time::Instant) -> Vec<String> {
        self.seen.retain(|_, at| now.duration_since(*at) < TOAST_REPEAT_WINDOW);
        let mut order: Vec<(&'static str, String)> = Vec::new();
        let mut counts: std::collections::HashMap<(&'static str, String), usize> = Default::default();
        for e in entries {
            self.cursor = self.cursor.max(e.seq + 1);
            let key = (e.source.label(), e.message.clone());
            if self.seen.contains_key(&key) {
                continue; // repeat of a toast still on the books
            }
            let n = counts.entry(key.clone()).or_insert(0);
            if *n == 0 {
                order.push(key);
            }
            *n += 1;
        }
        order
            .into_iter()
            .map(|key| {
                let n = counts[&key];
                self.seen.insert(key.clone(), now);
                let first_line = key.1.lines().next().unwrap_or("").to_string();
                if n > 1 {
                    format!("{}: {first_line} (×{n})", key.0)
                } else {
                    format!("{}: {first_line}", key.0)
                }
            })
            .collect()
    }

    /// Where to read from next: pass to [`unnotified_errors_since`].
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
}

static TAIL_STARTED: OnceLock<()> = OnceLock::new();

/// Start the thread that follows the daemon's log (once). It stats the file
/// every few seconds and reads only what was appended.
pub fn start_agentd_tail(path: PathBuf) {
    if TAIL_STARTED.set(()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("agentd-log-tail".into())
        .spawn(move || {
            let mut tail = AgentdTail::new(path);
            loop {
                if let Err(err) = tail.poll() {
                    // Surfaces in the panel itself, once per failing poll.
                    record(
                        Level3::Error,
                        LogSource::App,
                        format!("could not read the tod-agentd log {}: {err}", tail.path.display()),
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });
    if let Err(err) = spawned {
        record(Level3::Error, LogSource::App, format!("could not start the tod-agentd log tail: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_bounded_and_keeps_the_newest() {
        let mut ring = LogRing::default();
        for n in 0..CAPACITY + 7 {
            ring.push(Level3::Info, LogSource::App, format!("m{n}"));
        }
        assert_eq!(ring.len(), CAPACITY);
        let all = ring.filtered(LogFilter::All);
        assert_eq!(all.last().unwrap().message, format!("m{}", CAPACITY + 6));
        assert_eq!(all.first().unwrap().message, "m7");
    }

    #[test]
    fn filter_keeps_warnings_and_errors() {
        let mut ring = LogRing::default();
        ring.push(Level3::Info, LogSource::Status, "i".into());
        ring.push(Level3::Warn, LogSource::App, "w".into());
        ring.push(Level3::Error, LogSource::App, "e".into());
        let shown: Vec<_> = ring
            .filtered(LogFilter::ErrorsAndWarnings)
            .into_iter()
            .map(|e| e.message)
            .collect();
        assert_eq!(shown, ["w", "e"]);
    }

    #[test]
    fn unread_counts_only_new_attention_entries_and_survives_clear() {
        let mut ring = LogRing::default();
        ring.push(Level3::Error, LogSource::App, "old".into());
        let mark = ring.next_seq();
        ring.push(Level3::Info, LogSource::Status, "i".into());
        ring.push(Level3::Warn, LogSource::App, "w".into());
        ring.push(Level3::Error, LogSource::App, "e".into());
        assert_eq!(ring.unread_since(mark), 2);
        assert_eq!(ring.unread_errors_since(mark), 1);
        ring.clear();
        assert_eq!(ring.unread_since(mark), 0);
        ring.push(Level3::Error, LogSource::App, "after".into());
        assert_eq!(ring.unread_since(mark), 1);
    }

    #[test]
    fn sources_come_from_the_target() {
        assert_eq!(LogSource::for_target("tod_cli::pr"), LogSource::TodCli);
        assert_eq!(LogSource::for_target("tod_core::autopilot::cloud"), LogSource::Orchestrator);
        assert_eq!(LogSource::for_target("tod_ui::app::window"), LogSource::App);
    }

    #[test]
    fn fmt_lines_parse_by_level() {
        let (l, t) = parse_fmt_line("2026-10-14T10:00:00.1Z  WARN tod_core::x: it broke").unwrap();
        assert_eq!(l, Level3::Warn);
        assert_eq!(t, "tod_core::x: it broke");
        assert!(parse_fmt_line("2026-10-14T10:00:00Z  INFO a: b").is_none());
        assert!(parse_fmt_line("   continuation").is_none());
    }

    #[test]
    fn tail_starts_at_the_end_then_reads_only_new_whole_lines() {
        let dir = std::env::temp_dir().join(format!("tod-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agentd.log");
        std::fs::write(&path, "t ERROR old: before\n").unwrap();
        let mut tail = AgentdTail::new(path.clone());
        assert_eq!(tail.poll().unwrap(), 0);
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        write!(f, "t INFO a: x\nt WARN b: new\nt ERROR c: part").unwrap();
        assert_eq!(tail.poll().unwrap(), 1);
        writeln!(f, "ial").unwrap();
        assert_eq!(tail.poll().unwrap(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn layer_records_warn_and_error_but_not_info() {
        use tracing_subscriber::prelude::*;
        let sub = tracing_subscriber::registry().with(RingLayer);
        let before = next_seq();
        tracing::subscriber::with_default(sub, || {
            tracing::info!("quiet");
            tracing::warn!(node = 3, "careful");
            tracing::error!("broke");
        });
        let mine: Vec<_> = snapshot(LogFilter::All)
            .into_iter()
            .filter(|e| e.seq >= before)
            .collect();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].level, Level3::Warn);
        assert!(mine[0].message.starts_with("careful") && mine[0].message.contains("node=3"));
        assert_eq!(mine[1].level, Level3::Error);
        assert_eq!(mine[1].message, "broke");
    }

    #[test]
    fn toast_gate_folds_repeats_within_the_window() {
        let mk = |seq, msg: &str| LogEntry {
            seq,
            at: SystemTime::now(),
            level: Level3::Error,
            source: LogSource::TodCli,
            message: msg.into(),
            notified: false,
        };
        let mut gate = ToastGate::default();
        let t0 = std::time::Instant::now();
        let first = gate.texts(&[mk(0, "boom"), mk(1, "boom"), mk(2, "other")], t0);
        assert_eq!(first, ["tod-cli: boom (×2)", "tod-cli: other"]);
        assert_eq!(gate.cursor(), 3);
        // Same error soon after: folded away, no flood.
        assert!(gate.texts(&[mk(3, "boom")], t0 + std::time::Duration::from_secs(10)).is_empty());
        // After the window it toasts again.
        let later = gate.texts(&[mk(4, "boom")], t0 + TOAST_REPEAT_WINDOW + std::time::Duration::from_secs(1));
        assert_eq!(later, ["tod-cli: boom"]);
    }

    #[test]
    fn notified_entries_never_need_a_toast() {
        let mut ring = LogRing::default();
        ring.push_with(Level3::Error, LogSource::App, "toast text".into(), true);
        ring.push(Level3::Error, LogSource::TodCli, "from cli".into());
        ring.push(Level3::Warn, LogSource::App, "warn".into());
        let need = ring.unnotified_errors_since(0);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].message, "from cli");
    }

    #[test]
    fn a_tod_cli_problem_reaches_the_buffer_with_its_source() {
        let root = std::env::temp_dir().join(format!("tod-cli-problem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut tail = tod_problems::Tail::new(&root);
        drain_problems(&mut tail).unwrap();
        tod_problems::report(&root, "tod-cli", tod_problems::Level::Error, Some("node-7"), "no such node").unwrap();
        let before = next_seq();
        assert_eq!(drain_problems(&mut tail).unwrap(), 1);
        let e = unnotified_errors_since(before);
        let mine = e.iter().find(|e| e.message == "[node-7] no such node").expect("entry");
        assert_eq!(mine.source, LogSource::TodCli);
        assert_eq!(mine.level, Level3::Error);
        std::fs::remove_dir_all(&root).ok();
    }
}
