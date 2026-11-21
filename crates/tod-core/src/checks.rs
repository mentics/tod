//! Checks: the commands a phase's gate runs (type checking, lint, and so on),
//! configured per phase in the Lifecycle config capability
//! (`tod_store::lifecycle_config::Check`).
//!
//! The app runs them, never an agent, where the node's files are (this
//! machine, a dev container, or its cloud sandbox). A check passes when its
//! command exits 0 and fails otherwise, with its output as the finding. Each
//! run is recorded with a **fingerprint of the files** it ran against
//! ([`fingerprint`]: every repository's `HEAD`, its uncommitted changes, and
//! its untracked files), so a run is current exactly while nothing changed,
//! committed or not, and every change makes the next look run them again.
//!
//! They are part of the gate out of the phase they are configured for
//! ([`gate_criteria`], one criterion per check, answered from the latest
//! run), and the autopilot runs the stale ones before anything else in the
//! phase ([`run_stale`]) and sends what failed to an agent as findings
//! ([`record_findings`]). A command that cannot run at all ([`Verdict::Broken`])
//! is never sent to an agent: it stops the run for the user.

use crate::phase::CriterionCheck;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::time::{Duration, Instant};
use tod_store::check_runs::{CheckRun, CheckRunRepo, Verdict};
use tod_store::fleet::{FleetStore, Workdir, worktree};
use tod_store::lifecycle_config::{self, Check};
use tod_store::outline::{GateCriterion, OUTCOME_FAIL, OUTCOME_PASS};
use uuid::Uuid;

/// How long one check may run before it is stopped and counts as broken.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long a check's setup may run (a cold dependency install is slower than a check).
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The most of a check's output kept: its end, where tools put the summary.
pub const OUTPUT_TAIL_BYTES: usize = 16 * 1024;

/// The slug prefix of a check's gate criterion.
pub const SLUG_PREFIX: &str = "check.";

/// The config key (`lifecycle_config::PHASES`) for a lifecycle state: its own
/// name, except the two states whose work is named for the protocol.
pub fn phase_key(state: &str) -> Option<&'static str> {
    let key = match state {
        "active" => "implement",
        "verifying" => "verify",
        other => lifecycle_config::PHASES.iter().copied().find(|p| *p == other)?,
    };
    Some(key)
}

/// The checks the gate out of `state` runs for `node`, after inheritance.
pub fn configured(conn: &rusqlite::Connection, node: Uuid, state: &str) -> Result<Vec<Check>> {
    let Some(key) = phase_key(state) else { return Ok(Vec::new()) };
    Ok(lifecycle_config::resolve_checks(conn, node, key)?.map(|r| r.checks).unwrap_or_default())
}

/// What one repository's files look like: its `HEAD`, its diff against
/// `HEAD`, and its untracked files' contents.
fn repo_fingerprint(repo: &Workdir) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(repo.git(&["rev-parse", "HEAD"]).with_context(|| format!("read HEAD in {repo}"))?);
    let diff = repo.git_output(&["diff", "HEAD", "--binary", "--no-ext-diff"])?;
    anyhow::ensure!(diff.status.success(), "git diff failed in {repo}: {}", String::from_utf8_lossy(&diff.stderr).trim());
    hasher.update(&diff.stdout);
    let listed = repo.git_output(&["ls-files", "-o", "--exclude-standard", "-z"])?;
    anyhow::ensure!(listed.status.success(), "git ls-files failed in {repo}: {}", String::from_utf8_lossy(&listed.stderr).trim());
    let files: Vec<&str> = listed.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()).filter_map(|f| std::str::from_utf8(f).ok()).collect();
    for chunk in files.chunks(200) {
        let mut args = vec!["hash-object", "--"];
        args.extend(chunk.iter().copied());
        let hashed = repo.git_output(&args)?;
        anyhow::ensure!(hashed.status.success(), "git hash-object failed in {repo}: {}", String::from_utf8_lossy(&hashed.stderr).trim());
        for (name, hash) in chunk.iter().zip(String::from_utf8_lossy(&hashed.stdout).lines()) {
            hasher.update(format!("{name} {hash}\n"));
        }
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the files at `cwd` look like: for the repository and each submodule,
/// [`repo_fingerprint`]. Equal fingerprints mean nothing a check could read
/// has changed.
pub fn fingerprint(cwd: &Workdir) -> Result<String> {
    Tree::new(cwd)?.fingerprint_all()
}

/// The repository at a node's files and its submodules, with what git says
/// about each, read once however many checks ask.
struct Tree {
    /// `(path under the root, "" for the root; directory)`, parents first.
    repos: Vec<(String, Workdir)>,
    fingerprints: std::cell::RefCell<std::collections::HashMap<usize, String>>,
    changes: std::cell::RefCell<std::collections::HashMap<usize, Changes>>,
}

/// What differs in one repository from where its branch left `origin`'s
/// default branch: commits, uncommitted changes and untracked files. Paths
/// are under the tree's root.
#[derive(Clone, Default)]
struct Changes {
    /// Added, copied, modified, renamed or untracked: files that exist.
    present: Vec<String>,
    deleted: Vec<String>,
}

impl Tree {
    fn new(cwd: &Workdir) -> Result<Self> {
        let mut repos = vec![(String::new(), cwd.clone())];
        for rel in worktree::submodule_paths(cwd).context("list the submodules")? {
            let dir = cwd.join(&rel);
            repos.push((rel, dir));
        }
        Ok(Self { repos, fingerprints: Default::default(), changes: Default::default() })
    }

    /// The key naming the files' commits ([`tod_store::fleet::provision::heads_key`]:
    /// the root's `HEAD` and each submodule's own) when the files are exactly
    /// those commits: nothing uncommitted or untracked in the repository or any
    /// submodule. A submodule pointer that moved does not count (tod never
    /// commits one). Gitignored files are not looked at, as in
    /// [`repo_fingerprint`]. `None` otherwise.
    fn clean_head(&self) -> Result<Option<String>> {
        tod_store::fleet::provision::clean_heads_key(&self.repos[0].1)
    }

    fn fingerprint_of(&self, index: usize) -> Result<String> {
        if let Some(found) = self.fingerprints.borrow().get(&index) {
            return Ok(found.clone());
        }
        let repo = &self.repos[index].1;
        let fp = repo_fingerprint(repo)?;
        self.fingerprints.borrow_mut().insert(index, fp.clone());
        Ok(fp)
    }

    fn fingerprint_all(&self) -> Result<String> {
        self.fingerprint_over(&(0..self.repos.len()).collect::<Vec<_>>(), "")
    }

    fn fingerprint_over(&self, indexes: &[usize], extra: &str) -> Result<String> {
        let mut hasher = Sha256::new();
        for &i in indexes {
            hasher.update(format!("repo {}\n{}\n", self.repos[i].1, self.fingerprint_of(i)?));
        }
        hasher.update(extra);
        Ok(hex(&hasher.finalize()))
    }

    /// The repositories a check scoped to `scope` depends on: those inside it,
    /// and the one that holds it.
    fn in_scope(&self, scope: &str) -> Vec<usize> {
        if scope == "." {
            return (0..self.repos.len()).collect();
        }
        let owner = (0..self.repos.len())
            .filter(|&i| under(scope, &self.repos[i].0))
            .max_by_key(|&i| self.repos[i].0.len());
        (0..self.repos.len())
            .filter(|&i| Some(i) == owner || (!self.repos[i].0.is_empty() && under(&self.repos[i].0, scope)))
            .collect()
    }

    fn changes_of(&self, index: usize) -> Result<Changes> {
        if let Some(found) = self.changes.borrow().get(&index) {
            return Ok(found.clone());
        }
        let (rel, repo) = &self.repos[index];
        let base = worktree::new_branch_base(repo).with_context(|| {
            format!("no origin/HEAD, origin/main or origin/master in {repo}: cannot tell what this branch changed")
        })?;
        let merge_base = repo.git(&["merge-base", &base, "HEAD"]).with_context(|| format!("find where this branch left {base} in {repo}"))?;
        let names = |args: &[&str]| -> Result<Vec<String>> {
            let out = repo.git_output(args)?;
            anyhow::ensure!(out.status.success(), "git {} failed in {repo}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim());
            Ok(out
                .stdout
                .split(|b| *b == 0)
                .filter(|f| !f.is_empty())
                .filter_map(|f| std::str::from_utf8(f).ok())
                .map(|f| if rel.is_empty() { f.to_string() } else { format!("{rel}/{f}") })
                .collect())
        };
        let mut present = names(&["diff", "--name-only", "-z", "--diff-filter=ACMRT", "--ignore-submodules=all", &merge_base])?;
        present.extend(names(&["ls-files", "-o", "--exclude-standard", "-z"])?);
        let deleted = names(&["diff", "--name-only", "-z", "--diff-filter=D", "--ignore-submodules=all", &merge_base])?;
        let found = Changes { present, deleted };
        self.changes.borrow_mut().insert(index, found.clone());
        Ok(found)
    }
}

/// Whether `path` is `prefix` or below it (`.` and blank hold everything).
fn under(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || prefix == "." || path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

/// `*` (any run of characters, `/` included) and `?` against all of `text`.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Whether a check applies to the tree as it is, with the files to hand it
/// and the fingerprint its latest run is judged against.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Assessment {
    applicable: bool,
    /// Changed files under the check's directory that match its pattern,
    /// relative to it, in order.
    files: Vec<String>,
    fingerprint: String,
}

fn assess(tree: &Tree, check: &Check) -> Result<Assessment> {
    let scope = check.scope().to_string();
    let indexes = tree.in_scope(&scope);
    if !check.is_scoped() {
        return Ok(Assessment { applicable: true, files: Vec::new(), fingerprint: tree.fingerprint_over(&indexes, "")? });
    }
    let patterns: Vec<&str> = check.pattern.split_whitespace().collect();
    let dir = check.dir.trim();
    let wanted = |path: &str| -> bool {
        if !under(path, &scope) {
            return false;
        }
        let rel = relative_to(path, dir);
        patterns.is_empty() || patterns.iter().any(|g| glob_match(g, rel))
    };
    let (mut files, mut any) = (Vec::new(), false);
    let mut seen = std::collections::BTreeSet::new();
    let mut deleted = Vec::new();
    for &i in &indexes {
        let changes = tree.changes_of(i)?;
        for path in changes.present {
            if wanted(&path) && seen.insert(path.clone()) {
                any = true;
                if under(&path, dir) {
                    files.push(relative_to(&path, dir).to_string());
                }
            }
        }
        for path in changes.deleted {
            if wanted(&path) {
                any = true;
                deleted.push(path);
            }
        }
    }
    files.sort();
    deleted.sort();
    let extra = format!("files\n{}\ndeleted\n{}", files.join("\n"), deleted.join("\n"));
    Ok(Assessment { applicable: any, files, fingerprint: tree.fingerprint_over(&indexes, &extra)? })
}

/// `path` below `dir` (the whole path when it is not).
fn relative_to<'a>(path: &'a str, dir: &str) -> &'a str {
    if dir.is_empty() || dir == "." {
        return path;
    }
    path.strip_prefix(dir).and_then(|r| r.strip_prefix('/')).unwrap_or(path)
}

