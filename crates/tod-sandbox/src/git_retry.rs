//! Which `git fetch` failures are worth trying again, for the Rust callers
//! (`tod_store::fleet::workdir::retry_ref_lock_race`) and, as shell, for the
//! scripts that run git where Rust cannot wrap it (the quick branch script,
//! the sandbox checkout script, `scripts/refresh-workspace-golden.sh`, which
//! carries a copy of [`FETCH_RETRY_SH`] kept equal by a test).

/// Stderr fragments of a fetch that lost a race with another git on the same
/// repository (a ref or file lock, or a ref that moved under it).
pub const LOCK_RACE_MARKERS: [&str; 2] = ["cannot lock ref", "Unable to create"];

/// Stderr fragments of a transient network failure: TLS receive errors, DNS,
/// timeouts, resets, a pack cut short, a failed RPC (curl 56/28), HTTP 5xx.
pub const NETWORK_MARKERS: [&str; 18] = [
    "GnuTLS recv error",
    "GnuTLS send error",
    "SSL_read",
    "SSL_connect",
    "Could not resolve host",
    "Connection timed out",
    "Connection reset",
    "Operation timed out",
    "early EOF",
    "RPC failed",
    "curl 56",
    "curl 28",
    "unexpected disconnect",
    "The remote end hung up unexpectedly",
    "error: 500",
    "error: 502",
    "error: 503",
    "error: 504",
];

pub fn is_ref_lock_race(stderr: &str) -> bool {
    stderr.contains("cannot lock ref")
        || (stderr.contains("is at ") && stderr.contains("but expected"))
        || (stderr.contains("Unable to create") && stderr.contains(".lock"))
}

pub fn is_transient_network_error(stderr: &str) -> bool {
    NETWORK_MARKERS.iter().any(|m| stderr.contains(m))
}

/// A shell function, `tod_fetch_retry git ... fetch ...`: runs the command,
/// keeps its combined output in `$out`, and runs it again after 1, 2 and 4
/// seconds while that output is a lock race or a transient network failure
/// (the same classes as [`is_ref_lock_race`] and [`is_transient_network_error`]).
/// It returns the last attempt's status; the caller reports `$out`. Any other
/// failure returns at once. POSIX sh.
pub const FETCH_RETRY_SH: &str = r#"tod_fetch_retry() {
  for _w in 1 2 4 x; do
    out=$("$@" 2>&1) && return 0
    _rc=$?
    case "$out" in
      *"cannot lock ref"*|*"is at "*"but expected"*|*"Unable to create"*".lock"*|\
      *"GnuTLS recv error"*|*"GnuTLS send error"*|*"SSL_read"*|*"SSL_connect"*|*"Could not resolve host"*|\
      *"Connection timed out"*|*"Connection reset"*|*"Operation timed out"*|*"early EOF"*|*"RPC failed"*|\
      *"curl 56"*|*"curl 28"*|*"unexpected disconnect"*|*"The remote end hung up unexpectedly"*|\
      *"error: 500"*|*"error: 502"*|*"error: 503"*|*"error: 504"*) ;;
      *) return "$_rc" ;;
    esac
    [ "$_w" = x ] && return "$_rc"
    sleep "$_w"
  done
}
"#;

// ---- counting the retries (shepherd gap 49) ----
//
// This leaf crate holds the counter's core so every Rust retry loop, here and
// in `tod-store`, reports through one place; `tod_store::git_retries` is the
// sink the store installs and the table it writes.

/// Retry classes besides the two the markers above name.
pub const CLASS_LOCK_RACE: &str = "lock_race";
pub const CLASS_NETWORK: &str = "network";
/// A checkout refused for want of credentials the proxy had not yet supplied.
pub const CLASS_AUTH: &str = "auth";
/// `git worktree add` refused for a stale registration, pruned and tried again.
pub const CLASS_STALE_WORKTREE: &str = "stale_worktree";
/// A fetch that failed for no transient reason and was tried once more (a
/// partial clone's "not our ref" after a branch moved).
pub const CLASS_OTHER: &str = "other";

/// One call that was retried, as a retry loop reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retried {
    /// The node (id string) the thread was working for, when it was.
    pub node: Option<String>,
    pub workdir: String,
    pub op: String,
    pub class: String,
    pub retries: u32,
    pub recovered: bool,
    pub error: String,
}

/// Where a retried call is counted; the error is why it could not be.
pub type Sink = std::sync::Arc<dyn Fn(Retried) -> anyhow::Result<()> + Send + Sync>;

