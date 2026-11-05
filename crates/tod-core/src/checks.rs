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

/// What the files at `cwd` look like: for the repository and each submodule,
/// its `HEAD`, its diff against `HEAD`, and its untracked files' contents.
/// Equal fingerprints mean nothing a check could read has changed.
pub fn fingerprint(cwd: &Workdir) -> Result<String> {
    let mut repos = vec![cwd.clone()];
    repos.extend(worktree::submodule_dirs(cwd).context("list the submodules")?);
    let mut hasher = Sha256::new();
    for repo in &repos {
        hasher.update(format!("repo {repo}\n"));
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
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
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

/// Run one check at `cwd`: its command through the shell, stderr folded into
/// stdout. Exit 0 passes, any other exit fails; a command that cannot be run
/// or outlasts [`CHECK_TIMEOUT`] is broken.
pub fn run_command(cwd: &Workdir, check: &Check) -> (Verdict, String) {
    // `exec 2>&1` makes one stream of what a person would see in a terminal.
    let script = format!("exec 2>&1\n{}", check.command);
    let result = match cwd {
        Workdir::Host(path) => {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", &script]).current_dir(path);
            run_with_timeout(command, CHECK_TIMEOUT, &path.display().to_string())
        }
        Workdir::Sandbox { .. } => cwd.output_within(CHECK_TIMEOUT, "sh", &["-c", &script]).map(|o| (o.status.code(), o.stdout)),
        Workdir::Container { container, path } => run_in_container(container, path, &script),
    };
    match result {
        Ok((Some(0), out)) => (Verdict::Passed, tail(&out)),
        Ok((Some(_), out)) => (Verdict::Failed, tail(&out)),
        Ok((None, out)) => (Verdict::Broken, format!("the command was stopped by a signal\n{}", tail(&out))),
        Err(err) => (Verdict::Broken, format!("{err:#}")),
    }
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
    Ok((status.code(), buf))
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
        self.runs.iter().all(|r| r.verdict == Verdict::Passed)
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
    let fingerprint = fingerprint(&cwd)?;
    let mut round = Round { fingerprint: fingerprint.clone(), runs: Vec::new() };
    for check in &checks {
        let latest = fleet.read(|conn| CheckRunRepo::new(conn).latest(node, key, &check.command))?;
        let run = match latest {
            Some(run) if run.fingerprint == fingerprint && run.label == check.label => run,
            _ => {
                let (verdict, output) = run_command(&cwd, check);
                let run = CheckRun {
                    node_id: node,
                    phase: key.to_string(),
                    command: check.command.clone(),
                    label: check.label.clone(),
                    fingerprint: fingerprint.clone(),
                    verdict,
                    output,
                    ran_at: tod_store::check_runs::now(),
                };
                fleet.enqueue(tod_store::fleet::FleetMutation::RecordCheckRun(Box::new(run.clone())))?;
                fleet.writer().flush()?;
                run
            }
        };
        round.runs.push(run);
    }
    Ok(round)
}

/// The name a run goes by in messages: its label, else its command.
pub fn run_name(run: &CheckRun) -> &str {
    let label = run.label.trim();
    if label.is_empty() { run.command.trim() } else { label }
}

/// The gate criteria for the checks configured for `state`, answered from
/// each one's latest run. These are not rows of the criteria catalog, so they
/// are never saved as evaluations (`settle_gate` leaves them out).
pub fn gate_criteria(conn: &rusqlite::Connection, node: Uuid, state: &str, to: &str) -> Result<Vec<CriterionCheck>> {
    let Some(key) = phase_key(state) else { return Ok(Vec::new()) };
    let repo = CheckRunRepo::new(conn);
    let mut out = Vec::new();
    for (index, check) in configured(conn, node, state)?.into_iter().enumerate() {
        let (outcome, detail) = match repo.latest(node, key, &check.command)? {
            None => (OUTCOME_FAIL, "has not run yet".to_string()),
            Some(run) => match run.verdict {
                Verdict::Passed => (OUTCOME_PASS, String::new()),
                Verdict::Failed => (OUTCOME_FAIL, format!("failed\n{}", run.output)),
                Verdict::Broken => (OUTCOME_FAIL, format!("could not run: {}", run.output)),
            },
        };
        out.push(CriterionCheck {
            criterion: GateCriterion {
                id: criterion_id(node, key, &check.command),
                from_state: state.to_string(),
                to_state: to.to_string(),
                slug: format!("{SLUG_PREFIX}{}", slug(check.name())),
                label: format!("Check: {}", check.name()),
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

fn criterion_id(node: Uuid, phase: &str, command: &str) -> Uuid {
    let digest = Sha256::digest(format!("check\0{node}\0{phase}\0{command}"));
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
        let summary = finding_summary(run_name(run));
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
                        "The phase's check `{}` (`{}`) exited non-zero. Fix what it reports; the app runs it again after your change.\n\n{}",
                        run_name(run),
                        run.command,
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
        let check = |command: &str| Check { label: String::new(), command: command.to_string() };
        assert_eq!(run_command(&cwd, &check("echo fine")), (Verdict::Passed, "fine".to_string()));
        assert_eq!(run_command(&cwd, &check("echo bad >&2; exit 3")), (Verdict::Failed, "bad".to_string()));
        // Output on success is kept too, and a missing command is a failure of the
        // command (exit 127), not of running it.
        assert_eq!(run_command(&cwd, &check("no-such-command-here")).0, Verdict::Failed);
    }

    #[test]
    fn a_missing_directory_is_broken_not_failed() {
        let cwd = Workdir::host("/no/such/dir/for/checks");
        let (verdict, why) = run_command(&cwd, &Check { label: String::new(), command: "true".to_string() });
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
}