/// The end of `bytes` as text, at most [`OUTPUT_TAIL_BYTES`], cut on a line.
pub fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= OUTPUT_TAIL_BYTES {
        return text.trim_end().to_string();
    }
    let mut start = text.len() - OUTPUT_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let cut = &text[start..];
    let cut = cut.find('\n').map_or(cut, |i| &cut[i + 1..]);
    format!("[output cut: the last {} bytes]\n{}", cut.len(), cut.trim_end())
}

/// The first words of a check's output when it was killed for memory.
pub const KILLED_FOR_MEMORY: &str = "Killed for memory";

/// What the check's script prints first, ahead of the command, where the
/// machine has `/proc/meminfo`: total and available MB. Stripped from the
/// output; used only to say what the machine had when a check is killed.
const MEM_MARKER: &str = "@@tod-mem ";
const MEM_PROBE: &str = "if [ -r /proc/meminfo ]; then echo \"@@tod-mem $(awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{print int(t/1024), int(a/1024)}' /proc/meminfo 2>/dev/null)\"; fi";

/// Take the memory lines the script printed out of the output, wherever they
/// are (a missing `awk` would put its error ahead of one): the first one's
/// (total MB, available MB) and the rest of the output.
fn split_memory(out: &[u8]) -> (Option<(u64, u64)>, Vec<u8>) {
    let mut mem = None;
    let mut rest = Vec::with_capacity(out.len());
    for line in out.split_inclusive(|b| *b == b'\n') {
        match line.strip_prefix(MEM_MARKER.as_bytes()) {
            Some(numbers) => {
                if mem.is_none() {
                    let text = String::from_utf8_lossy(numbers);
                    let mut nums = text.split_whitespace().map(|n| n.parse::<u64>());
                    if let (Some(Ok(total)), Some(Ok(free))) = (nums.next(), nums.next()) {
                        mem = Some((total, free));
                    }
                }
            }
            None => rest.extend_from_slice(line),
        }
    }
    (mem, rest)
}

/// Whether a non-zero exit was the machine running out of memory. Only what
/// a runtime really does then counts, so a failing test that merely mentions
/// memory stays a failure: a SIGKILL (exit 137 = 128 + 9: how the shell,
/// `docker exec` and the relay report one; the host's signal is mapped to it
/// too); V8's heap abort or a C++ `bad_alloc` (SIGABRT, 134) with its message;
/// Python's uncaught `MemoryError` (exit 1) as the final line of the
/// traceback; the shell's "Killed" as the last line.
fn killed_for_memory(code: i32, text: &str) -> bool {
    if code == 137 {
        return true;
    }
    let last = text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    if last == "Killed" || last.ends_with(": Killed") {
        return true;
    }
    if code == 134 && (text.contains("JavaScript heap out of memory") || text.contains("std::bad_alloc")) {
        return true;
    }
    code == 1 && (last == "MemoryError" || last.starts_with("MemoryError:"))
}

/// The first line of a memory failure's output.
fn memory_message(code: i32, mem: Option<(u64, u64)>, place: &str, command: &str) -> String {
    let how = if code == 137 {
        "SIGKILL, likely the out-of-memory killer"
    } else if code == 134 || code == 1 {
        "the process ran out of memory"
    } else {
        "the shell reported it killed, likely by the out-of-memory killer"
    };
    let machine = match mem {
        Some((total, free)) => format!(" It ran in {place}, which has {total} MB; {free} MB were free when the check started."),
        None => format!(" It ran in {place}."),
    };
    format!("{KILLED_FOR_MEMORY} ({how}).{machine} Command: `{}`.", command.trim())
}

/// Where a check ran, for the message.
fn place_name(cwd: &Workdir) -> String {
    match cwd {
        Workdir::Host(_) => "this machine".to_string(),
        Workdir::Container { container, .. } => format!("dev container {container}"),
        Workdir::Sandbox { sandbox, .. } => format!("sandbox {sandbox}"),
    }
}

/// Whether a stored run may stand as the answer for unchanged files. Only a
/// verdict about the code can (passed, failed, skipped); a broken run (timeout,
/// killed for memory, could not start) says nothing about the files, only about
/// the machine at that moment, so the next look runs the check again.
pub fn reusable(run: &CheckRun) -> bool {
    run.verdict != Verdict::Broken
}

/// When a run finished, as a person reads it.
pub fn ran_at_text(run: &CheckRun) -> String {
    chrono::DateTime::from_timestamp_millis(run.ran_at)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("{} ms since the epoch", run.ran_at))
}

/// Why a broken run stopped the node: its output, and (unless it was killed for
/// memory, whose words are fixed) that it is a fresh run and when it ended.
/// [`run_stale`] never returns a stored broken run, so this is always fresh.
pub fn broken_why(run: &CheckRun) -> String {
    if is_memory_kill(run) {
        run.output.clone()
    } else {
        format!("{} (a fresh run, ended {}; nothing recorded is reused for a check that could not run)", run.output, ran_at_text(run))
    }
}

/// Whether a stored run is one killed for memory.
pub fn is_memory_kill(run: &CheckRun) -> bool {
    run.verdict == Verdict::Broken && run.output.starts_with(KILLED_FOR_MEMORY)
}

/// What freeing memory in a sandbox did, for the retry and for the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFreed {
    /// Free MB before and after, where `/proc/meminfo` exists.
    pub before_mb: Option<u64>,
    pub after_mb: Option<u64>,
    /// One line per process stopped (pid and command line).
    pub stopped: Vec<String>,
    /// Anything that did not go as planned but did not stop the freeing.
    pub notes: Vec<String>,
}

impl MemoryFreed {
    fn describe(&self) -> String {
        let mb = |v: Option<u64>| v.map_or("unknown".to_string(), |v| format!("{v} MB"));
        let stopped = if self.stopped.is_empty() {
            "no dev server was running".to_string()
        } else {
            format!("stopped {}", self.stopped.join("; "))
        };
        let notes = if self.notes.is_empty() { " (the page cache was dropped too)".to_string() } else { format!(" ({})", self.notes.join("; ")) };
        format!("{stopped}; free memory {} before, {} after{notes}", mb(self.before_mb), mb(self.after_mb))
    }
}

/// Shell function: does the command line `$1` (words joined by spaces) belong
/// to a dev server the workspace boot starts? Anchored on the program, never on
/// text anywhere in the line: `python manage.py runserver`, `python -m uvicorn`
/// or `-m gunicorn` (or a `gunicorn`/`uvicorn` program), and node running
/// vite (`.../node_modules/.bin/vite`, `.../vite/bin/vite.js`, `vite`; vitest
/// does not match). A `grep`, `tail`, `sh`, editor or agent never does.
const DEV_SERVER_MATCH: &str = r#"dev_server_match() {
  set -f
  set -- $1
  prog=${1##*/}
  case "$prog" in
    python|python[0-9]*)
      case "$2" in
        manage.py|*/manage.py) [ "$3" = runserver ]; return;;
        -m) case "$3" in uvicorn|gunicorn) return 0;; esac; return 1;;
        *) case "${2##*/}" in gunicorn|uvicorn) return 0;; esac; return 1;;
      esac;;
    node)
      shift
      while [ $# -gt 0 ]; do
        case "$1" in -*) shift;; *) break;; esac
      done
      case "$1" in */node_modules/.bin/vite|*/vite/bin/vite.js|vite) return 0;; esac
      return 1;;
    gunicorn|uvicorn) return 0;;
  esac
  return 1
}
"#;

