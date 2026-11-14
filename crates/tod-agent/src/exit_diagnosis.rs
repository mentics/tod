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
echo "== dmesg"; dmesg 2>/dev/null | grep -i 'out of memory' | tail -n 2
echo "== events"; cat /sys/fs/cgroup/memory.events 2>/dev/null
echo "== meminfo"; grep -E '^(MemTotal|MemAvailable):' /proc/meminfo 2>/dev/null
echo "== df"; df -Pk / 2>/dev/null | tail -n 1
echo "== tmp"; timeout 10 du -skx /tmp/* /tmp/.[!.]* 2>/dev/null | sort -rn | head -n 5
"#;

/// What [`PROBE_SCRIPT`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnosis {
    /// The kernel's last "Out of memory: Killed process …" line, if `dmesg`
    /// could be read and held one.
    pub oom_line: Option<String>,
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
                "dmesg" => {
                    // `dmesg` lines may carry a `[ 123.4]` timestamp.
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

    /// Whether what was found points at running out of memory.
    pub fn out_of_memory(&self) -> bool {
        self.oom_line.is_some()
            || self.oom_kills.is_some_and(|n| n > 0)
            || self.mem_available_kb.is_some_and(|kb| kb < LOW_MEMORY_KB)
            || self.root_used_pct.is_some_and(|p| p >= FULL_ROOT_PCT)
    }

    /// One sentence to append to the error (without a trailing period), or
    /// `None` when the probe found nothing at all.
    pub fn explain(&self) -> Option<String> {
        let mut facts = Vec::new();
        if let Some(pct) = self.root_used_pct {
            let ram = if self.out_of_memory() { ", which uses RAM" } else { "" };
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
            facts.push(format!("the memory cgroup has killed {n} process(es)"));
        }
        if let Some(line) = &self.oom_line {
            facts.push(format!("kernel: {line}"));
        }
        if facts.is_empty() && !self.out_of_memory() {
            return None;
        }
        let head = if self.out_of_memory() {
            "the agent was killed: the machine ran out of memory"
        } else {
            "no out-of-memory kill was found"
        };
        Some(if facts.is_empty() { head.to_string() } else { format!("{head} ({})", facts.join("; ")) })
    }
}

/// `1234567` KiB as "1.2 GB", `765432` as "747 MB".
fn size(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

/// Look, in `environment`, for why an agent there died. `None` on the host
/// (nothing cheap to say) and when the probe found nothing; a probe that
/// could not run says so rather than staying silent.
pub fn probe(environment: &AgentEnvironment) -> Option<String> {
    let result = match environment {
        AgentEnvironment::Host => return None,
        AgentEnvironment::Sandbox(launch) => launch.run_script(PROBE_SCRIPT, PROBE_LIMIT),
        AgentEnvironment::DevContainer(launch) => crate::devcontainer::ContainerExec::connect(&launch.container)
            .and_then(|exec| exec.output("/", "sh", &["-c", PROBE_SCRIPT]))
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned()),
    };
    match result {
        Ok(output) => Diagnosis::parse(&output).explain(),
        Err(err) => Some(format!("could not check why the agent stopped: {err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OOM: &str = "== dmesg\n[ 4521.9] Out of memory: Killed process 812 (MainThread) total-vm:1 kB\n\
== events\nlow 0\nhigh 0\nmax 12\noom 3\noom_kill 3\n\
== meminfo\nMemTotal:        8192000 kB\nMemAvailable:     188416 kB\n\
== df\noverlay 8000000 7680000 320000 96% /\n\
== tmp\n783360\t/tmp/tfv\n783000\t/tmp/tfcheck\n12\t/tmp/x\n";

    #[test]
    fn parses_an_oom_kill_and_a_full_root() {
        let d = Diagnosis::parse(OOM);
        assert_eq!(d.oom_line.as_deref(), Some("Out of memory: Killed process 812 (MainThread) total-vm:1 kB"));
        assert_eq!(d.oom_kills, Some(3));
        assert_eq!(d.mem_available_kb, Some(188416));
        assert_eq!(d.mem_total_kb, Some(8192000));
        assert_eq!(d.root_used_pct, Some(96));
        assert_eq!(d.tmp_largest[0], ("/tmp/tfv".to_string(), 783360));
        assert!(d.out_of_memory());
    }

    #[test]
    fn explains_in_one_sentence_with_the_largest_directories() {
        let text = Diagnosis::parse(OOM).explain().unwrap();
        assert!(text.starts_with("the agent was killed: the machine ran out of memory ("), "{text}");
        assert!(text.contains("root filesystem 96% full, which uses RAM"), "{text}");
        assert!(text.contains("184 MB memory available of 7.8 GB"), "{text}");
        assert!(text.contains("/tmp/tfv 765 MB"), "{text}");
        assert!(text.contains("kernel: Out of memory: Killed process 812"), "{text}");
    }

    #[test]
    fn a_healthy_machine_is_reported_as_such_not_as_oom() {
        let d = Diagnosis::parse(
            "== dmesg\n== events\noom_kill 0\n== meminfo\nMemTotal: 8192000 kB\nMemAvailable: 6000000 kB\n\
             == df\noverlay 8000000 1000000 7000000 13% /\n== tmp\n100\t/tmp/a\n",
        );
        assert!(!d.out_of_memory());
        let text = d.explain().unwrap();
        assert!(text.starts_with("no out-of-memory kill was found"), "{text}");
        assert!(!text.contains("which uses RAM"), "{text}");
    }

    #[test]
    fn unreadable_output_yields_no_claim() {
        assert_eq!(Diagnosis::parse("").explain(), None);
        assert_eq!(Diagnosis::parse("garbage\n== df\nnonsense\n").explain(), None);
    }

    #[test]
    fn a_full_root_alone_counts_even_without_dmesg() {
        let d = Diagnosis::parse("== df\noverlay 100 97 3 97% /\n");
        assert!(d.out_of_memory());
        assert!(d.explain().unwrap().contains("root filesystem 97% full, which uses RAM"));
    }

    #[test]
    fn sizes_read_as_mb_or_gb() {
        assert_eq!(size(765432), "747 MB");
        assert_eq!(size(3_984_588), "3.8 GB");
    }

    #[test]
    fn the_host_is_not_probed() {
        assert_eq!(probe(&AgentEnvironment::Host), None);
    }
}
