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
fn glob_match(pattern: &str, text: &str) -> bool {
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
const MEM_PROBE: &str = "if [ -r /proc/meminfo ]; then echo \"@@tod-mem $(awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{print int(t/1024), int(a/1024)}' /proc/meminfo)\"; fi";

/// Split the memory line the script printed off the output: (total MB,
/// available MB) and the rest.
fn split_memory(out: &[u8]) -> (Option<(u64, u64)>, &[u8]) {
    let marker = MEM_MARKER.as_bytes();
    if !out.starts_with(marker) {
        return (None, out);
    }
    let end = out.iter().position(|b| *b == b'\n').map_or(out.len(), |i| i + 1);
    let line = String::from_utf8_lossy(&out[marker.len()..end]);
    let mut nums = line.split_whitespace().map(|n| n.parse::<u64>());
    let mem = match (nums.next(), nums.next()) {
        (Some(Ok(total)), Some(Ok(free))) => Some((total, free)),
        _ => None,
    };
    (mem, &out[end..])
}

/// Output that says the process ran out of memory itself (not killed from
/// outside): V8's heap limit, Python's `MemoryError`, a C++ `bad_alloc`.
fn shows_out_of_memory(text: &str) -> bool {
    ["JavaScript heap out of memory", "MemoryError", "std::bad_alloc", "Cannot allocate memory", "Allocation failed - JavaScript heap"]
        .iter()
        .any(|needle| text.contains(needle))
}

/// Whether a non-zero exit was the machine running out of memory: a kill by
/// SIGKILL (exit 137 = 128 + 9: how the shell, `docker exec` and the relay
/// report one; the host's own signal is mapped to it too), the shell's
/// "Killed", or output showing the heap running out.
fn killed_for_memory(code: i32, text: &str) -> bool {
    code == 137 || text.lines().any(|l| l.trim() == "Killed" || l.trim_end().ends_with(" Killed")) || shows_out_of_memory(text)
}

/// The first line of a memory failure's output.
fn memory_message(code: i32, text: &str, mem: Option<(u64, u64)>) -> String {
    let how = if code == 137 {
        "SIGKILL, likely the out-of-memory killer"
    } else if shows_out_of_memory(text) {
        "the process ran out of memory"
    } else {
        "the shell reported it killed, likely by the out-of-memory killer"
    };
    let machine = match mem {
        Some((total, free)) => format!(" The machine has {total} MB; {free} MB were free when the check started."),
        None => String::new(),
    };
    format!("{KILLED_FOR_MEMORY} ({how}).{machine} The code may be fine: lower the check's heap, scope it, or remove it.")
}

/// Run one check at `cwd`: its command through the shell, stderr folded into
/// stdout. Exit 0 passes, any other exit fails; a command that cannot be run
/// or outlasts [`CHECK_TIMEOUT`] is broken, and so is one killed for memory
/// (an environment problem for the user, never a finding for the Fix agent).
pub fn run_command(cwd: &Workdir, check: &Check, files: &[String]) -> (Verdict, String) {
    // `exec 2>&1` makes one stream of what a person would see in a terminal.
    let script = format!("exec 2>&1\n{MEM_PROBE}\nexport TOD_CHANGED_FILES={}\n{}", shell_quote(&files.join("\n")), check.command);
    let result = match cwd {
        Workdir::Host(path) => {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", &script]).current_dir(path);
            run_with_timeout(command, CHECK_TIMEOUT, &path.display().to_string())
        }
        Workdir::Sandbox { .. } => cwd.output_within(CHECK_TIMEOUT, "sh", &["-c", &script]).map(|o| (exit_code(&o.status), o.stdout)),
        Workdir::Container { container, path } => run_in_container(container, path, &script),
    };
    match result {
        Ok((Some(code), out)) => {
            let (mem, rest) = split_memory(&out);
            let text = tail(rest);
            if code == 0 {
                (Verdict::Passed, text)
            } else if killed_for_memory(code, &text) {
                (Verdict::Broken, format!("{}\n\n{text}", memory_message(code, &text, mem)))
            } else {
                (Verdict::Failed, text)
            }
        }
        Ok((None, out)) => (Verdict::Broken, format!("the command was stopped by a signal\n{}", tail(split_memory(&out).1))),
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
fn run_in_container(container: &str, path: &str, script: &str) -> Result<(Option<i32>, Vec<u8>)> {
    let secs = CHECK_TIMEOUT.as_secs();
    let wrapper = format!(
        "if command -v timeout >/dev/null 2>&1; then exec timeout -k 5 {secs} sh -c \"$1\"; else exec sh -c \"$1\"; fi"
    );
    let exec = tod_agent::devcontainer::ContainerExec::connect(container)?;
    let command = exec.command(path, "sh", &["-c", &wrapper, "sh", script])?;
    let (code, out) = run_with_timeout(command, CHECK_TIMEOUT + Duration::from_secs(30), container)?;
    if code == Some(124) {
        anyhow::bail!("the command did not finish in {} minutes and was stopped", secs / 60);
    }
    Ok((code, out))
}

/// Run `command` (stdin closed, stderr dropped: the script folds it into
/// stdout) and collect its output, killing it after `limit`.
fn run_with_timeout(mut command: std::process::Command, limit: Duration, what: &str) -> Result<(Option<i32>, Vec<u8>)> {
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
            anyhow::bail!("the command did not finish in {} minutes and was stopped", CHECK_TIMEOUT.as_secs() / 60);
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

/// Run the checks configured for `state` whose latest run was against other
/// files than these (or never happened), and return every check's latest run.
/// Does nothing, and returns an empty round, when none is configured. Git,
/// Docker, or a sandbox may be involved: never call it on the UI thread.
pub fn run_stale(fleet: &FleetStore, node: Uuid, state: &str) -> Result<Round> {
    let Some(key) = phase_key(state) else { return Ok(Round::default()) };
    let checks = fleet.read(|conn| configured(conn, node, state))?;
    if checks.is_empty() {
        return Ok(Round::default());
    }
    let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string())
        .with_context(|| format!("find the files to run the {state} checks in"))?;
    let tree = Tree::new(&cwd)?;
    let mut round = Round { fingerprint: tree.fingerprint_all()?, runs: Vec::new() };
    // The commands this round actually ran, for the node's activity.
    let mut ran: Vec<tod_store::node_timeline::CommandResult> = Vec::new();
    for check in &checks {
        let dir = clean_dir(&check.dir);
        let assessment = assess(&tree, check)?;
        let latest = fleet.read(|conn| CheckRunRepo::new(conn).latest(node, key, &dir, &check.command))?;
        let run = match latest {
            Some(run) if run.fingerprint == assessment.fingerprint && run.label == check.label => run,
            _ => {
                let (verdict, output) = if assessment.applicable {
                    run_command(&cwd.join(&dir), check, &assessment.files)
                } else {
                    (Verdict::Skipped, format!("nothing changed in {}", check.scope()))
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
    Ok(round)
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
                Verdict::Broken => (OUTCOME_FAIL, format!("could not run: {}", run.output)),
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
                    severity: "high".to_string(),
                    file: None,
                    line: None,
                    summary,
                    detail: Some(format!(
                        "The phase's check `{}` (`{}`{}) exited non-zero. Fix what it reports; the app runs it again after your change.\n\n{}",
                        run_name(run),
                        run.command,
                        if run.dir.is_empty() { String::new() } else { format!(", run in `{}`", run.dir) },
                        run.output
                    )),
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
        // The shell's own exit 137 reads the same, as it does through a relay.
        assert_eq!(run_command(&cwd, &check("exit 137"), &[]).0, Verdict::Broken);
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
        assert_eq!(split_memory(b"@@tod-mem 8000 3400\nhello\n"), (Some((8000, 3400)), &b"hello\n"[..]));
        assert_eq!(split_memory(b"hello"), (None, &b"hello"[..]));
        let message = memory_message(137, "", Some((8000, 3400)));
        assert!(message.contains("The machine has 8000 MB; 3400 MB were free when the check started."));
        assert!(!memory_message(137, "", None).contains("MB"));
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