/// The script that frees memory in a sandbox. It stops only the dev servers the
/// workspace boot starts ([`DEV_SERVER_MATCH`]), by PID: TERM, then KILL for
/// what is left, and reports a process as `stopped` only once it is gone and
/// every kill that failed as `not-stopped`. It prints `@@tod-free-before MB`,
/// `stopped <pid> <cmd>`, `not-stopped <pid> <why>`, `note <text>` and
/// `@@tod-free-after MB`.
fn free_memory_script() -> String {
    format!(
        r#"exec 2>&1
{DEV_SERVER_MATCH}
mem() {{ awk '/^MemAvailable:/{{a=$2}} END{{print int(a/1024)}}' /proc/meminfo; }}
echo "@@tod-free-before $(mem)"
found=""
for d in /proc/[0-9]*; do
  pid=${{d#/proc/}}
  [ "$pid" = "$$" ] && continue
  cmd=$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null) || continue
  dev_server_match "$cmd" || continue
  found="$found$pid $(echo "$cmd" | cut -c1-120)
"
done
sent=""
printf '%s' "$found" | while read -r pid cmd; do
  err=$(kill -TERM "$pid" 2>&1) || echo "not-stopped $pid: $err"
done
sleep 3
printf '%s' "$found" | while read -r pid cmd; do
  if [ -d "/proc/$pid" ]; then
    err=$(kill -KILL "$pid" 2>&1) || echo "not-stopped $pid: $err"
    sleep 1
  fi
  if [ -d "/proc/$pid" ]; then echo "not-stopped $pid: still running after KILL"; else echo "stopped $pid $cmd"; fi
done
sync
err=$(sh -c 'echo 3 > /proc/sys/vm/drop_caches' 2>&1) || echo "note page cache not dropped: $err"
echo "@@tod-free-after $(mem)"
"#
    )
}

fn parse_freed(out: &str) -> Result<MemoryFreed> {
    let number = |tag: &str| out.lines().find_map(|l| l.strip_prefix(tag)).and_then(|n| n.trim().parse::<u64>().ok());
    let before_mb = number("@@tod-free-before ");
    let after_mb = number("@@tod-free-after ");
    if after_mb.is_none() {
        anyhow::bail!("the memory-freeing script did not finish; its output: {}", tail(out.as_bytes()));
    }
    let failed: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("not-stopped ")).collect();
    if !failed.is_empty() {
        anyhow::bail!("could not stop {}", failed.join("; "));
    }
    let stopped = out.lines().filter_map(|l| l.strip_prefix("stopped ")).map(str::to_string).collect();
    let notes = out.lines().filter_map(|l| l.strip_prefix("note ")).map(str::to_string).collect();
    Ok(MemoryFreed { before_mb, after_mb, stopped, notes })
}

/// Free memory in the sandbox `cwd` is in (stop its dev servers, drop the
/// page cache). Never for another place: the user's machine is not touched.
pub fn free_sandbox_memory(cwd: &Workdir) -> Result<MemoryFreed> {
    if !matches!(cwd, Workdir::Sandbox { .. }) {
        anyhow::bail!("memory is freed only inside a sandbox");
    }
    let o = cwd.output_within(Duration::from_secs(90), "sh", &["-c", &free_memory_script()])?;
    parse_freed(&String::from_utf8_lossy(&o.stdout))
}

/// Run one check at `cwd`: its command through the shell, stderr folded into
/// stdout. Exit 0 passes, any other exit fails; a command that cannot be run
/// or outlasts [`CHECK_TIMEOUT`] is broken, and so is one killed for memory
/// (an environment problem for the user, never a finding for the Fix agent).
/// In a sandbox a first kill for memory frees memory and runs the check once more.
pub fn run_command(cwd: &Workdir, check: &Check, files: &[String]) -> (Verdict, String) {
    let (verdict, out) = run_command_with(cwd, &|| run_command_once(cwd, check, files), &free_sandbox_memory);
    match missing_dependencies_hint(check, verdict, &out) {
        Some(hint) => (verdict, format!("{hint}\n\n{out}")),
        None => (verdict, out),
    }
}

/// Phrases tools print when what they need is not installed in the directory.
/// Language- and tool-neutral on purpose: tod names no package manager.
const MISSING_DEPENDENCY_MARKERS: &[&str] = &[
    "cannot find module",
    "cannot find package",
    "modulenotfounderror",
    "no module named",
    "command not found",
    ": not found",
    "no project-local",
    "not installed",
    "missing dependenc",
    "install dependencies",
];

/// Whether a line of output tells the user to run an install.
fn says_to_install(lower: &str) -> bool {
    lower.lines().any(|l| l.contains("run ") && l.contains("install"))
}

/// When a failed or timed-out check looks like it ran in a directory with no
/// installed dependencies, the plain reason, put ahead of the output. `None`
/// for a pass, a memory kill, or output that does not point at it.
pub fn missing_dependencies_hint(check: &Check, verdict: Verdict, output: &str) -> Option<String> {
    if output.starts_with(KILLED_FOR_MEMORY) {
        return None;
    }
    let lower = output.to_lowercase();
    let setup = check.setup.trim();
    let advice = if setup.is_empty() {
        "The check has no Setup command: set one on the check in the lifecycle config (a command that installs what it needs; tod runs it once per worktree before the check)."
    } else {
        "The check's Setup command ran and passed, but the dependencies are still missing: fix the setup command in the lifecycle config."
    };
    match verdict {
        Verdict::Failed => {
            let marker = MISSING_DEPENDENCY_MARKERS.iter().find(|m| lower.contains(**m));
            if marker.is_none() && !says_to_install(&lower) {
                return None;
            }
            Some(format!(
                "Probably missing dependencies in this worktree, not a problem with the code (the output mentions {:?}). {advice}",
                marker.copied().unwrap_or("an install step")
            ))
        }
        Verdict::Broken if lower.contains("did not finish in") => Some(format!(
            "The check timed out. In a fresh worktree that has no installed dependencies, that is often a cold install or a hang waiting for one. {advice}"
        )),
        _ => None,
    }
}

/// Run a check's `setup` once per worktree, before the check, and return `None` when the
/// directory is ready or `Some` broken run's output when it is not. The pass is a marker file
/// in the worktree's git directory (so a recreated worktree at the same path runs it again),
/// and is also shown in `check_runs` under the command `setup: <command>`. A failed or timed-out setup never falls back to running the
/// check without it. Returns the command's result when it actually ran.
fn ensure_setup(fleet: &FleetStore, node: Uuid, key: &str, dir: &str, cwd: &Workdir, check: &Check) -> Result<Option<SetupOutcome>> {
    let setup = check.setup.trim();
    if setup.is_empty() {
        return Ok(None);
    }
    let command = format!("setup: {setup}");
    // The pass is a marker inside the worktree's own git directory, so it goes when the
    // worktree does: a recreated one at the same path (Reset environment, a removed
    // location, a replaced sandbox) has none and runs the setup again. The database
    // record is only for display; a path or sandbox name is not an instance.
    // The git directory is shared by every directory of the repository, so the marker names
    // the (normalized) directory too: the same setup in `ui` and `api` is two setups.
    let id = hex(&Sha256::digest(format!("{}\0{setup}", clean_dir(dir)).as_bytes()));
    let marker = format!("\"$(git rev-parse --absolute-git-dir)/tod-setup-{}\"", &id[..16]);
    let (present, _) = run_shell(cwd, &format!("test -f {marker}"), &[], CHECK_TIMEOUT);
    if present == Verdict::Passed {
        return Ok(None);
    }
    let place = format!("{cwd:?}");
    let (verdict, output) = run_shell(cwd, setup, &[], SETUP_TIMEOUT);
    let (verdict, output) = if verdict == Verdict::Passed {
        match run_shell(cwd, &format!("touch {marker}"), &[], CHECK_TIMEOUT) {
            (Verdict::Passed, _) => (verdict, output),
            (_, why) => (Verdict::Broken, format!("{output}\n\nthe setup passed but tod could not record it in the worktree's git directory: {why}")),
        }
    } else {
        (verdict, output)
    };
    let verdict = if verdict == Verdict::Failed { Verdict::Broken } else { verdict };
    let output = if verdict == Verdict::Passed {
        output
    } else {
        format!(
            "The setup `{setup}` for check `{}` {} in {}, so the check was not run. It is a fresh run, and the check will not run until the setup passes.\n\n{output}",
            check.display(),
            if output.contains("did not finish in") { "timed out" } else { "failed" },
            if dir.is_empty() { "the node's files" } else { dir },
        )
    };
    let run = CheckRun {
        node_id: node,
        phase: key.to_string(),
        dir: dir.to_string(),
        command,
        label: format!("setup for {}", check.name()),
        fingerprint: place,
        verdict,
        output,
        ran_at: tod_store::check_runs::now(),
        tree_key: String::new(),
    };
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordCheckRun(Box::new(run.clone())))?;
    fleet.writer().flush()?;
    Ok(Some(SetupOutcome { verdict: run.verdict, output: run.output }))
}

/// What a setup that ran came to.
struct SetupOutcome {
    verdict: Verdict,
    output: String,
}

/// [`run_command`] with the run and the memory-freeing step given (for tests). `free` is
/// called only for a sandbox, after the first run was killed for memory.
fn run_command_with(cwd: &Workdir, run: &dyn Fn() -> (Verdict, String), free: &dyn Fn(&Workdir) -> Result<MemoryFreed>) -> (Verdict, String) {
    let (verdict, out) = run();
    if !(matches!(cwd, Workdir::Sandbox { .. }) && verdict == Verdict::Broken && out.starts_with(KILLED_FOR_MEMORY)) {
        return (verdict, out);
    }
    let note = match free(cwd) {
        Ok(f) => format!("tod freed memory and ran the check once more: {}.", f.describe()),
        Err(err) => return (Verdict::Broken, format!("{out}\n\ntod tried to free memory to run the check again and could not, so it did not run it again: {err:#}")),
    };
    let (verdict2, out2) = run();
    if verdict2 == Verdict::Broken && out2.starts_with(KILLED_FOR_MEMORY) {
        return (Verdict::Broken, format!("{out2}\n\nThe check was killed for memory twice. {note}"));
    }
    (verdict2, out2)
}

fn run_command_once(cwd: &Workdir, check: &Check, files: &[String]) -> (Verdict, String) {
    run_shell(cwd, &check.command, files, CHECK_TIMEOUT)
}

