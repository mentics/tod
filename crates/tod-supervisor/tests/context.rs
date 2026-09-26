//! A context change the orchestrator marked on a running node is taken at
//! the supervisor's next stopping point: the node is moved back when its
//! state no longer holds, the current conversation's session is ended (said
//! in its transcript), and the mark is remembered so it is taken once.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tod_core::autopilot::Budget;
use tod_store::fleet::FleetStore;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use tod_supervisor::agent::AgentKind;
use tod_supervisor::hold::Holder;
use tod_supervisor::orchestrator::Orchestrator;
use tod_supervisor::{Config, Woke, context, wake};
use uuid::Uuid;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct NoHold;
impl Holder for NoHold {
    fn hold(&self, _: &str, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn release(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A node in `active` with Files and a two-step plan, running in the cloud.
fn app_database(root: &Path) -> Uuid {
    let fleet = FleetStore::open(root).unwrap();
    fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
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
    let conn = rusqlite::Connection::open(root.join("tod.db")).unwrap();
    tod_store::cloud_nodes::upsert(&conn, node, "node-test", "alice", 1).unwrap();
    node
}

fn media() -> tod_core::media::MediaPaths {
    tod_core::media::MediaPaths::from_media_root(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("tod").join("media"),
    )
    .unwrap()
}

fn config(orchestrator: &Orchestrator, node: Uuid, base: &Path) -> Config {
    Config {
        orchestrator: orchestrator.clone(),
        node,
        workspace: base.join("workspace"),
        state_dir: base.join("supervisor"),
        agent: AgentKind::Mock,
        holder: Arc::new(NoHold),
        transcripts: None,
        media: media(),
        budget: Budget { max_sessions: 1, max_duration: Duration::from_secs(600) },
        poll: Duration::from_millis(20),
        push_branch: false,
        scheduler: None,
        sandbox: "node-test".into(),
    }
}

fn remote(base: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(base.join("orchestrator").join("users").join("alice").join("tod.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    conn
}

fn context_notes(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT body FROM conversation_turns WHERE role = 'rotation' AND body LIKE 'The context changed%'")
        .unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

#[test]
fn a_context_change_is_taken_at_the_next_stopping_point() {
    let tmp = Temp(std::env::temp_dir().join(format!("tod-supervisor-context-{}", Uuid::new_v4())));
    let base = tmp.0.clone();
    std::fs::create_dir_all(&base).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
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
    let orchestrator = Orchestrator::new(orch_url.clone(), "alice", node.to_string());

    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    git(&workspace, &["init", "--quiet"]);

    // A first wake: no mark yet; it leaves a conversation behind.
    let woke = wake(config(&orchestrator, node, &base)).unwrap();
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert!(context_notes(&remote(&base)).is_empty());
    assert_eq!(context::load_seen(&base.join("supervisor")), 0);

    // Meanwhile, the node is put in `review` with its plan still open (its
    // state no longer holds) and the orchestrator marks its context changed,
    // as `impact_handler` does.
    let mark = 1_000_000;
    {
        let conn = remote(&base);
        conn.execute("UPDATE node_lifecycle SET state = 'review' WHERE node_id = ?1", [node.as_bytes().to_vec()])
            .unwrap();
        tod_store::cloud_nodes::mark_context_changed(&conn, node, mark).unwrap();
    }

    let woke = wake(config(&orchestrator, node, &base)).unwrap();
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert_eq!(context::load_seen(&base.join("supervisor")), mark, "the mark was taken");
    let notes = context_notes(&remote(&base));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("`review` no longer holds, so the supervisor moved the node back to"), "{notes:?}");

    // Taken once: another wake with no new mark adds no note.
    let woke = wake(config(&orchestrator, node, &base)).unwrap();
    assert!(matches!(woke, Woke::Ran(_)), "{woke:?}");
    assert_eq!(context_notes(&remote(&base)).len(), 1);
}
