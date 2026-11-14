//! Why did the agent process die? When an agent in a cloud sandbox or dev
//! container exits without being asked to, the bridge reports only that it
//! "exited unexpectedly". The usual cause there is the kernel's OOM killer
//! (a sandbox's root filesystem lives in RAM, so a full `/tmp` is a memory
//! problem). [`probe`] asks the machine, once and with a time bound, and
//! [`Diagnosis::explain`] turns the answer into a sentence appended to the
//! original error (never replacing it).
//!
//! Everything here is plain text in, text out, so it is tested on any OS; only
//! the probe's script runs on Linux.

use crate::devcontainer::AgentEnvironment;
use std::time::Duration;

/// How long the probe may take in all (the `du` inside it is bounded too).
pub const PROBE_LIMIT: Duration = Duration::from_secs(25);

/// The script the probe runs where the agent was, as `sh -c`. It prints
/// `== <section>` headers followed by raw command output, which
/// [`Diagnosis::parse`] reads. Every command may fail on its own (no `dmesg`
/// permission, no cgroup v2); a section is then just empty.
pub const PROBE_SCRIPT: &str = r#"
echo "== uptime"; cut -d' ' -f1 /proc/uptime 2>/dev/null
echo "== dmesg"; dmesg 2>/dev/null | grep -i 'out of memory' | tail -n 2
echo "== events"; cat /sys/fs/cgroup/memory.events 2>/dev/null
echo "== meminfo"; grep -E '^(MemTotal|MemAvailable):' /proc/meminfo 2>/dev/null
echo "== df"; df -Pk / 2>/dev/null | tail -n 1
echo "== tmp"; timeout 10 du -skx /tmp/* /tmp/.[!.]* 2>/dev/null | sort -rn | head -n 5
"#;

/// What [`PROBE_SCRIPT`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Diagnosis {
    /// The kernel's last "Out of memory: Killed process …" line, if `dmesg`
    /// could be read and held one.
    pub oom_line: Option<String>,
    /// Seconds since boot at which that line was logged (`dmesg`'s own
    /// timestamp), when it has one.
    pub oom_at: Option<f64>,
    /// Seconds since boot now.
    pub uptime: Option<f64>,
    /// The cgroup's `oom_kill` count, if it has one.
    pub oom_kills: Option<u64>,
    pub mem_total_kb: Option<u64>,
    pub mem_available_kb: Option<u64>,
    /// How full `/` is, in percent.
    pub root_used_pct: Option<u8>,
    /// Largest top-level entries of `/tmp`, in KiB, biggest first.
    pub tmp_largest: Vec<(String, u64)>,
}

/// Below this much available memory the machine is, for our purposes, out.
const LOW_MEMORY_KB: u64 = 400 * 1024;
/// At or above this a RAM-backed root filesystem is a memory problem.
const FULL_ROOT_PCT: u8 = 90;

impl Diagnosis {
    /// Read the output of [`PROBE_SCRIPT`]. Lines it does not understand are
    /// ignored: the diagnosis is an addition to the real error, so a probe
    /// that printed something odd must not hide that error.
    pub fn parse(output: &str) -> Self {
        let mut d = Self::default();
        let mut section = "";
        for line in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(name) = line.strip_prefix("== ") {
                section = match name {
                    "uptime" => "uptime",
                    "dmesg" => "dmesg",
                    "events" => "events",
                    "meminfo" => "meminfo",
                    "df" => "df",
                    "tmp" => "tmp",
                    _ => "",
                };
                continue;
            }
            match section {
                "uptime" => d.uptime = line.parse().ok(),
                "dmesg" => {
                    // `dmesg` lines may carry a `[ 123.4]` timestamp: seconds since boot.
                    d.oom_at = line
                        .strip_prefix('[')
                        .and_then(|rest| rest.split_once(']'))
                        .and_then(|(t, _)| t.trim().parse().ok());
                    let text = line.rsplit_once("] ").map_or(line, |(_, rest)| rest);
                    d.oom_line = Some(text.to_string());
                }
                "events" => {
                    if let Some(n) = line.strip_prefix("oom_kill ").and_then(|n| n.trim().parse().ok()) {
                        d.oom_kills = Some(n);
                    }
                }
                "meminfo" => {
                    let kb = |rest: &str| rest.split_whitespace().next().and_then(|n| n.parse().ok());
                    if let Some(rest) = line.strip_prefix("MemTotal:") {
                        d.mem_total_kb = kb(rest);
                    } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                        d.mem_available_kb = kb(rest);
                    }
                }
                "df" => {
                    // POSIX `df -P`: filesystem, blocks, used, available, capacity, mount.
                    d.root_used_pct = line
                        .split_whitespace()
                        .find_map(|f| f.strip_suffix('%'))
                        .and_then(|n| n.parse().ok());
                }
                "tmp" => {
                    if let Some((kb, path)) = line.split_once(char::is_whitespace)
                        && let Ok(kb) = kb.parse::<u64>()
                    {
                        d.tmp_largest.push((path.trim().to_string(), kb));
                    }
                }
                _ => {}
            }
        }
        d.tmp_largest.sort_by(|a, b| b.1.cmp(&a.1));
        d
    }

    /// Whether the machine is short of memory now: little available, or a
    /// RAM-backed root filesystem nearly full.
    pub fn memory_pressure(&self) -> bool {
        self.mem_available_kb.is_some_and(|kb| kb < LOW_MEMORY_KB)
            || self.root_used_pct.is_some_and(|p| p >= FULL_ROOT_PCT)
    }

    /// Whether the kernel log shows an OOM kill after the agent started
    /// (`age` ago). `dmesg` is boot-wide; a line with no timestamp, or no
    /// uptime to compare it with, is not taken as recent.
    pub fn kill_since_start(&self, age: Option<Duration>) -> bool {
        match (self.oom_line.is_some(), self.oom_at, self.uptime, age) {
            (true, Some(at), Some(up), Some(age)) => at >= up - age.as_secs_f64() - KILL_SLACK_SECS,
            _ => false,
        }
    }

    /// One sentence to append to the error (without a trailing period), or
    /// `None` when the probe found nothing at all. It claims a kill only with
    /// evidence of one since the agent started ([`Self::kill_since_start`]);
    /// otherwise it says what the machine looks like.
    pub fn explain(&self, age: Option<Duration>) -> Option<String> {
        let killed = self.kill_since_start(age);
        let pressure = self.memory_pressure();
        let mut facts = Vec::new();
        if let Some(pct) = self.root_used_pct {
            let ram = if pressure { ", which uses RAM" } else { "" };
            facts.push(format!("root filesystem {pct}% full{ram}"));
        }
        if let Some(kb) = self.mem_available_kb {
            let total = self.mem_total_kb.map(|t| format!(" of {}", size(t))).unwrap_or_default();
            facts.push(format!("{} memory available{total}", size(kb)));
        }
        if !self.tmp_largest.is_empty() {
            let list: Vec<String> =
                self.tmp_largest.iter().map(|(path, kb)| format!("{path} {}", size(*kb))).collect();
            facts.push(format!("largest in /tmp: {}", list.join(", ")));
        }
        if let Some(n) = self.oom_kills.filter(|n| *n > 0) {
            facts.push(format!("the memory cgroup has recorded {n} OOM kill(s) so far"));
        }
        if let Some(line) = &self.oom_line {
            let when = if killed { "" } else { " (logged before the agent started, or at an unknown time)" };
            facts.push(format!("kernel: {line}{when}"));
        }
        if facts.is_empty() {
            return None;
        }
        let head = if killed {
            "the agent was killed: the machine ran out of memory"
        } else if pressure {
            "the machine is nearly out of memory, which likely killed the agent"
        } else {
            "no out-of-memory kill was found since the agent started"
        };
        Some(format!("{head} ({})", facts.join("; ")))
    }
}

/// Clock slack when comparing a kernel timestamp with the agent's start.
const KILL_SLACK_SECS: f64 = 30.0;

/// `1234567` KiB as "1.2 GB", `765432` as "747 MB".
fn size(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

/// Look, in `environment`, for why an agent there died (it had run for `age`).
/// `None` on a non-Linux host and when the probe found nothing; a probe that
/// could not run says so rather than staying silent.
pub fn probe(environment: &AgentEnvironment, age: Option<Duration>) -> Option<String> {
    let result = match environment {
        // The supervisor in a node's sandbox (and a daemon on a Linux
        // machine) spawns the agent on the machine it runs on. Elsewhere
        // there is no `/proc` to ask.
        AgentEnvironment::Host if cfg!(target_os = "linux") => {
            crate::sandbox::run_local_script(PROBE_SCRIPT, PROBE_LIMIT)
        }
        AgentEnvironment::Host => return None,
        AgentEnvironment::Sandbox(launch) => launch.run_script(PROBE_SCRIPT, PROBE_LIMIT),
        AgentEnvironment::DevContainer(launch) => crate::devcontainer::ContainerExec::connect(&launch.container)
            .and_then(|exec| exec.output("/", "sh", &["-c", PROBE_SCRIPT]))
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned()),
    };
    match result {
        Ok(output) => Diagnosis::parse(&output).explain(age),
        Err(err) => Some(format!("could not check why the agent stopped: {err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OOM: &str = "== uptime\n5000.5\n== dmesg\n[ 4521.9] Out of memory: Killed process 812 (MainThread) total-vm:1 kB\n\
== events\nlow 0\nhigh 0\nmax 12\noom 3\noom_kill 3\n\
== meminfo\nMemTotal:        8192000 kB\nMemAvailable:     188416 kB\n\
== df\noverlay 8000000 7680000 320000 96% /\n\
== tmp\n783360\t/tmp/tfv\n783000\t/tmp/tfcheck\n12\t/tmp/x\n";

    const RECENT: Option<Duration> = Some(Duration::from_secs(900));

    #[test]
    fn parses_an_oom_kill_and_a_full_root() {
        let d = Diagnosis::parse(OOM);
        assert_eq!(d.oom_line.as_deref(), Some("Out of memory: Killed process 812 (MainThread) total-vm:1 kB"));
        assert_eq!(d.oom_at, Some(4521.9));
        assert_eq!(d.uptime, Some(5000.5));
        assert_eq!(d.oom_kills, Some(3));
        assert_eq!(d.mem_available_kb, Some(188416));
        assert_eq!(d.mem_total_kb, Some(8192000));
        assert_eq!(d.root_used_pct, Some(96));
        assert_eq!(d.tmp_largest[0], ("/tmp/tfv".to_string(), 783360));
        assert!(d.memory_pressure());
    }

    #[test]
    fn a_kill_after_the_agent_started_is_claimed_with_the_largest_directories() {
        // Agent started 900 s ago (at 4100); the kill was at 4521.9.
        let text = Diagnosis::parse(OOM).explain(RECENT).unwrap();
        assert!(text.starts_with("the agent was killed: the machine ran out of memory ("), "{text}");
        assert!(text.contains("root filesystem 96% full, which uses RAM"), "{text}");
        assert!(text.contains("184 MB memory available of 7.8 GB"), "{text}");
        assert!(text.contains("/tmp/tfv 765 MB"), "{text}");
        assert!(text.contains("kernel: Out of memory: Killed process 812"), "{text}");
    }

    #[test]
    fn a_kill_before_the_agent_started_is_not_claimed() {
        // Agent started 100 s ago (at 4900): the kill at 4521.9 was earlier.
        let text = Diagnosis::parse(OOM).explain(Some(Duration::from_secs(100))).unwrap();
        assert!(text.starts_with("the machine is nearly out of memory, which likely killed the agent"), "{text}");
        assert!(text.contains("logged before the agent started"), "{text}");
        assert!(!text.contains("the agent was killed"), "{text}");
        // Without the agent's age nothing is claimed either.
        assert!(!Diagnosis::parse(OOM).explain(None).unwrap().contains("the agent was killed"));
    }

    #[test]
    fn pressure_alone_is_not_a_kill() {
        let d = Diagnosis::parse("== df\noverlay 100 97 3 97% /\n");
        assert!(d.memory_pressure());
        let text = d.explain(RECENT).unwrap();
        assert!(text.starts_with("the machine is nearly out of memory, which likely killed the agent"), "{text}");
        assert!(text.contains("root filesystem 97% full, which uses RAM"), "{text}");
    }

    #[test]
    fn a_healthy_machine_is_reported_as_such_not_as_oom() {
        let d = Diagnosis::parse(
            "== dmesg\n== events\noom_kill 0\n== meminfo\nMemTotal: 8192000 kB\nMemAvailable: 6000000 kB\n\
             == df\noverlay 8000000 1000000 7000000 13% /\n== tmp\n100\t/tmp/a\n",
        );
        assert!(!d.memory_pressure());
        let text = d.explain(RECENT).unwrap();
        assert!(text.starts_with("no out-of-memory kill was found"), "{text}");
        assert!(!text.contains("which uses RAM"), "{text}");
    }

    #[test]
    fn unreadable_output_yields_no_claim() {
        assert_eq!(Diagnosis::parse("").explain(RECENT), None);
        assert_eq!(Diagnosis::parse("garbage\n== df\nnonsense\n").explain(RECENT), None);
    }

    #[test]
    fn sizes_read_as_mb_or_gb() {
        assert_eq!(size(765432), "747 MB");
        assert_eq!(size(3_984_588), "3.8 GB");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_host_is_probed_on_linux() {
        // The supervisor's agent runs on the host of the sandbox.
        assert!(probe(&AgentEnvironment::Host, RECENT).is_some());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_host_is_not_probed_off_linux() {
        assert_eq!(probe(&AgentEnvironment::Host, RECENT), None);
    }
}