/// Run `command` through the shell at `cwd`, stopping it after `limit`.
pub(crate) fn run_shell(cwd: &Workdir, command_text: &str, files: &[String], limit: Duration) -> (Verdict, String) {
    // `exec 2>&1` makes one stream of what a person would see in a terminal.
    let script = format!("exec 2>&1\n{MEM_PROBE}\nexport TOD_CHANGED_FILES={}\n{}", shell_quote(&files.join("\n")), command_text);
    let result = match cwd {
        Workdir::Host(path) => {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", &script]).current_dir(path);
            run_with_timeout(command, limit, limit, &path.display().to_string())
        }
        Workdir::Sandbox { .. } => cwd.output_within(limit, "sh", &["-c", &script]).map(|o| (exit_code(&o.status), o.stdout)),
        Workdir::Container { container, path } => run_in_container(container, path, &script, limit),
    };
    match result {
        Ok((Some(code), out)) => {
            let (mem, rest) = split_memory(&out);
            let text = tail(&rest);
            if code == 0 {
                (Verdict::Passed, text)
            } else if killed_for_memory(code, &text) {
                (Verdict::Broken, format!("{}\n\n{text}", memory_message(code, mem, &place_name(cwd), command_text)))
            } else {
                (Verdict::Failed, text)
            }
        }
        Ok((None, out)) => (Verdict::Broken, format!("the command was stopped by a signal\n{}", tail(&split_memory(&out).1))),
        Err(err) => (Verdict::Broken, format!("{err:#}")),
    }
}

/// A process's exit code; one ended by a signal is `128 + signal`, as a shell
/// and the relay report it.
fn exit_code(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.code().or_else(|| status.signal().map(|s| 128 + s))
    }
    #[cfg(not(unix))]
    {
        status.code()
    }
}

/// `text` as one single-quoted shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Run `script` in a dev container. The check is wrapped in the container's
/// `timeout` (when it has one), which stops the process itself; the host-side
/// deadline is a little later and stops the `docker exec` client, so a check
/// can never hang forever either way. Exit 124 (what `timeout` returns) is a
/// timeout, reported as a broken check.
fn run_in_container(container: &str, path: &str, script: &str, limit: Duration) -> Result<(Option<i32>, Vec<u8>)> {
    let secs = limit.as_secs();
    let wrapper = format!(
        "if command -v timeout >/dev/null 2>&1; then exec timeout -k 5 {secs} sh -c \"$1\"; else exec sh -c \"$1\"; fi"
    );
    let exec = tod_agent::devcontainer::ContainerExec::connect(container)?;
    let command = exec.command(path, "sh", &["-c", &wrapper, "sh", script])?;
    let (code, out) = run_with_timeout(command, limit + Duration::from_secs(30), limit, container)?;
    if code == Some(124) {
        anyhow::bail!("the command did not finish in {} minutes and was stopped", secs / 60);
    }
    Ok((code, out))
}

/// Run `command` (stdin closed, stderr dropped: the script folds it into
/// stdout) and collect its output, killing it after `limit`.
fn run_with_timeout(mut command: std::process::Command, limit: Duration, shown: Duration, what: &str) -> Result<(Option<i32>, Vec<u8>)> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start the check in {what}"))?;
    let mut stdout = child.stdout.take().context("the command's output was not captured")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = stdout.read_to_end(&mut buf);
        (buf, read)
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            anyhow::bail!("the command did not finish in {} minutes and was stopped", shown.as_secs() / 60);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (buf, read) = reader.join().map_err(|_| anyhow::anyhow!("reading the command's output panicked"))?;
    read.context("read the command's output")?;
    Ok((exit_code(&status), buf))
}

/// What running the stale checks found.
#[derive(Debug, Clone, Default)]
pub struct Round {
    /// The fingerprint the checks ran against.
    pub fingerprint: String,
    /// Every configured check's latest run, in order.
    pub runs: Vec<CheckRun>,
}

impl Round {
    pub fn failed(&self) -> impl Iterator<Item = &CheckRun> {
        self.runs.iter().filter(|r| r.verdict == Verdict::Failed)
    }

    pub fn broken(&self) -> impl Iterator<Item = &CheckRun> {
        self.runs.iter().filter(|r| r.verdict == Verdict::Broken)
    }

    pub fn clear(&self) -> bool {
        self.runs.iter().all(|r| matches!(r.verdict, Verdict::Passed | Verdict::Skipped))
    }
}

/// Names a committed tree and what a check is, as a run records it
/// ([`CheckRun::tree_key`]): the head commit and every setting of the check
/// that decides what it does here.
pub fn tree_key(head: &str, check: &Check) -> String {
    let text = format!(
        "tree\0{head}\0{}\0{}\0{}\0{}\0{}\0{}",
        check.label, check.command, check.dir, check.changed, check.pattern, check.setup
    );
    hex(&Sha256::digest(text.as_bytes()))
}