static SINK: std::sync::RwLock<Option<Sink>> = std::sync::RwLock::new(None);
static FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Installs the sink (the store that owns this process's database does it
/// when it opens). The latest wins.
pub fn set_sink(sink: Sink) {
    *SINK.write().unwrap_or_else(|p| p.into_inner()) = Some(sink);
}

/// How many retries this process failed to record (each was also logged).
pub fn record_failures() -> u64 {
    FAILURES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Counts one failure to record, and logs it. Recording never fails the call
/// it describes, so this is where such a failure is kept.
pub fn count_failure(what: &str, why: &str) {
    FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::error!("could not record the retry of {what}: {why}");
}

thread_local! {
    static NODE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// While alive, retries this thread makes are counted against a node.
pub struct NodeScope(Option<String>);

impl Drop for NodeScope {
    fn drop(&mut self) {
        NODE.with(|n| *n.borrow_mut() = self.0.take());
    }
}

/// Counts this thread's retries against `node` (an id string) until dropped.
pub fn scope_node(node: Option<String>) -> NodeScope {
    NodeScope(NODE.with(|n| n.replace(node)))
}

/// The node this thread's retries count against. A thread that works for its
/// caller (a spawned scope thread) captures this before it spawns and
/// re-enters it with [`scope_node`], since a thread-local is not inherited.
pub fn current_node() -> Option<String> {
    NODE.with(|n| n.borrow().clone())
}

/// Reports a retried call to the sink: no sink, nothing to do; a sink that
/// cannot record is counted and logged, never allowed to change the call.
pub fn report(workdir: &str, op: &str, class: &str, retries: u32, recovered: bool, error: &str) {
    let Some(sink) = SINK.read().unwrap_or_else(|p| p.into_inner()).clone() else {
        return;
    };
    let record = Retried {
        node: current_node(),
        workdir: workdir.to_string(),
        op: op.to_string(),
        class: class.to_string(),
        retries,
        recovered,
        error: error.to_string(),
    };
    if let Err(e) = sink(record) {
        count_failure(&format!("{op} in {workdir}"), &format!("{e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_function_names_every_marker() {
        for m in NETWORK_MARKERS.iter().chain(LOCK_RACE_MARKERS.iter()) {
            assert!(FETCH_RETRY_SH.contains(&format!("*\"{m}\"*")), "missing {m}");
        }
        assert!(FETCH_RETRY_SH.contains("*\"is at \"*\"but expected\"*"));
    }

    #[test]
    fn shell_function_retries_then_returns_the_last_output() {
        // The command runs in a subshell, so its attempts are counted in a file.
        let script = format!(
            "{FETCH_RETRY_SH}\nsleep() {{ :; }}\nc=$(mktemp)\ncount() {{ echo x >>\"$c\"; wc -l <\"$c\" | tr -d ' '; }}\n\
             flaky() {{ n=$(count); echo \"try $n: GnuTLS recv error (-110)\"; [ \"$n\" -ge 3 ]; }}\n\
             tod_fetch_retry flaky && echo \"ok after $(count)\"\n\
             : >\"$c\"; fail() {{ count >/dev/null; echo 'fatal: Authentication failed'; return 1; }}\n\
             tod_fetch_retry fail || echo \"gave up after $(count): $out\"\n\
             : >\"$c\"; down() {{ count >/dev/null; echo 'cannot lock ref'; return 1; }}\n\
             tod_fetch_retry down || echo \"exhausted after $(count): $out\""
        );
        let o = std::process::Command::new("sh").args(["-c", &script]).output().unwrap();
        let s = String::from_utf8_lossy(&o.stdout);
        assert!(s.contains("ok after 4"), "{s}");
        assert!(s.contains("gave up after 2: fatal: Authentication failed"), "{s}");
        assert!(s.contains("exhausted after 5: cannot lock ref"), "{s}");
    }

    #[test]
    fn golden_script_carries_the_same_function() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/refresh-workspace-golden.sh");
        let script = std::fs::read_to_string(path).unwrap();
        assert!(script.contains(FETCH_RETRY_SH));
        assert!(script.contains("tod_fetch_retry git -C \"$CLONE\" fetch"));
        assert!(!script.contains("\ngit -C \"$CLONE\" fetch"));
    }

    #[test]
    fn checkout_script_fetches_through_the_retry() {
        let me = crate::node::GitIdentity { name: "n".into(), email: "e".into() };
        let s = crate::node::checkout_script("u", "b", &me, true);
        assert!(s.contains(FETCH_RETRY_SH));
        assert!(s.contains("tod_fetch_retry git -C \"$dir\" fetch"));
        assert!(!s.contains("; git -C \"$dir\" fetch"));
    }
}
