//! A wake end to end: an in-process orchestrator seeded with a user's
//! database, a real local git repository with a bare `origin`, a stand-in
//! relay that records hold requests, and the mock agent. The node's work
//! must show up on the orchestrator, the hold must be taken and released,
//! and a Claude session file must be mirrored.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_core::autopilot::Budget;
use tod_store::fleet::FleetStore;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use tod_supervisor::agent::AgentKind;
use tod_supervisor::hold::RelayHolder;
use tod_supervisor::orchestrator::Orchestrator;
use tod_supervisor::transcripts::TranscriptStore;
use tod_supervisor::{Config, Woke, wake};
use uuid::Uuid;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A relay stand-in: answers 200 to anything and records the request lines.
fn fake_relay() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream: TcpStream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            if let Some(line) = head.lines().next() {
                log.lock().unwrap().push(line.to_string());
            }
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (url, seen)
}

/// The user's app database: a node in `active` with Files and a two-step plan.
fn app_database(root: &Path) -> Uuid {
    let fleet = FleetStore::open(root).unwrap();
    fleet
        .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
        .unwrap();
    let list_id = fleet.list_outline_lists().unwrap()[0].id;
    let node = Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Cloud node".into(),
        })
        .unwrap();
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node,
            capabilities: vec![Capability::Spec, Capability::Files],
        })
        .unwrap();
    fleet.writer().flush().unwrap();
    // Where the repository is on the user's machine; the supervisor points
    // its copy at its own checkout.
    fleet
        .enqueue(tod_store::fleet::FleetMutation::UpdateTaskRepo {
            id: node.to_string(),
            repo: Some("C:/Users/someone/src/repo".into()),
        })
        .unwrap();
    for n in 0..2 {
        fleet
            .enqueue_outline(OutlineMutation::CreatePlanStep {
                step_id: None,
                node_id: node,
                after_id: None,
                before: false,
                body: format!("Step {n}"),
            })
            .unwrap();
    }
    fleet.writer().flush().unwrap();
    tod_core::lifecycle::set_lifecycle(&fleet, node, "active").unwrap();
    fleet.writer().flush().unwrap();
    node
}

fn media() -> tod_core::media::MediaPaths {
    tod_core::media::MediaPaths::from_media_root(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("tod").join("media"),
    )
    .unwrap()
}

#[test]
fn a_wake_runs_the_node_and_its_work_reaches_the_orchestrator() {
    let tmp = Temp(std::env::temp_dir().join(format!("tod-supervisor-wake-{}", Uuid::new_v4())));
    let base = tmp.0.clone();
    std::fs::create_dir_all(&base).unwrap();

    // The orchestrator, seeded with the app's database.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let orch_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tod_orchestrator::Server::new(tod_orchestrator::Config {
        base: base.join("orchestrator"),
        tod_cli: "tod-cli-not-used".into(),
        tod_cli_prefix: Vec::new(),
    });
    std::thread::spawn(move || server.serve(listener));
    let app = base.join("app");
    let node = app_database(&app);
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = ureq::post(format!("{orch_url}/users/alice/seed")).send(std::fs::read(&snap).unwrap()).unwrap();
    assert_eq!(seeded.status(), 200);
    let orchestrator = Orchestrator::new(orch_url.clone(), "alice", node.to_string());
    let seed_seq = orchestrator.changes_after(0).unwrap().last_seq;

    // The node's checkout, with an origin to push to.
    let origin = base.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "--quiet"]);
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    git(&workspace, &["init", "--quiet"]);
    git(&workspace, &["checkout", "--quiet", "-b", "node-branch"]);
    std::fs::write(workspace.join("README"), "hi\n").unwrap();
    git(&workspace, &["add", "README"]);
    git(&workspace, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "init"]);
    git(&workspace, &["remote", "add", "origin", &origin.to_string_lossy()]);

    // A Claude session file, as Claude would have written it.
    let projects = base.join("claude-projects");
    let session = projects.join("-workspace-repo").join("0000-session.jsonl");
    std::fs::create_dir_all(session.parent().unwrap()).unwrap();
    std::fs::write(&session, "{\"type\":\"user\"}\n").unwrap();

    let (relay_url, relay_seen) = fake_relay();
    let sink: Arc<dyn TranscriptStore> = Arc::new(orchestrator.clone());
    let woke = wake(Config {
        orchestrator: orchestrator.clone(),
        node,
        workspace: workspace.clone(),
        state_dir: base.join("supervisor"),
        agent: AgentKind::Mock,
        holder: Arc::new(RelayHolder::new(relay_url)),
        transcripts: Some((projects.clone(), sink)),
        media: media(),
        budget: Budget { max_sessions: 1, max_duration: Duration::from_secs(600) },
        poll: Duration::from_millis(20),
        push_branch: true,
    })
    .unwrap();
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");

    // The supervisor's own writes (the implementation conversation, its
    // turns) and the mock agent's (plan steps) are on the orchestrator, in
    // the feed the app pulls.
    let feed = orchestrator.changes_after(seed_seq).unwrap();
    let tables: Vec<&str> = feed.changes.iter().map(|c| c.table.as_str()).collect();
    assert!(tables.contains(&"conversations"), "{tables:?}");
    assert!(tables.contains(&"conversation_turns"), "{tables:?}");
    assert!(tables.contains(&"node_plan_steps"), "{tables:?}");
    // Its local Files rewrite never leaves the sandbox.
    assert!(!tables.contains(&"node_files") && !tables.contains(&"node_fields"), "{tables:?}");

    // Held while it worked, released at the end.
    let seen = relay_seen.lock().unwrap().clone();
    assert!(seen.first().is_some_and(|l| l.starts_with("POST /hold?reason=supervisor&secs=120 ")), "{seen:?}");
    assert!(seen.last().is_some_and(|l| l.starts_with("POST /release?reason=supervisor ")), "{seen:?}");

    // The session file was mirrored.
    assert_eq!(orchestrator.fetch_transcript("-workspace-repo__0000-session").unwrap(), b"{\"type\":\"user\"}\n");

    // The branch was pushed.
    assert_eq!(git(&origin, &["rev-parse", "node-branch"]), git(&workspace, &["rev-parse", "HEAD"]));

    // A second wake reuses the copy (no reseed) and picks up the app's
    // changes made meanwhile.
    let woke = wake(Config {
        orchestrator: orchestrator.clone(),
        node,
        workspace: workspace.clone(),
        state_dir: base.join("supervisor"),
        agent: AgentKind::Mock,
        holder: Arc::new(RelayHolder::new(fake_relay().0)),
        transcripts: None,
        media: media(),
        budget: Budget { max_sessions: 1, max_duration: Duration::from_secs(600) },
        poll: Duration::from_millis(20),
        push_branch: false,
    })
    .unwrap();
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
}