/// The round for a node whose worktree is not made, answered without making
/// it, when that is certain: every check's latest run passed (or did not
/// apply) against exactly the commit now at the head of the node's branch on
/// `origin`, and the check is as it was then ([`tree_key`]). `None` when
/// the files are made (then they are looked at as they are), or when any check
/// has no such run: the caller makes the files and looks properly. A node
/// parked for days on a review is woken often (a webhook, a timer); without
/// this each wake leased a pool worktree, which the daemon's release then took
/// back (shepherd gap 68).
fn current_without_files(fleet: &FleetStore, node: Uuid, key: &str, checks: &[Check]) -> Result<Option<Round>> {
    let Some(head) = tod_store::fleet::provision::unmade_branch_head(fleet, &node.to_string())
        .context("find out whether the pushed branch changed since the checks last ran")?
    else {
        return Ok(None);
    };
    let mut runs = Vec::new();
    for check in checks {
        let latest = fleet.read(|conn| CheckRunRepo::new(conn).latest(node, key, &clean_dir(&check.dir), &check.command))?;
        match latest {
            Some(run)
                if run.tree_key == tree_key(&head, check)
                    && run.label == check.label
                    && matches!(run.verdict, Verdict::Passed | Verdict::Skipped) =>
            {
                runs.push(run)
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(Round { fingerprint: format!("head {head}"), runs }))
}

/// The stored run that still answers for `check` on files assessed as
/// `assessment`, when `clean_head` is the head if they are exactly a commit:
/// the same files (fingerprint), or the same commit and check ([`tree_key`]; a
/// worktree made again elsewhere has other paths, so another fingerprint).
fn current_run(latest: Option<CheckRun>, check: &Check, assessment: &Assessment, clean_head: Option<&str>) -> Option<CheckRun> {
    let run = latest?;
    let key_now = clean_head.map(|head| tree_key(head, check));
    let same = run.fingerprint == assessment.fingerprint || key_now.as_deref().is_some_and(|k| run.tree_key == k);
    (run.label == check.label && reusable(&run) && same).then_some(run)
}

/// The round for a node whose worktree is made, answered by looking at the
/// files without using them: nothing is stamped as used, so a node parked on a
/// review, woken every few minutes to find nothing changed, does not keep its
/// lease from being given back (`worktree_release`). `None` when any check has
/// to run: the caller then resolves the files as a use.
fn current_in_made_files(fleet: &FleetStore, node: Uuid, key: &str, checks: &[Check]) -> Result<Option<Round>> {
    let looked = tod_store::fleet::provision::read_made_files(fleet, &node.to_string(), |cwd| {
        let tree = Tree::new(cwd)?;
        let clean_head = tree.clean_head()?;
        let mut runs = Vec::new();
        for check in checks {
            let dir = clean_dir(&check.dir);
            let assessment = assess(&tree, check)?;
            let latest = fleet.read(|conn| CheckRunRepo::new(conn).latest(node, key, &dir, &check.command))?;
            let Some(run) = current_run(latest, check, &assessment, clean_head.as_deref()) else { return Ok(None) };
            let run = match clean_head.as_deref().map(|head| tree_key(head, check)) {
                // Remember which commit it answers for, so a later look needs no files.
                Some(k) if run.tree_key != k => {
                    let run = CheckRun { tree_key: k, ..run };
                    fleet.enqueue(tod_store::fleet::FleetMutation::RecordCheckRun(Box::new(run.clone())))?;
                    fleet.writer().flush()?;
                    run
                }
                _ => run,
            };
            runs.push(run);
        }
        Ok(Some(Round { fingerprint: tree.fingerprint_all()?, runs }))
    })?;
    Ok(looked.flatten())
}

/// Run the checks configured for `state` whose latest run was against other
/// files than these (or never happened), and return every check's latest run.
/// Does nothing, and returns an empty round, when none is configured. Git,
/// Docker, or a sandbox may be involved: never call it on the UI thread.
pub fn run_stale(fleet: &FleetStore, node: Uuid, state: &str) -> Result<Round> {
    run_stale_notifying(fleet, node, state, &mut |_| {})
}

/// [`run_stale`], telling `on_work` (once, with what it is doing) just before
/// the first command (or its setup) runs. A look that finds every check
/// current, or skipped, never calls it, so a caller that stays
/// "waiting" through a cheap re-check can say "working" exactly when the
/// minutes begin (gap 80).
pub fn run_stale_notifying(fleet: &FleetStore, node: Uuid, state: &str, on_work: &mut dyn FnMut(&str)) -> Result<Round> {
    let Some(key) = phase_key(state) else { return Ok(Round::default()) };
    let checks = fleet.read(|conn| configured(conn, node, state))?;
    if checks.is_empty() {
        return Ok(Round::default());
    }
    // Current runs of a node with no worktree need none made to say so.
    if let Some(round) = current_without_files(fleet, node, key, &checks)? {
        return Ok(round);
    }
    if let Some(round) = current_in_made_files(fleet, node, key, &checks)? {
        return Ok(round);
    }
    // The cheap looks could not answer. Resolving the files and fingerprinting
    // them are looks (git, docker exec: seconds), not work: only a command
    // about to run is, so `on_work` waits for that (gap 80).
    let mut announced = false;
    let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string())
        .with_context(|| format!("find the files to run the {state} checks in"))?;
    let mut tree = Tree::new(&cwd)?;
    let mut clean_head = tree.clean_head()?;
    let mut round = Round { fingerprint: tree.fingerprint_all()?, runs: Vec::new() };
    // The commands this round actually ran, for the node's activity.
    let mut ran: Vec<tod_store::node_timeline::CommandResult> = Vec::new();
    let mut fixed_files = false;
    for check in &checks {
        let dir = clean_dir(&check.dir);
        let assessment = assess(&tree, check)?;
        let latest = fleet.read(|conn| CheckRunRepo::new(conn).latest(node, key, &dir, &check.command))?;
        let mut key_now = clean_head.as_deref().map(|head| tree_key(head, check));
        let mut assessment = assessment;
        // The files a fixer changed (below): the tree is read again after this check.
        let mut refreshed: Option<Tree> = None;
        let run = match latest {
            // Current: the same files, or the same commit (a worktree made
            // again elsewhere has other paths, so another fingerprint).
            Some(run)
                if run.label == check.label
                    && reusable(&run)
                    && (run.fingerprint == assessment.fingerprint || key_now.as_deref().is_some_and(|k| run.tree_key == k)) =>
            {
                match key_now {
                    // Remember which commit it answers for, so a later look
                    // needs no files ([`current_without_files`]).
                    Some(k) if run.tree_key != k => {
                        let run = CheckRun { tree_key: k, ..run };
                        fleet.enqueue(tod_store::fleet::FleetMutation::RecordCheckRun(Box::new(run.clone())))?;
                        fleet.writer().flush()?;
                        run
                    }
                    _ => run,
                }
            }
            _ => {
                let (verdict, output) = if assessment.applicable {
                    if !announced {
                        announced = true;
                        on_work(&format!("running the {} checks", tod_store::settings::lifecycle_display_name(state)));
                    }
                    let at = cwd.join(&dir);
                    match ensure_setup(fleet, node, key, &dir, &at, check)? {
                        Some(setup) if setup.verdict != Verdict::Passed => {
                            ran.push(tod_store::node_timeline::CommandResult {
                                name: format!("setup for {}", check.display()),
                                command: check.setup.clone(),
                                verdict: setup.verdict.as_str().to_string(),
                                output: setup.output.clone(),
                            });
                            (setup.verdict, setup.output)
                        }
                        Some(setup) => {
                            ran.push(tod_store::node_timeline::CommandResult {
                                name: format!("setup for {}", check.display()),
                                command: check.setup.clone(),
                                verdict: setup.verdict.as_str().to_string(),
                                output: setup.output,
                            });
                            run_command(&at, check, &assessment.files)
                        }
                        None => run_command(&at, check, &assessment.files),
                    }
                } else {
                    (Verdict::Skipped, format!("nothing changed in {}", check.scope()))
                };
                // A configured fixer runs on a failure, then only this check runs again.
                let (verdict, output) = if verdict == Verdict::Failed && !check.fix.trim().is_empty() && assessment.applicable {
                    let at = cwd.join(&dir);
                    let (verdict, output, fixed) = fix_and_rerun(&at, check, &cwd, output, &mut ran)?;
                    if verdict == Verdict::Failed {
                        // The configured fix did not settle it: the failure goes on to an
                        // agent, which is a lesson about the fix (once per task and check).
                        fleet
                            .interview(
                                "autopilot",
                                tod_store::interview::InterviewCommand::Lessons(tod_store::lessons::Op::FixerFailed {
                                    node,
                                    check_name: check.display(),
                                    check_key: tod_store::quick_fixes::check_key(&dir, &check.command),
                                    why: output.clone(),
                                }),
                            )
                            .map_err(|e| anyhow::anyhow!("could not record the fixer failure of {}: {e}", check.display()))?;
                    }
                    if let Some(new_tree) = fixed {
                        assessment = assess(&new_tree, check)?;
                        key_now = new_tree.clean_head()?.as_deref().map(|head| tree_key(head, check));
                        refreshed = Some(new_tree);
                    }
                    (verdict, output)
                } else {
                    (verdict, output)
                };
                let run = CheckRun {
                    node_id: node,
                    phase: key.to_string(),
                    dir,
                    command: check.command.clone(),
                    label: check.label.clone(),
                    fingerprint: assessment.fingerprint,
                    verdict,
                    output,
                    ran_at: tod_store::check_runs::now(),
                    tree_key: key_now.unwrap_or_default(),
                };
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordCheckRun(Box::new(run.clone())))?;
                fleet.writer().flush()?;
                if run.verdict != Verdict::Skipped {
                    ran.push(tod_store::node_timeline::CommandResult {
                        name: run_name(&run),
                        command: run.command.clone(),
                        verdict: run.verdict.as_str().to_string(),
                        output: run.output.clone(),
                    });
                }
                run
            }
        };
        if let Some(new_tree) = refreshed {
            fixed_files = true;
            clean_head = new_tree.clean_head()?;
            round.fingerprint = new_tree.fingerprint_all()?;
            tree = new_tree;
        }
        round.runs.push(run);
    }
    if !ran.is_empty() {
        let failed = ran.iter().any(|r| r.is_failure());
        let title = if ran.len() == 1 { format!("Ran {}", ran[0].name) } else { format!("Ran {} commands", ran.len()) };
        let entry = tod_store::node_timeline::TimelineEntry::new(
            node,
            tod_store::node_timeline::KIND_CHECKS,
            state,
            title,
            failed,
            serde_json::to_string(&ran)?,
        );
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
        fleet.writer().flush()?;
    }
    // A mechanical commit is exempt from review: a round of review that was
    // done before it covers the new head too, so it does not make the pull
    // request's first head look unreviewed (`pr_draft_review::latest_review`).
    if fixed_files {
        crate::pr_draft_review::record_review_phase_heads(fleet, node, &cwd)?;
    }
    Ok(round)
}

/// Run only the check `command` (run in `dir`) of `state`'s configured
/// checks again, against the files as they are now, and record its run: what
/// tod does after a quick fix (`doc/requests/design.md` section 5), so no other
/// check is run for it. A check that no longer applies to the files is
/// `Skipped`. An error when `state` has no such check configured. Git, Docker
/// or a sandbox may be involved: never call it on the UI thread.
pub fn run_one(fleet: &FleetStore, node: Uuid, state: &str, dir: &str, command: &str) -> Result<CheckRun> {
    let key = phase_key(state).with_context(|| format!("the {state} state has no checks"))?;
    let checks = fleet.read(|conn| configured(conn, node, state))?;
    let want = clean_dir(dir);
    let check = checks
        .iter()
        .find(|c| clean_dir(&c.dir) == want && c.command == command)
        .with_context(|| format!("no check `{command}` (in `{want}`) is configured for the {state} state"))?;
    let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string())
        .with_context(|| format!("find the files to run the {state} check in"))?;
    let tree = Tree::new(&cwd)?;
    let clean_head = tree.clean_head()?;
    let assessment = assess(&tree, check)?;
    let mut ran: Vec<tod_store::node_timeline::CommandResult> = Vec::new();
    let (verdict, output) = if assessment.applicable {
        let at = cwd.join(&want);
        match ensure_setup(fleet, node, key, &want, &at, check)? {
            Some(setup) if setup.verdict != Verdict::Passed => (setup.verdict, setup.output),
            _ => run_command(&at, check, &assessment.files),
        }
    } else {
        (Verdict::Skipped, format!("nothing changed in {}", check.scope()))
    };
    let run = CheckRun {
        node_id: node,
        phase: key.to_string(),
        dir: want,
        command: check.command.clone(),
        label: check.label.clone(),
        fingerprint: assessment.fingerprint,
        verdict,
        output,
        ran_at: tod_store::check_runs::now(),
        tree_key: clean_head.as_deref().map(|head| tree_key(head, check)).unwrap_or_default(),
    };
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordCheckRun(Box::new(run.clone())))?;
    fleet.writer().flush()?;
    if run.verdict != Verdict::Skipped {
        ran.push(tod_store::node_timeline::CommandResult {
            name: run_name(&run),
            command: run.command.clone(),
            verdict: run.verdict.as_str().to_string(),
            output: run.output.clone(),
        });
        let entry = tod_store::node_timeline::TimelineEntry::new(
            node,
            tod_store::node_timeline::KIND_CHECKS,
            state,
            format!("Ran {} again after a quick fix", ran[0].name),
            ran[0].is_failure(),
            serde_json::to_string(&ran)?,
        );
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
        fleet.writer().flush()?;
    }
    Ok(run)
}

