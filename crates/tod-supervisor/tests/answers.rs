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
    // The question says what was spent and what each answer does, and the
    // beat the wake left says why it stopped.
    let (question, beat) = {
        let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
        let question: String = conn.query_row("SELECT question FROM decisions", [], |r| r.get(0)).unwrap();
        (question, tod_store::cloud_activity::get(&conn, s.node).unwrap().unwrap())
    };
    assert!(
        question.starts_with("Used 1 of 1 sessions (")
            && question.contains("Keep going gives it 1 more sessions and ")
            && question.contains("Leave it stopped keeps it stopped until you change this answer."),
        "{question}"
    );
    assert!(beat.is_idle() && beat.stopped.as_deref().is_some_and(|r| r.contains("budget is spent")), "{beat:?}");

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
    // The beat it left says so, naming the question and what resumes it.
    {
        let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
        let beat = tod_store::cloud_activity::get(&conn, s.node).unwrap().unwrap();
        let reason = beat.stopped.expect("a wake that left the node stopped left no reason");
        assert!(
            reason.starts_with("left stopped: ") && reason.contains("failing several times") && reason.contains("Leave it stopped"),
            "{reason}"
        );
        let state = tod_store::run_state::get(&conn, s.node).unwrap().expect("no run state left");
        assert_eq!(state.phase, tod_store::run_state::Phase::Stopped);
        assert_eq!(state.reason_kind, tod_core::run_state::kind::LEFT_STOPPED);
        assert_eq!(state.reason_text, reason);
    }
    // Held only while it looked: released before going back to sleep.
    let seen = seen.lock().unwrap().clone();
    assert!(seen.last().is_some_and(|l| l.starts_with("POST /release?reason=supervisor ")), "{seen:?}");
    assert_eq!(count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns"), turns);
    // And it asked nothing more.
    assert_eq!(count(&s.remote_db, "SELECT COUNT(*) FROM decisions"), 1);
}

#[test]
fn a_request_to_run_overrides_leave_it_stopped_and_is_taken() {
    let s = setup("override");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(Outcome::BudgetExhausted { .. })), "{woke:?}");
    answer(&s, 2); // Leave it stopped
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::LeftStopped(_)), "{woke:?}");

    // "Run with a new session": the request file.
    let state = s.base.join("supervisor");
    let request = state.join(tod_core::autopilot::FRESH_SESSION_REQUEST_FILE);
    std::fs::write(&request, b"").unwrap();
    let turns = count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert!(!request.exists(), "the request was taken");
    assert!(count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns") > turns, "the node ran");

    // The next scheduled wake, with no request, does not revert to stopped.
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(!matches!(woke, Woke::LeftStopped(_)), "{woke:?}");
}

/// A request row the app wrote (as sync delivers it to the orchestrator).
fn request(s: &Setup, kind: &str) -> Uuid {
    let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    tod_store::node_requests::record(&conn, s.node, kind, "app", 1).unwrap()
}

/// `(consumed_at set, consumed_by)` of a request, read from the orchestrator.
fn consumption(s: &Setup, id: Uuid) -> (bool, Option<String>) {
    let conn = rusqlite::Connection::open(&s.remote_db).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    let row = tod_store::node_requests::list_for_node(&conn, s.node)
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap();
    (row.consumed_at.is_some(), row.consumed_by)
}

/// A request written while the supervisor was not running is consumed at its
/// next wake, supersedes the older answer to leave the node stopped, and
/// records who took it and when; the next wake has nothing to take.
#[test]
fn a_request_row_written_while_asleep_supersedes_a_stop_answer_and_is_consumed() {
    let s = setup("row-override");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(Outcome::BudgetExhausted { .. })), "{woke:?}");
    answer(&s, 2); // Leave it stopped
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::LeftStopped(_)), "{woke:?}");

    let id = request(&s, tod_store::node_requests::KIND_KEEP_GOING);
    assert_eq!(consumption(&s, id), (false, None));
    let turns = count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert!(count(&s.remote_db, "SELECT COUNT(*) FROM conversation_turns") > turns, "the node ran");
    let (consumed, by) = consumption(&s, id);
    assert!(consumed, "consumed at the wake");
    assert!(by.unwrap().starts_with("supervisor:"));
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(!matches!(woke, Woke::LeftStopped(_)), "{woke:?}");
}

/// A wake that exits early (another supervisor holds the node) takes nothing:
/// the request is still there for the next wake.
#[test]
fn an_early_exit_leaves_a_request_unconsumed() {
    let s = setup("row-early");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    let id = request(&s, tod_store::node_requests::KIND_RUN);

    let state = s.base.join("supervisor");
    let held = match tod_supervisor::lock::acquire(&state).unwrap() {
        tod_supervisor::lock::Taken::Held(l) => l,
        tod_supervisor::lock::Taken::Busy { .. } => panic!("nobody holds it yet"),
    };
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::AlreadyRunning { .. }), "{woke:?}");
    assert_eq!(consumption(&s, id), (false, None), "an early exit consumes nothing");
    drop(held);

    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert!(consumption(&s, id).0, "the next wake took it");
}

/// "Check now" does not countermand a stop answer the way a request to run does.
#[test]
fn a_check_now_request_does_not_supersede_leave_it_stopped() {
    let s = setup("row-check");
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::Ran(Outcome::BudgetExhausted { .. })), "{woke:?}");
    answer(&s, 2);
    let id = request(&s, tod_store::node_requests::KIND_CHECK_NOW);
    let woke = wake_once(&s, fake_relay().0, 1);
    assert!(matches!(woke, Woke::LeftStopped(_)), "{woke:?}");
    assert_eq!(consumption(&s, id), (false, None));
}
