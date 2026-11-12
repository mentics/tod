//! `SandboxHealth`: whether a node's sandbox can do its job, decided by
//! asking it, never by Blaxel's `status = DEPLOYED` (which a sandbox in
//! standby, half-made, with a full disk, or with a dead relay all show).
//!
//! The probe runs after a sandbox is created, forked, given new proxy rules
//! or woken, and before one is reused. It stops at the first failing check
//! and the error names which check it was ([`Check`]), so "the sandbox is
//! unhealthy" is never the whole message.
//!
//! The checks, in order:
//! 1. [`Check::Reachable`]: the workload answers (not Blaxel's `WORKLOAD_UNAVAILABLE` 404).
//! 2. [`Check::Disk`]: free space on `/` ([`Blaxel::ensure_free_space`]: cleans tod's own leftovers once).
//! 3. [`Check::Relay`] and [`Check::Manifest`] (stage [`Stage::Ready`] only):
//!    the relay answers, and `/opt/tod/manifest` is a node's (and equals the
//!    one the caller just installed, when it says which).
//! 4. [`Check::Env`] and [`Check::GitRemote`]: the node's environment is in
//!    effect and `git ls-remote` reads the repository through the proxy
//!    ([`crate::node::probe_script`]). Steps 3 and 4 are retried after each of
//!    `probe_delays`: a fork's proxy rules and environment land by a second
//!    call, and a relay just restarted takes a moment to answer.

use crate::blaxel::{Blaxel, ProcessResult};
use crate::node::{PROBE_NO_NODE_ENV, probe_script};
use crate::provision::{is_node_manifest, relay_manifest};
use crate::relay::shell_quote;
use anyhow::Result;
use std::time::Duration;

/// One thing the probe checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Reachable,
    Disk,
    Relay,
    Manifest,
    Env,
    GitRemote,
}

impl Check {
    pub fn name(self) -> &'static str {
        match self {
            Check::Reachable => "reachable",
            Check::Disk => "disk free",
            Check::Relay => "relay up",
            Check::Manifest => "manifest",
            Check::Env => "node environment",
            Check::GitRemote => "git ls-remote through the proxy",
        }
    }
}

/// Whether the sandbox was made by the setup that is probing it, or already
/// existed. What the user does next differs: a new sandbox that fails is kept
/// and the next Start deletes it and makes another (`NameDecision::ReplaceFailed`);
/// an existing one is kept by Start, which installs tod's files into it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// Made in this Start attempt.
    New,
    /// Recorded earlier and reused.
    Existing,
}

/// The failed check and what it saw.
#[derive(Debug)]
pub struct HealthFailure {
    pub check: Check,
    pub detail: String,
    /// [`Check::Manifest`] only: the sandbox reports a manifest that is not a
    /// node's at all (a plain sandbox), rather than another node build's.
    pub plain: bool,
    pub setup: Setup,
}

/// The marker the task panel looks for: a text that has it gets no generic
/// next step appended (`cloud_node::describe`). A failure while a Start makes
/// a sandbox is wrapped by `discard_failed_sandbox`, which owns the step then
/// (it deletes a temporary failure's sandbox at once and keeps another); only
/// an existing sandbox's failure says its own.
pub const WHAT_TO_DO: &str = "What to do:";

