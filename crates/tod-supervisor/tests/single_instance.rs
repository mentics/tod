//! Two supervisors never run at once (`tod_supervisor::lock`): the new
//! dead-man retry wake may fire while a long run goes on, and must then be
//! harmless: the second one exits at once saying so, touches nothing (not
//! the replica, not the hold, not the schedule), and pokes the first.

use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_core::autopilot::Budget;
use tod_core::scheduler::Scheduler;
use tod_supervisor::agent::AgentKind;
use tod_supervisor::hold::Holder;
use tod_supervisor::orchestrator::Orchestrator;
use tod_supervisor::{Config, Woke, lock, wake};
use uuid::Uuid;

#[derive(Default)]
struct Calls(Mutex<Vec<String>>);

impl Holder for Calls {
    fn hold(&self, reason: &str, secs: u64) -> Result<()> {
        self.0.lock().unwrap().push(format!("hold {reason} {secs}"));
        Ok(())
    }
    fn release(&self, reason: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("release {reason}"));
        Ok(())
    }
}

impl Scheduler for Calls {
    fn schedule(&self, id: Uuid, _sandbox: &str, at_ms: i64) -> Result<()> {
        self.0.lock().unwrap().push(format!("schedule {id} {at_ms}"));
        Ok(())
    }
    fn cancel(&self, id: Uuid) -> Result<()> {
        self.0.lock().unwrap().push(format!("cancel {id}"));
        Ok(())
    }
}

#[test]
fn a_second_supervisor_exits_at_once_and_leaves_everything_alone() {
    let dir = std::env::temp_dir().join(format!("tod-supervisor-single-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let node = Uuid::new_v4();
    // The running supervisor's lock (this process stands in for it).
    let held = match lock::acquire(&dir).unwrap() {
        lock::Taken::Held(l) => l,
        lock::Taken::Busy { .. } => panic!("nobody holds it yet"),
    };
    let calls = Arc::new(Calls::default());
    let woke = wake(Config {
        // Never contacted: the wake must return before it opens anything.
        orchestrator: Orchestrator::new("http://127.0.0.1:1", "alice", node.to_string()),
        node,
        workspace: dir.join("workspace"),
        state_dir: dir.clone(),
        agent: AgentKind::Mock,
        holder: calls.clone(),
        transcripts: None,
        media: tod_core::media::MediaPaths::from_media_root(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("tod").join("media"),
        )
        .unwrap(),
        budget: Budget { max_sessions: 1, max_duration: Duration::from_secs(60) },
        guards: Default::default(),
        poll: Duration::from_millis(20),
        push_branch: false,
        scheduler: Some(calls.clone()),
        sandbox: "node-test".into(),
    })
    .unwrap();
    assert_eq!(woke, Woke::AlreadyRunning { holder: Some(std::process::id()) });
    assert!(calls.0.lock().unwrap().is_empty(), "a second supervisor touched the hold or the schedule: {:?}", calls.0.lock().unwrap());
    assert!(!dir.join("tod.db").exists(), "it opened the replica");
    // It poked the holder to look again.
    assert!(tod_supervisor::signal::take_poke());
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}
