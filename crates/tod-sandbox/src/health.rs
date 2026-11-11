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

/// The failed check and what it saw.
#[derive(Debug)]
pub struct HealthFailure {
    pub check: Check,
    pub detail: String,
}

impl std::fmt::Display for HealthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sandbox health check \"{}\" failed: {}", self.check.name(), self.detail)
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
    let fail = |check: Check, detail: String| HealthFailure { check, detail };
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
                return Err(fail(
                    Check::Manifest,
                    format!(
                        "{} reports manifest {reported:?}, expected {}",
                        spec.label,
                        spec.expected_manifest.map_or("a node's".to_string(), |m| format!("{m:?}"))
                    ),
                ));
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