impl HealthFailure {
    /// What is wrong and the likely cause, in plain words.
    pub fn problem(&self) -> &'static str {
        match self.check {
            Check::Reachable => "The sandbox did not answer. It may still be starting, or it has stopped.",
            Check::Disk => "The sandbox is out of disk space, so tod cannot work in it. tod already removed its own leftovers.",
            Check::Relay => {
                "The sandbox was made but tod's relay (the program tod talks to inside it) is not running. \
                 The setup was probably cut short."
            }
            Check::Manifest if self.plain => {
                "The sandbox was made but tod's files in it are incomplete: it was set up as a plain \
                 sandbox, not a node sandbox. This happens when a fork loses tod's files or two setups overlap."
            }
            Check::Manifest => {
                "The sandbox has tod's files from a different build than this one. It was probably made \
                 before tod was updated."
            }
            Check::Env => {
                "The sandbox was made but tod's settings for this node did not take effect in it. A fork \
                 may still be applying them."
            }
            Check::GitRemote => {
                "The sandbox cannot read the project's repository, so it cannot check the code out. The \
                 GitHub token may be missing or expired, or the sandbox's network proxy is not working."
            }
        }
    }

    /// The next step. For an existing sandbox, a "What to do:" step naming the
    /// runner menu action. For one this Start made, only a hint (no marker): the
    /// step itself is `discard_failed_sandbox`'s, which knows whether it kept or
    /// deleted the sandbox, and a second one would contradict it.
    pub fn next_step(&self) -> &'static str {
        match (self.check, self.setup) {
            (Check::GitRemote, Setup::New) => {
                "To fix the token, use the red \"GitHub token\" banner's Add token or Replace token button \
                 (the banner is absent when the token could not be checked, e.g. offline); otherwise check the repository URL."
            }
            (Check::GitRemote, Setup::Existing) => {
                "What to do: add or replace the GitHub token with the red \"GitHub token\" banner's Add token \
                 or Replace token button (the banner is absent when the token could not be checked, e.g. offline; \
                 then check the repository URL), then use Reset environment in the task panel's runner menu \
                 to set the sandbox up again."
            }
            (Check::Manifest, Setup::New) if !self.plain => {
                "If new sandboxes keep coming out old, refresh the project's golden sandbox with `tod-sandbox golden refresh`."
            }
            (Check::Manifest, Setup::Existing) if !self.plain => {
                "What to do: use Reset environment in the task panel's runner menu to install this build's \
                 files again. If it fails the same way, run `tod-sandbox golden refresh` for the project's \
                 golden sandbox, then use Delete sandbox (it saves the work first) and Start for a new one."
            }
            (Check::Disk, Setup::New) => "If it fills up again, give the sandbox more disk or use a smaller image.",
            (Check::Disk, Setup::Existing) => {
                "What to do: use Delete sandbox in the task panel's runner menu (it saves the work first), \
                 then Start makes a new one. If that fills up too, give the sandbox more disk or use a smaller image."
            }
            (_, Setup::New) => "",
            (_, Setup::Existing) => {
                "What to do: use Reset environment in the task panel's runner menu to install tod's files \
                 again and check once more. If it fails the same way, use Delete sandbox (it saves the work \
                 first); the next Start makes a new one."
            }
        }
    }
}

impl std::fmt::Display for HealthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}\nTechnical detail: sandbox health check \"{}\" failed: {}",
            self.problem(),
            if self.next_step().is_empty() { "" } else { " " },
            self.next_step(),
            self.check.name(),
            self.detail
        )
    }
}

impl std::error::Error for HealthFailure {}

/// How much of the probe to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// A sandbox just made, before tod's files are in it: no relay yet.
    Base,
    /// A sandbox that has been provisioned: everything.
    Ready,
}

/// What the probe is checking against.
pub struct HealthSpec<'a> {
    /// The sandbox's name, for messages.
    pub label: &'a str,
    pub node: &'a str,
    pub repo_url: &'a str,
    /// Free bytes needed on `/`.
    pub min_free: u64,
    /// How long to wait for the workload to answer at all.
    pub reachable_wait: Duration,
    /// The manifest just installed, when the caller knows it; else any
    /// node manifest is sane.
    pub expected_manifest: Option<&'a str>,
    /// Seconds to wait between tries of the relay, manifest, environment and `git ls-remote` checks.
    pub probe_delays: &'a [u64],
    /// Whether this setup made the sandbox; decides the next step in a failure.
    pub setup: Setup,
}

/// What the probe asks the sandbox; [`Blaxel`] answers it for real.
pub trait Probes: Sync {
    fn reachable(&self, wait: Duration) -> Result<()>;
    fn disk(&self, min_free: u64, label: &str) -> Result<()>;
    /// The manifest the relay reports, `None` if it does not answer.
    fn relay_manifest(&self) -> Option<String>;
    /// Runs [`probe_script`].
    fn env_and_git(&self, script: &str) -> Result<ProcessResult>;
}