/// A check failed and has a configured `fix`: run it (committing what it
/// changed as tod's own mechanical commit, [`crate::fixer`]) and run only
/// this check again. Returns the verdict and output to record, and the
/// freshly read tree when the fix changed files. A fix that fails, changes
/// nothing, or leaves the check failing hands the failure on as it was, with
/// what the fixer did added: nothing is hidden, and no agent is asked until
/// the fixer has had its turn.
fn fix_and_rerun(
    at: &Workdir,
    check: &Check,
    cwd: &Workdir,
    first_output: String,
    ran: &mut Vec<tod_store::node_timeline::CommandResult>,
) -> Result<(Verdict, String, Option<Tree>)> {
    let outcome = crate::fixer::run_fix(at, &check.display(), &check.fix)?;
    let (fix_verdict, fix_output) = match &outcome {
        crate::fixer::FixOutcome::Failed(why) => (Verdict::Failed, why.clone()),
        crate::fixer::FixOutcome::NoChange => (Verdict::Passed, "changed nothing".to_string()),
        crate::fixer::FixOutcome::Committed { head } => (Verdict::Passed, format!("committed as {head} (a mechanical commit)")),
        crate::fixer::FixOutcome::ChangedUncommitted => {
            (Verdict::Passed, "changed files, left uncommitted: the tree already had uncommitted work that is not the fixer's".to_string())
        }
    };
    ran.push(tod_store::node_timeline::CommandResult {
        name: format!("fix for {}", check.display()),
        command: check.fix.clone(),
        verdict: fix_verdict.as_str().to_string(),
        output: fix_output.clone(),
    });
    if !outcome.changed() {
        return Ok((Verdict::Failed, format!("{first_output}\n\nThe configured fix `{}` ran: {fix_output}", check.fix.trim()), None));
    }
    let tree = Tree::new(cwd)?;
    let assessment = assess(&tree, check)?;
    let (verdict, output) = run_command(at, check, &assessment.files);
    let output = match verdict {
        Verdict::Passed => format!("passed after tod's configured fix `{}` ({fix_output})\n\n{output}", check.fix.trim()),
        _ => format!("{output}\n\n(after tod's configured fix `{}`, which {fix_output}; before it: {first_output})", check.fix.trim()),
    };
    Ok((verdict, output, Some(tree)))
}

/// A check's directory as run keys hold it: `.` is the root, blank.
fn clean_dir(dir: &str) -> String {
    let dir = dir.trim();
    if dir == "." { String::new() } else { dir.to_string() }
}

/// The name a run goes by in messages: its label, else its command.
pub fn run_name(run: &CheckRun) -> String {
    lifecycle_config::display_name(&run.label, &run.command, &run.dir)
}

/// The gate criteria for the checks configured for `state`, answered from
/// each one's latest run. These are not rows of the criteria catalog, so they
/// are never saved as evaluations (`settle_gate` leaves them out).
pub fn gate_criteria(conn: &rusqlite::Connection, node: Uuid, state: &str, to: &str) -> Result<Vec<CriterionCheck>> {
    let Some(key) = phase_key(state) else { return Ok(Vec::new()) };
    let repo = CheckRunRepo::new(conn);
    let mut out = Vec::new();
    for (index, check) in configured(conn, node, state)?.into_iter().enumerate() {
        let dir = clean_dir(&check.dir);
        let (outcome, detail) = match repo.latest(node, key, &dir, &check.command)? {
            None => (OUTCOME_FAIL, "has not run yet".to_string()),
            Some(run) => match run.verdict {
                Verdict::Passed => (OUTCOME_PASS, String::new()),
                Verdict::Failed => (OUTCOME_FAIL, format!("failed\n{}", run.output)),
                Verdict::Broken => (OUTCOME_FAIL, format!("could not run (the recorded result of the run that ended {}, not a new run): {}", ran_at_text(&run), run.output)),
                // Nothing it is scoped to changed: not part of this gate.
                Verdict::Skipped => continue,
            },
        };
        out.push(CriterionCheck {
            criterion: GateCriterion {
                id: criterion_id(node, key, &dir, &check.command),
                from_state: state.to_string(),
                to_state: to.to_string(),
                slug: format!("{SLUG_PREFIX}{}", slug(&check.display())),
                label: format!("Check: {}", check.display()),
                sort_order: 1000 + index as i32,
                active: true,
            },
            outcome,
            detail,
        });
    }
    Ok(out)
}

/// Whether `criterion` is a check's, not a row of the catalog.
pub fn is_check_criterion(criterion: &GateCriterion) -> bool {
    criterion.slug.starts_with(SLUG_PREFIX)
}

