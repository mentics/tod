//! The user's answers to the questions the supervisor and the watchdog ask
//! (`tod_core::stop_questions`) take effect on the next wake.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_core::autopilot::{Budget, Outcome};
use tod_core::stop_questions;
use tod_store::decisions::{DecisionRepo, NewDecision};
use tod_store::fleet::FleetStore;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use tod_supervisor::agent::AgentKind;
use tod_supervisor::hold::RelayHolder;
use tod_supervisor::orchestrator::Orchestrator;
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

fn count(db: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

struct Setup {
    _tmp: Temp,
    base: PathBuf,
    node: Uuid,
    orchestrator: Orchestrator,
    workspace: PathBuf,
    remote_db: PathBuf,
}

fn setup(name: &str) -> Setup {
    let tmp = Temp(std::env::temp_dir().join(format!("tod-supervisor-{name}-{}", Uuid::new_v4())));
    let base = tmp.0.clone();
    std::fs::create_dir_all(&base).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let orch_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tod_orchestrator::Server::new(tod_orchestrator::Config {
        base: base.join("orchestrator"),
        tod_cli: "tod-cli-not-used".into(),
        tod_cli_prefix: Vec::new(),
    })
    .unwrap();
    std::thread::spawn(move || server.serve(listener));
    let app = base.join("app");
    let node = app_database(&app);
    let snap = base.join("snap.db");
    tod_store::sync::snapshot(&app.join("tod.db"), &snap).unwrap();
    let seeded = ureq::post(format!("{orch_url}/users/alice/seed")).send(std::fs::read(&snap).unwrap()).unwrap();
    assert_eq!(seeded.status(), 200);
    let orchestrator = Orchestrator::new(orch_url, "alice", node.to_string());
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    git(&workspace, &["init", "--quiet"]);
    git(&workspace, &["checkout", "--quiet", "-b", "node-branch"]);
    std::fs::write(workspace.join("README"), "hi\n").unwrap();
    git(&workspace, &["add", "README"]);
    git(&workspace, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "init"]);
    let remote_db = base.join("orchestrator").join("users").join("alice").join("tod.db");
    Setup { _tmp: tmp, base, node, orchestrator, workspace, remote_db }
}

fn wake_once(s: &Setup, relay: String, max_sessions: u32) -> Woke {
    wake(Config {
        orchestrator: s.orchestrator.clone(),
        node: s.node,
        workspace: s.workspace.clone(),
        state_dir: s.base.join("supervisor"),
        agent: AgentKind::Mock,
        holder: Arc::new(RelayHolder::new(relay)),
        transcripts: None,
        media: media(),
        budget: Budget { max_sessions, max_duration: Duration::from_secs(600) },
        guards: Default::default(),
        poll: Duration::from_millis(20),
        push_branch: false,
        scheduler: None,
        sandbox: "node-test".into(),
    })
    .unwrap()
}

/// Answers the node's latest stop question on the orchestrator's store, as
/// the app's answer would arrive there by sync.
fn answer(s: &Setup, option: i64) {
    let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    let latest = stop_questions::latest(&conn, s.node).unwrap().expect("a stop question");
    DecisionRepo::new(&conn).answer(latest.decision, Some(option), None, "user").unwrap();
}

#[test]
fn keep_going_after_the_budget_grants_another() {
    let s = setup("budget");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(Outcome::BudgetExhausted { .. })), "{woke:?}");
    let latest = {
        let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
        stop_questions::latest(&conn, s.node).unwrap().expect("asked")
    };
    assert_eq!(latest.kind, stop_questions::BUDGET);
    assert_eq!(latest.answer, stop_questions::Answer::Pending);

    // Unanswered, it stays stopped on the question, and does not ask again.
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(Outcome::BudgetExhausted { .. })), "{woke:?}");
    assert_eq!(count(&s.remote_db, "SELECT COUNT(*) FROM decisions"), 1);

    let turns = count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns");
    answer(&s, 1); // Keep going
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert!(
        count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns") > turns,
        "the wake went on past the first budget"
    );
}

#[test]
fn leave_it_stopped_after_failures_keeps_the_node_stopped() {
    let s = setup("failures");
    // The question a supervisor asks after repeated failures.
    {
        let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        DecisionRepo::new(&conn)
            .create(
                s.node,
                None,
                Some(stop_questions::FAILURES),
                &NewDecision {
                    question: "The agent failed 3 times in a row. Keep going?".into(),
                    options: stop_questions::SUPERVISOR_OPTIONS.iter().map(|o| o.to_string()).collect(),
                    evidence: Vec::new(),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    answer(&s, 2); // Leave it stopped
    let turns = count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns");
    let (relay, seen) = fake_relay();
    let woke = wake_once(&s, relay, 30);
    assert!(matches!(woke, Woke::LeftStopped(_)), "{woke:?}");
    assert!(seen.lock().unwrap().is_empty(), "no hold: nothing ran");
    assert_eq!(count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns"), turns);
    // And it asked nothing more.
    assert_eq!(count(&s.remote_db, "SELECT COUNT(*) FROM decisions"), 1);
}