struct Live<'a> {
    bx: &'a Blaxel,
    url: &'a str,
}

impl Probes for Live<'_> {
    fn reachable(&self, wait: Duration) -> Result<()> {
        self.bx.wait_reachable(self.url, wait)
    }
    fn disk(&self, min_free: u64, label: &str) -> Result<()> {
        self.bx.ensure_free_space(self.url, "/", min_free, label)
    }
    fn relay_manifest(&self) -> Option<String> {
        relay_manifest(self.bx, self.url)
    }
    fn env_and_git(&self, script: &str) -> Result<ProcessResult> {
        self.bx.run(self.url, &format!("sh -c {}", shell_quote(script)), 60)
    }
}

/// Probes the sandbox at `url`; see the module comment.
pub fn probe(
    bx: &Blaxel,
    url: &str,
    stage: Stage,
    spec: &HealthSpec,
    progress: &mut dyn FnMut(&str),
) -> Result<(), HealthFailure> {
    evaluate(&Live { bx, url }, stage, spec, &mut |d| std::thread::sleep(d), progress)
}

/// [`probe`] over any [`Probes`], with the sleep given.
pub fn evaluate(
    probes: &dyn Probes,
    stage: Stage,
    spec: &HealthSpec,
    sleep: &mut dyn FnMut(Duration),
    progress: &mut dyn FnMut(&str),
) -> Result<(), HealthFailure> {
    let fail = |check: Check, detail: String| HealthFailure { check, detail, plain: false, setup: spec.setup };
    probes.reachable(spec.reachable_wait).map_err(|e| fail(Check::Reachable, format!("{}: {e:#}", spec.label)))?;
    let script = probe_script(spec.repo_url, spec.node);
    // Each check is a round trip to the sandbox, so they are asked together
    // and judged in the order they always were: disk, then the relay's
    // manifest, then the environment and the repository.
    let once = || -> Result<(), HealthFailure> {
        let (disk, manifest, env) = std::thread::scope(|scope| {
            let disk = scope.spawn(|| probes.disk(spec.min_free, spec.label));
            let manifest = (stage == Stage::Ready).then(|| scope.spawn(|| probes.relay_manifest()));
            let env = scope.spawn(|| probes.env_and_git(&script));
            (
                disk.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the disk check panicked"))),
                manifest.map(|m| m.join().unwrap_or(None)),
                env.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the environment check panicked"))),
            )
        });
        disk.map_err(|e| fail(Check::Disk, format!("{e:#}")))?;
        if let Some(reported) = manifest {
            let Some(reported) = reported else {
                return Err(fail(Check::Relay, format!("the relay in {} does not answer", spec.label)));
            };
            let sane = match spec.expected_manifest {
                Some(expected) => reported == expected,
                None => is_node_manifest(&reported),
            };
            if !sane {
                let mut failure = fail(
                    Check::Manifest,
                    format!(
                        "{} reports manifest {reported:?}, expected {}",
                        spec.label,
                        spec.expected_manifest.map_or("a node's".to_string(), |m| format!("{m:?}"))
                    ),
                );
                failure.plain = !is_node_manifest(&reported);
                return Err(failure);
            }
        }
        match env {
            Ok(res) if res.exit_code == 0 => Ok(()),
            Ok(res) if res.exit_code == PROBE_NO_NODE_ENV => Err(fail(Check::Env, res.output().trim().to_string())),
            Ok(res) => Err(fail(Check::GitRemote, format!("exit {}: {}", res.exit_code, res.output().trim()))),
            // A sandbox still coming up fails the call itself.
            Err(err) => Err(fail(Check::Reachable, format!("the process API of {}: {err:#}", spec.label))),
        }
    };
    let mut attempt = 0;
    loop {
        let Err(failure) = once() else { return Ok(()) };
        // A full disk is not waited out.
        if failure.check == Check::Disk {
            return Err(failure);
        }
        let Some(delay) = spec.probe_delays.get(attempt) else { return Err(failure) };
        progress(&format!(
            "{} is not healthy yet ({}: {}); trying again in {delay}s…",
            spec.label,
            failure.check.name(),
            failure.detail.lines().next().unwrap_or("")
        ));
        sleep(Duration::from_secs(*delay));
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        reachable: bool,
        disk: bool,
        manifest: Option<String>,
        runs: Mutex<Vec<(i32, &'static str)>>,
        ran: Mutex<u32>,
    }

    impl Fake {
        fn healthy() -> Self {
            Self { reachable: true, disk: true, manifest: Some("node relay=abc".into()), runs: Mutex::new(vec![]), ran: Mutex::new(0) }
        }
    }

    fn result(exit_code: i32, out: &str) -> ProcessResult {
        ProcessResult { exit_code, logs: Some(out.into()), status: None }
    }

    impl Probes for Fake {
        fn reachable(&self, _: Duration) -> Result<()> {
            if self.reachable { Ok(()) } else { anyhow::bail!("did not wake") }
        }
        fn disk(&self, _: u64, _: &str) -> Result<()> {
            if self.disk { Ok(()) } else { anyhow::bail!("sandbox disk full: 3 MB free") }
        }
        fn relay_manifest(&self) -> Option<String> {
            self.manifest.clone()
        }
        fn env_and_git(&self, _: &str) -> Result<ProcessResult> {
            *self.ran.lock().unwrap() += 1;
            let mut runs = self.runs.lock().unwrap();
            let (code, out) = if runs.is_empty() { (0, "") } else { runs.remove(0) };
            Ok(result(code, out))
        }
    }

    fn spec<'a>(expected: Option<&'a str>, delays: &'a [u64]) -> HealthSpec<'a> {
        HealthSpec {
            label: "node-x",
            node: "n1",
            repo_url: "https://github.com/o/r",
            min_free: 1,
            reachable_wait: Duration::from_millis(1),
            expected_manifest: expected,
            probe_delays: delays,
            setup: Setup::New,
        }
    }

    fn run(f: &Fake, stage: Stage, s: &HealthSpec) -> Result<(), HealthFailure> {
        evaluate(f, stage, s, &mut |_| {}, &mut |_| {})
    }

    #[test]
    fn a_healthy_sandbox_passes() {
        run(&Fake::healthy(), Stage::Ready, &spec(None, &[])).unwrap();
        run(&Fake::healthy(), Stage::Ready, &spec(Some("node relay=abc"), &[])).unwrap();
    }

    #[test]
    fn each_failure_names_its_check() {
        let mut f = Fake::healthy();
        f.reachable = false;
        assert_eq!(run(&f, Stage::Ready, &spec(None, &[])).unwrap_err().check, Check::Reachable);

        let mut f = Fake::healthy();
        f.disk = false;
        let e = run(&f, Stage::Ready, &spec(None, &[])).unwrap_err();
        assert_eq!(e.check, Check::Disk);
        assert!(e.to_string().contains("\"disk free\""), "{e}");

        let mut f = Fake::healthy();
        f.manifest = None;
        assert_eq!(run(&f, Stage::Ready, &spec(None, &[])).unwrap_err().check, Check::Relay);

        let mut f = Fake::healthy();
        f.manifest = Some("fork relay=1".into());
        assert_eq!(run(&f, Stage::Ready, &spec(None, &[])).unwrap_err().check, Check::Manifest);

        let f = Fake::healthy();
        assert_eq!(run(&f, Stage::Ready, &spec(Some("node relay=other"), &[])).unwrap_err().check, Check::Manifest);

        let f = Fake::healthy();
        *f.runs.lock().unwrap() = vec![(PROBE_NO_NODE_ENV, "TOD_NODE is ''")];
        assert_eq!(run(&f, Stage::Ready, &spec(None, &[])).unwrap_err().check, Check::Env);

        let f = Fake::healthy();
        *f.runs.lock().unwrap() = vec![(128, "could not read Username")];
        let e = run(&f, Stage::Ready, &spec(None, &[])).unwrap_err();
        assert_eq!(e.check, Check::GitRemote);
        assert!(e.detail.contains("could not read Username"), "{e}");
    }

    #[test]
    fn each_failure_says_what_is_wrong_and_the_real_next_step_before_the_detail() {
        const RESET: &str = "What to do: use Reset environment in the task panel's runner menu to install tod's files again and check once more. \
            If it fails the same way, use Delete sandbox (it saves the work first); the next Start makes a new one.";
        let said = |f: &Fake, setup: Setup| {
            let s = HealthSpec { setup, ..spec(None, &[]) };
            run(f, Stage::Ready, &s).unwrap_err().to_string()
        };
        let said_expected = |f: &Fake, setup: Setup| {
            let s = HealthSpec { setup, ..spec(Some("node relay=abc"), &[]) };
            run(f, Stage::Ready, &s).unwrap_err().to_string()
        };
        // A new sandbox's step is discard_failed_sandbox's: the text has no marker. An existing one's has exactly one, at the end.
        let split = |text: &str, setup: Setup| {
            let (plain, tech) = text.split_once("\nTechnical detail: ").unwrap_or_else(|| panic!("no detail line: {text}"));
            let n = plain.matches(WHAT_TO_DO).count();
            assert_eq!(n, usize::from(setup == Setup::Existing), "{plain}");
            (plain.to_string(), tech.to_string())
        };
        for setup in [Setup::New, Setup::Existing] {
            let end = |p: &str| {
                if setup == Setup::Existing {
                    assert!(p.ends_with(RESET), "{p}");
                } else {
                    assert!(p.ends_with('.') && !p.contains("Start"), "{p}");
                }
            };

            let mut f = Fake::healthy();
            f.reachable = false;
            let (p, t) = split(&said(&f, setup), setup);
            assert!(p.starts_with("The sandbox did not answer."), "{p}");
            end(&p);
            assert!(t.contains("sandbox health check \"reachable\" failed") && t.contains("did not wake"), "{t}");

            let mut f = Fake::healthy();
            f.manifest = None;
            let (p, t) = split(&said(&f, setup), setup);
            assert!(p.contains("tod's relay"), "{p}");
            end(&p);
            assert!(t.contains("\"relay up\""), "{t}");

            let mut f = Fake::healthy();
            f.manifest = Some("fork agents=false".into());
            let (p, t) = split(&said(&f, setup), setup);
            assert!(p.contains("set up as a plain sandbox, not a node sandbox"), "{p}");
            end(&p);
            assert!(t.contains("\"manifest\"") && t.contains("fork agents=false"), "{t}");

            let f = Fake::healthy();
            *f.runs.lock().unwrap() = vec![(PROBE_NO_NODE_ENV, "TOD_NODE is ''")];
            let (p, t) = split(&said(&f, setup), setup);
            assert!(p.contains("settings for this node did not take effect"), "{p}");
            end(&p);
            assert!(t.contains("TOD_NODE is ''"), "{t}");
        }

        let other = || Fake { manifest: Some("node relay=other".into()), ..Fake::healthy() };
        let (p, t) = split(&said_expected(&other(), Setup::New), Setup::New);
        assert!(p.contains("different build"), "{p}");
        assert!(p.ends_with("If new sandboxes keep coming out old, refresh the project's golden sandbox with `tod-sandbox golden refresh`."), "{p}");
        assert!(t.contains("node relay=other"), "{t}");
        let (p, _) = split(&said_expected(&other(), Setup::Existing), Setup::Existing);
        assert!(p.ends_with("What to do: use Reset environment in the task panel's runner menu to install this build's files again. If it fails the same way, run `tod-sandbox golden refresh` for the project's golden sandbox, then use Delete sandbox (it saves the work first) and Start for a new one."), "{p}");

        let mut f = Fake::healthy();
        f.disk = false;
        let (p, t) = split(&said(&f, Setup::New), Setup::New);
        assert!(p.starts_with("The sandbox is out of disk space"), "{p}");
        assert!(p.ends_with("If it fills up again, give the sandbox more disk or use a smaller image."), "{p}");
        assert!(t.contains("\"disk free\"") && t.contains("3 MB free"), "{t}");
        let (p, _) = split(&said(&f, Setup::Existing), Setup::Existing);
        assert!(p.ends_with("What to do: use Delete sandbox in the task panel's runner menu (it saves the work first), then Start makes a new one. If that fills up too, give the sandbox more disk or use a smaller image."), "{p}");

        let git = || {
            let f = Fake::healthy();
            *f.runs.lock().unwrap() = vec![(128, "could not read Username")];
            f
        };
        let (p, t) = split(&said(&git(), Setup::New), Setup::New);
        assert!(p.contains("cannot read the project's repository"), "{p}");
        assert!(p.ends_with("(the banner is absent when the token could not be checked, e.g. offline); otherwise check the repository URL."), "{p}");
        assert!(p.contains("red \"GitHub token\" banner's Add token or Replace token button"), "{p}");
        assert!(!p.contains("Settings") && !p.contains("is valid"), "{p}");
        assert!(t.contains("could not read Username"), "{t}");
        let (p, _) = split(&said(&git(), Setup::Existing), Setup::Existing);
        assert!(p.ends_with("then use Reset environment in the task panel's runner menu to set the sandbox up again."), "{p}");
    }

    #[test]
    fn the_base_stage_does_not_need_a_relay() {
        let mut f = Fake::healthy();
        f.manifest = None;
        run(&f, Stage::Base, &spec(None, &[])).unwrap();
    }

    #[test]
    fn the_proxy_checks_are_retried_and_the_last_failure_is_the_error() {
        let f = Fake::healthy();
        *f.runs.lock().unwrap() = vec![(41, "env"), (128, "auth"), (0, "")];
        run(&f, Stage::Ready, &spec(None, &[1, 1])).unwrap();
        assert_eq!(*f.ran.lock().unwrap(), 3);

        let f = Fake::healthy();
        *f.runs.lock().unwrap() = vec![(41, "env"), (128, "auth"), (128, "auth")];
        let e = run(&f, Stage::Ready, &spec(None, &[1])).unwrap_err();
        assert_eq!(e.check, Check::GitRemote);
        assert_eq!(*f.ran.lock().unwrap(), 2);
    }

    #[test]
    fn the_live_probes_name_the_check_through_a_stub_process_api() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        // A process API that answers every command with the given exit code and output.
        fn serve(exit: i32, logs: &'static str) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        continue;
                    }
                    let mut len = 0usize;
                    loop {
                        let mut h = String::new();
                        reader.read_line(&mut h).unwrap();
                        if h.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0u8; len];
                    reader.read_exact(&mut body).unwrap();
                    let body = String::from_utf8_lossy(&body);
                    // `df` is the disk check; anything else is the env/git probe.
                    let reply = if body.contains("df -Pk") {
                        r#"{"exitCode":0,"logs":"Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100 1 99999999 1% /"}"#.to_string()
                    } else {
                        format!(r#"{{"exitCode":{exit},"logs":{}}}"#, serde_json::json!(logs))
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                }
            });
            url
        }
        let bx = Blaxel::new("ws", "tok");
        let s = HealthSpec { reachable_wait: Duration::from_secs(5), ..spec(None, &[]) };
        let err = probe(&bx, &serve(41, "TOD_NODE is '' in the sandbox environment"), Stage::Base, &s, &mut |_| {}).unwrap_err();
        assert_eq!(err.check, Check::Env, "{err}");
        let err = probe(&bx, &serve(128, "fatal: could not read Username"), Stage::Base, &s, &mut |_| {}).unwrap_err();
        assert_eq!(err.check, Check::GitRemote, "{err}");
        probe(&bx, &serve(0, "abc\tHEAD"), Stage::Base, &s, &mut |_| {}).unwrap();
    }
}