fn criterion_id(node: Uuid, phase: &str, dir: &str, command: &str) -> Uuid {
    let digest = Sha256::digest(format!("check\0{node}\0{phase}\0{dir}\0{command}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

fn slug(name: &str) -> String {
    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    slug.trim_matches('-').to_string()
}

/// The paragraph a finding carries when a quick fix tried its check first.
fn quick_fix_note(notes: &[(String, String)], name: &str) -> String {
    match notes.iter().find(|(n, _)| n == name) {
        Some((_, text)) => format!(
            "\n\nA quick fix (a small, cheap session limited to this one check) was tried first and did not make it pass. What it did and left undone:\n{}",
            if text.trim().is_empty() { "(it recorded nothing)" } else { text.trim() }
        ),
        None => String::new(),
    }
}

/// The summary of the finding a failing check becomes.
pub fn finding_summary(name: &str) -> String {
    format!("Check `{name}` fails")
}

/// Record each failing run as an open review finding on the node, which the
/// Fix protocol resolves, unless one for the same check is already open (then
/// its detail is left as it is: the output only changes when the files do,
/// and a new finding would answer a fix the agent has not made). Returns how
/// many findings were added.
pub fn record_findings(fleet: &FleetStore, node: Uuid, failed: &[&CheckRun]) -> Result<usize> {
    record_findings_with_notes(fleet, node, failed, &[])
}

/// [`record_findings`], with what a quick fix that tried a check first did and
/// left undone (`notes`: the check's [`run_name`] and the notes), added to that
/// check's finding, so the Fix session's opening carries it.
pub fn record_findings_with_notes(fleet: &FleetStore, node: Uuid, failed: &[&CheckRun], notes: &[(String, String)]) -> Result<usize> {
    let open: Vec<String> = fleet.read(|conn| {
        Ok(tod_store::review::ReviewRepo::new(conn)
            .list_for_node(node)?
            .into_iter()
            .filter(|f| f.is_open())
            .map(|f| f.summary)
            .collect())
    })?;
    let mut added = 0;
    for run in failed {
        let summary = finding_summary(&run_name(run));
        if open.contains(&summary) {
            continue;
        }
        fleet.interview(
            "autopilot",
            tod_store::interview::InterviewCommand::AddReviewFinding {
                node_id: node,
                conversation_id: None,
                finding: tod_store::review::NewFinding {
                    importance: "blocker".to_string(), impact: "high".to_string(), complexity: "simple".to_string(),
                    file: None,
                    line: None,
                    summary,
                    detail: Some(format!(
                        "The phase's check `{}` (`{}`{}) exited non-zero. Fix what it reports; the app runs it again after your change.\n\n{}",
                        run_name(run),
                        run.command,
                        if run.dir.is_empty() { String::new() } else { format!(", run in `{}`", run.dir) },
                        run.output
                    ) + &quick_fix_note(notes, &run_name(run))),
                    root_cause: None,
                    rule: None,
                },
            },
        )?;
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A check that wants `a.txt` to say `pretty`, with a fixer that writes it.
    fn format_check() -> Check {
        Check {
            label: "format".into(),
            command: "test \"$(cat a.txt)\" = pretty".into(),
            fix: "echo pretty > a.txt".into(),
            ..Check::default()
        }
    }

    #[test]
    fn a_failing_check_with_a_fix_is_fixed_committed_and_run_again_by_itself() {
        let dir = crate::fixer::tests::repo();
        let at = Workdir::host(dir.path());
        let check = format_check();
        let (first, first_out) = run_command(&at, &check, &[]);
        assert_eq!(first, Verdict::Failed);
        let mut ran = Vec::new();
        let (verdict, output, tree) = fix_and_rerun(&at, &check, &at, first_out, &mut ran).unwrap();
        assert_eq!(verdict, Verdict::Passed, "{output}");
        assert!(output.contains("passed after tod's configured fix"), "{output}");
        assert!(tree.is_some(), "the files changed, so the tree is read again");
        assert!(at.git(&["log", "-1", "--format=%s"]).unwrap().starts_with(crate::fixer::COMMIT_SUBJECT_PREFIX));
        assert_eq!(ran.len(), 1);
        assert_eq!(ran[0].name, "fix for format");
        assert!(at.git(&["status", "--porcelain"]).unwrap().is_empty());
    }

    #[test]
    fn a_fix_that_changes_nothing_leaves_the_failure_as_it_was_with_what_the_fixer_did() {
        let dir = crate::fixer::tests::repo();
        let at = Workdir::host(dir.path());
        let check = Check { fix: "true".into(), ..format_check() };
        let before = at.git(&["rev-parse", "HEAD"]).unwrap();
        let mut ran = Vec::new();
        let (verdict, output, tree) = fix_and_rerun(&at, &check, &at, "it failed".into(), &mut ran).unwrap();
        assert_eq!(verdict, Verdict::Failed);
        assert!(tree.is_none());
        assert!(output.contains("it failed") && output.contains("changed nothing"), "{output}");
        assert_eq!(at.git(&["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn a_fix_that_does_not_cure_the_check_still_fails_it_after_the_commit() {
        let dir = crate::fixer::tests::repo();
        let at = Workdir::host(dir.path());
        let check = Check { fix: "echo other > a.txt".into(), ..format_check() };
        let mut ran = Vec::new();
        let (verdict, output, tree) = fix_and_rerun(&at, &check, &at, "it failed".into(), &mut ran).unwrap();
        assert_eq!(verdict, Verdict::Failed);
        assert!(tree.is_some());
        assert!(output.contains("after tod's configured fix"), "{output}");
    }

    #[test]
    fn missing_dependencies_are_named_plainly_for_a_failure_or_a_timeout() {
        let bare = Check { command: "x".into(), ..Check::default() };
        let with = Check { command: "x".into(), setup: "install".into(), ..Check::default() };
        let hint = missing_dependencies_hint(&bare, Verdict::Failed, "No project-local tool was found. Run foo install").unwrap();
        assert!(hint.starts_with("Probably missing dependencies") && hint.contains("no Setup command"), "{hint}");
        let hint = missing_dependencies_hint(&with, Verdict::Failed, "Error: Cannot find module 'x'").unwrap();
        assert!(hint.contains("Setup command ran and passed"), "{hint}");
        let hint = missing_dependencies_hint(&bare, Verdict::Broken, "the command did not finish in 10 minutes and was stopped").unwrap();
        assert!(hint.starts_with("The check timed out") && hint.contains("no Setup command"), "{hint}");
        // A real failure, a pass and a memory kill say nothing about dependencies.
        assert_eq!(missing_dependencies_hint(&bare, Verdict::Failed, "1 test failed"), None);
        assert_eq!(missing_dependencies_hint(&bare, Verdict::Passed, "node_modules ok"), None);
        assert_eq!(missing_dependencies_hint(&bare, Verdict::Broken, "the sandbox failed"), None);
        // The hint leads the reason a run_command failure gives.
        let dir = tempfile::tempdir().unwrap();
        let (verdict, out) = run_command(&Workdir::host(dir.path()), &Check { command: "no-such-command-here".into(), ..Check::default() }, &[]);
        assert_eq!(verdict, Verdict::Failed);
        assert!(out.starts_with("Probably missing dependencies"), "{out}");
    }

    #[test]
    fn phase_key_maps_the_states_named_for_protocols() {
        assert_eq!(phase_key("active"), Some("implement"));
        assert_eq!(phase_key("verifying"), Some("verify"));
        assert_eq!(phase_key("review"), Some("review"));
        assert_eq!(phase_key("pr_draft"), Some("pr_draft"));
        assert_eq!(phase_key("ready"), None);
        assert_eq!(phase_key("done"), None);
    }

    #[test]
    fn tail_keeps_the_end_cut_on_a_line() {
        let line = "x".repeat(99) + "\n";
        let text = line.repeat(400);
        let out = tail(text.as_bytes());
        assert!(out.starts_with("[output cut"));
        assert!(out.len() < OUTPUT_TAIL_BYTES + 100);
        assert!(out.ends_with(&"x".repeat(99)));
        assert_eq!(tail(b"short\n"), "short");
    }

    #[test]
    fn a_check_passes_on_exit_zero_fails_otherwise_and_folds_stderr_in() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        let check = |command: &str| Check { command: command.to_string(), ..Check::default() };
        assert_eq!(run_command(&cwd, &check("echo fine"), &[]), (Verdict::Passed, "fine".to_string()));
        assert_eq!(run_command(&cwd, &check("echo bad >&2; exit 3"), &[]), (Verdict::Failed, "bad".to_string()));
        // Output on success is kept too, and a missing command is a failure of the
        // command (exit 127), not of running it.
        assert_eq!(run_command(&cwd, &check("no-such-command-here"), &[]).0, Verdict::Failed);
    }

    #[test]
    fn a_sigkilled_check_is_killed_for_memory_not_a_finding() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        let check = |command: &str| Check { command: command.to_string(), ..Check::default() };
        let (verdict, out) = run_command(&cwd, &check("echo working; kill -9 $$"), &[]);
        assert_eq!(verdict, Verdict::Broken, "{out}");
        assert!(out.starts_with("Killed for memory (SIGKILL, likely the out-of-memory killer)."), "{out}");
        assert!(out.contains("working"));
        assert!(!out.contains("@@tod-mem"));
        assert!(out.contains("It ran in this machine."), "{out}");
        // The shell's own exit 137 reads the same, as it does through a relay.
        assert_eq!(run_command(&cwd, &check("exit 137"), &[]).0, Verdict::Broken);
    }

    #[test]
    fn a_sandbox_check_killed_for_memory_frees_memory_and_uses_the_second_run() {
        use std::cell::Cell;
        let cwd = Workdir::sandbox("s", "/workspace");
        let runs = Cell::new(0);
        let frees = Cell::new(0);
        let run = || {
            runs.set(runs.get() + 1);
            if runs.get() == 1 {
                (Verdict::Broken, format!("{KILLED_FOR_MEMORY} (SIGKILL).\n\nKilled"))
            } else {
                (Verdict::Failed, "2 errors".to_string())
            }
        };
        let free = |_: &Workdir| {
            frees.set(frees.get() + 1);
            Ok(MemoryFreed { before_mb: Some(300), after_mb: Some(3500), stopped: vec!["7 node vite".to_string()], notes: vec![] })
        };
        assert_eq!(run_command_with(&cwd, &run, &free), (Verdict::Failed, "2 errors".to_string()));
        assert_eq!((runs.get(), frees.get()), (2, 1));
    }

    #[test]
    fn a_sandbox_check_killed_twice_is_broken_and_says_what_was_freed() {
        let cwd = Workdir::sandbox("s", "/workspace");
        let run = || (Verdict::Broken, format!("{KILLED_FOR_MEMORY} (SIGKILL).\n\nKilled"));
        let free = |_: &Workdir| Ok(MemoryFreed { before_mb: Some(300), after_mb: Some(3500), stopped: vec!["7 node vite".to_string()], notes: vec![] });
        let (verdict, out) = run_command_with(&cwd, &run, &free);
        assert_eq!(verdict, Verdict::Broken);
        assert!(out.starts_with(KILLED_FOR_MEMORY), "{out}");
        assert!(out.contains("killed for memory twice"), "{out}");
        assert!(out.contains("stopped 7 node vite"), "{out}");
        assert!(out.contains("300 MB before, 3500 MB after"), "{out}");
        // A failure to free is reported, not hidden.
        let bad = |_: &Workdir| Err(anyhow::anyhow!("relay down"));
        let (_, out) = run_command_with(&cwd, &|| (Verdict::Broken, format!("{KILLED_FOR_MEMORY} (SIGKILL).")), &bad);
        assert!(out.starts_with(KILLED_FOR_MEMORY), "{out}");
        assert!(out.contains("could not, so it did not run it again: relay down"), "{out}");
    }

    #[test]
    fn a_kill_outside_a_sandbox_never_frees_memory_or_retries() {
        let cwd = Workdir::host("/tmp");
        let calls = std::cell::Cell::new(0);
        let run = || {
            calls.set(calls.get() + 1);
            (Verdict::Broken, format!("{KILLED_FOR_MEMORY} (SIGKILL)."))
        };
        let free = |_: &Workdir| -> Result<MemoryFreed> { panic!("must not free memory on the user's machine") };
        assert_eq!(run_command_with(&cwd, &run, &free).0, Verdict::Broken);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn the_freed_report_is_parsed_and_an_unfinished_script_is_an_error() {
        let ok = "@@tod-free-before 300\nstopped 7 node /x/vite\n@@tod-free-after 3500\n";
        let f = parse_freed(ok).unwrap();
        assert_eq!((f.before_mb, f.after_mb, f.stopped.len()), (Some(300), Some(3500), 1));
        assert!(parse_freed("@@tod-free-before 300\nsh: boom\n").is_err());
        let err = parse_freed("@@tod-free-before 1\nnot-stopped 9: Operation not permitted\n@@tod-free-after 1\n").unwrap_err();
        assert!(format!("{err}").contains("could not stop 9: Operation not permitted"), "{err}");
    }

    fn dev_server(cmd: &str) -> bool {
        let script = format!("{DEV_SERVER_MATCH}\ndev_server_match \"$1\"");
        std::process::Command::new("sh").args(["-c", &script, "sh", cmd]).status().unwrap().success()
    }

    #[test]
    fn the_freeing_script_is_valid_shell() {
        let ok = std::process::Command::new("sh").args(["-n", "-c", &free_memory_script()]).status().unwrap();
        assert!(ok.success());
    }

    #[test]
    fn only_the_workspaces_dev_servers_match() {
        for yes in [
            "/workspace/flagship/.venv/bin/python manage.py runserver 0.0.0.0:9000",
            "/workspace/kb/.venv/bin/python -m uvicorn app.main:app --host 0.0.0.0 --port 8051",
            "/usr/bin/python3 -m gunicorn app:app",
            "/workspace/kb/.venv/bin/gunicorn app:app -w 2",
            "node /workspace/ui/node_modules/.bin/vite --host",
            "/usr/local/bin/node --max-old-space-size=4096 /workspace/ui/node_modules/vite/bin/vite.js",
        ] {
            assert!(dev_server(yes), "should match: {yes}");
        }
        for no in [
            "grep -r uvicorn /workspace",
            "tail -f /var/log/uvicorn.log",
            "node /workspace/ui/node_modules/.bin/vitest run",
            "node /workspace/ui/node_modules/vitest/vitest.mjs run",
            "sh -c python manage.py runserver",
            "/workspace/flagship/.venv/bin/python manage.py migrate",
            "/workspace/flagship/.venv/bin/python -m pytest -k uvicorn",
            "python -m basedpyright",
            "vim vite.config.ts",
            "claude --resume x",
            "/opt/tod/tod-relay --listen vite",
            "",
        ] {
            assert!(!dev_server(no), "should not match: {no}");
        }
    }

    #[test]
    fn a_v8_heap_exhaustion_is_killed_for_memory() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        let check = Check {
            command: "echo 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory'; exit 134".to_string(),
            ..Check::default()
        };
        let (verdict, out) = run_command(&cwd, &check, &[]);
        assert_eq!(verdict, Verdict::Broken, "{out}");
        assert!(out.starts_with("Killed for memory (the process ran out of memory)."), "{out}");
    }

    #[test]
    fn the_memory_line_is_split_off_and_reported() {
        assert_eq!(split_memory(b"@@tod-mem 8000 3400\nhello\n"), (Some((8000, 3400)), b"hello\n".to_vec()));
        assert_eq!(split_memory(b"hello"), (None, b"hello".to_vec()));
        // Stripped wherever it is: an error from a missing `awk` may come first.
        assert_eq!(split_memory(b"sh: awk: not found\n@@tod-mem \nhi\n"), (None, b"sh: awk: not found\nhi\n".to_vec()));
        let message = memory_message(137, Some((8000, 3400)), "sandbox s", "tsc --noEmit");
        assert!(message.contains("It ran in sandbox s, which has 8000 MB; 3400 MB were free when the check started."));
        assert!(message.contains("Command: `tsc --noEmit`."));
        assert!(!memory_message(137, None, "this machine", "x").contains(" MB"));
    }

    #[test]
    fn a_failure_that_only_mentions_memory_stays_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        let check = |command: &str| Check { command: command.to_string(), ..Check::default() };
        let (verdict, out) = run_command(&cwd, &check("echo 'test_big ... MemoryError caught'; echo 'std::bad_alloc'; echo 'Killed'; echo '1 failed'; exit 1"), &[]);
        assert_eq!(verdict, Verdict::Failed, "{out}");
        // An uncaught Python MemoryError is the traceback's last line.
        let (verdict, _) = run_command(&cwd, &check("echo Traceback; echo MemoryError; exit 1"), &[]);
        assert_eq!(verdict, Verdict::Broken);
        // Text needles with the wrong exit code do not count.
        assert!(!killed_for_memory(1, "JavaScript heap out of memory"));
        // The shell's "Killed" counts as the last line only.
        assert!(killed_for_memory(2, "build\n/bin/sh: 1: Killed\n"));
    }

    #[test]
    fn a_broken_run_is_never_reused_but_a_verdict_is() {
        let run = |verdict| CheckRun {
            node_id: Uuid::nil(),
            phase: "review".into(),
            dir: String::new(),
            command: "x".into(),
            label: String::new(),
            fingerprint: "f".into(),
            verdict,
            output: "the command did not finish in 10 minutes and was stopped".into(),
            ran_at: 0,
            tree_key: String::new(),
        };
        assert!(!reusable(&run(Verdict::Broken)));
        assert!(reusable(&run(Verdict::Passed)) && reusable(&run(Verdict::Failed)) && reusable(&run(Verdict::Skipped)));
        assert!(broken_why(&run(Verdict::Broken)).contains("a fresh run, ended 1970-01-01 00:00:00 UTC"));
    }

    #[test]
    fn a_memory_kill_is_not_reused_as_the_answer_for_unchanged_files() {
        let run = |verdict, output: &str| CheckRun {
            node_id: Uuid::nil(),
            phase: "review".into(),
            dir: String::new(),
            command: "x".into(),
            label: String::new(),
            fingerprint: "f".into(),
            verdict,
            output: output.into(),
            ran_at: tod_store::check_runs::now(),
            tree_key: String::new(),
        };
        assert!(is_memory_kill(&run(Verdict::Broken, "Killed for memory (SIGKILL).")));
        assert!(!is_memory_kill(&run(Verdict::Broken, "no such directory")));
        assert!(!is_memory_kill(&run(Verdict::Failed, "Killed for memory")));
    }

    #[test]
    fn a_missing_directory_is_broken_not_failed() {
        let cwd = Workdir::host("/no/such/dir/for/checks");
        let (verdict, why) = run_command(&cwd, &Check { command: "true".to_string(), ..Check::default() }, &[]);
        assert_eq!(verdict, Verdict::Broken, "{why}");
    }

    #[test]
    fn the_fingerprint_changes_with_committed_uncommitted_and_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        cwd.git(&["init", "-q"]).unwrap();
        cwd.git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "first"]).unwrap();
        let clean = fingerprint(&cwd).unwrap();
        assert_eq!(clean, fingerprint(&cwd).unwrap());
        std::fs::write(dir.path().join("new.txt"), "one").unwrap();
        let untracked = fingerprint(&cwd).unwrap();
        assert_ne!(clean, untracked);
        std::fs::write(dir.path().join("new.txt"), "two").unwrap();
        assert_ne!(untracked, fingerprint(&cwd).unwrap(), "an untracked file's content counts");
        cwd.git(&["add", "new.txt"]).unwrap();
        cwd.git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "second"]).unwrap();
        let committed = fingerprint(&cwd).unwrap();
        std::fs::write(dir.path().join("new.txt"), "three").unwrap();
        assert_ne!(committed, fingerprint(&cwd).unwrap(), "an edit to a tracked file counts");
    }

    fn commit(cwd: &Workdir, message: &str) {
        cwd.git(&["add", "-A"]).unwrap();
        cwd.git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", message]).unwrap();
    }

    /// A repository whose `origin/main` is its first commit, with `a/x.py`,
    /// `a/y.txt` and `b/z.py`.
    fn base_repo() -> (tempfile::TempDir, Workdir) {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        cwd.git(&["init", "-q", "-b", "main"]).unwrap();
        for f in ["a/x.py", "a/y.txt", "b/z.py"] {
            std::fs::create_dir_all(dir.path().join(f).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(f), "1").unwrap();
        }
        commit(&cwd, "first");
        cwd.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]).unwrap();
        (dir, cwd)
    }

    fn scoped(dir: &str, changed: &str, pattern: &str) -> Check {
        Check { command: "true".into(), dir: dir.into(), changed: changed.into(), pattern: pattern.into(), ..Check::default() }
    }

    #[test]
    fn globs_match_whole_paths_with_star_crossing_directories() {
        assert!(glob_match("*.py", "x.py"));
        assert!(glob_match("*.py", "pkg/sub/x.py"));
        assert!(!glob_match("*.py", "x.pyc"));
        assert!(glob_match("src/*.ts", "src/a/b.ts"));
        assert!(glob_match("?.md", "a.md"));
        assert!(!glob_match("?.md", "ab.md"));
    }

    #[test]
    fn a_scoped_check_applies_only_when_its_path_and_pattern_saw_a_change() {
        let (dir, cwd) = base_repo();
        let tree = || Tree::new(&cwd).unwrap();
        // Nothing changed since origin/main: scoped checks do not apply, an unscoped one does.
        assert!(!assess(&tree(), &scoped("a", "", "")).unwrap().applicable);
        assert!(assess(&tree(), &scoped("", "", "")).unwrap().applicable);

        // A committed change and an uncommitted one in `a`, an untracked one in `b`.
        std::fs::write(dir.path().join("a/x.py"), "2").unwrap();
        commit(&cwd, "edit x");
        std::fs::write(dir.path().join("a/y.txt"), "2").unwrap();
        std::fs::write(dir.path().join("b/new.py"), "1").unwrap();

        let a = assess(&tree(), &scoped("a", "", "")).unwrap();
        assert!(a.applicable);
        assert_eq!(a.files, ["x.py", "y.txt"], "relative to the check's directory");
        let a_py = assess(&tree(), &scoped("a", "", "*.py")).unwrap();
        assert_eq!(a_py.files, ["x.py"]);
        let b = assess(&tree(), &scoped("b", "", "*.py")).unwrap();
        assert_eq!(b.files, ["new.py"], "an untracked file counts");
        // A pattern with no match in the scope: not applicable.
        assert!(!assess(&tree(), &scoped("b", "", "*.rs")).unwrap().applicable);
        // `changed` overrides `dir` as the scope.
        let wide = assess(&tree(), &scoped("a", ".", "*.py")).unwrap();
        assert_eq!(wide.files, ["x.py"], "files outside the directory apply the check but are not handed to it");
        assert!(wide.applicable);

        // A deletion applies a check and is not handed over as a file.
        std::fs::remove_file(dir.path().join("b/z.py")).unwrap();
        let b = assess(&tree(), &scoped("b", "", "*.py")).unwrap();
        assert!(b.applicable);
        assert_eq!(b.files, ["new.py"]);
    }

    #[test]
    fn a_checks_fingerprint_moves_only_with_what_it_reads() {
        let (dir, cwd) = base_repo();
        std::fs::write(dir.path().join("a/x.py"), "2").unwrap();
        let before = assess(&Tree::new(&cwd).unwrap(), &scoped("a", "", "*.py")).unwrap();
        // Same files for the check: same fingerprint, even with the check's own repo edited elsewhere
        // only if the file list is unchanged and the repo is not (one repository here, so it moves).
        assert_eq!(before, assess(&Tree::new(&cwd).unwrap(), &scoped("a", "", "*.py")).unwrap());
        std::fs::write(dir.path().join("a/x.py"), "3").unwrap();
        let after = assess(&Tree::new(&cwd).unwrap(), &scoped("a", "", "*.py")).unwrap();
        assert_ne!(before.fingerprint, after.fingerprint);
    }

    #[test]
    fn without_an_origin_default_branch_a_scoped_check_errors_instead_of_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        cwd.git(&["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.path().join("f"), "1").unwrap();
        commit(&cwd, "first");
        let err = assess(&Tree::new(&cwd).unwrap(), &scoped("a", "", "")).unwrap_err();
        assert!(format!("{err:#}").contains("origin"), "{err:#}");
        // An unscoped check needs no base.
        assert!(assess(&Tree::new(&cwd).unwrap(), &scoped("", "", "")).unwrap().applicable);
    }

    #[test]
    fn changed_files_reach_the_command_as_one_quoted_variable() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(dir.path());
        let check = Check { command: "printf '%s' \"$TOD_CHANGED_FILES\"".into(), ..Check::default() };
        let files = vec!["a b.py".to_string(), "it's.py".to_string()];
        assert_eq!(run_command(&cwd, &check, &files), (Verdict::Passed, "a b.py\nit's.py".to_string()));
    }
}
