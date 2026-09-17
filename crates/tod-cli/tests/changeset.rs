//! End to end: the real `tod-cli` binary, run as a conversation's agent
//! (`TOD_INTERVIEW_ACTOR=conversation:<id>`), records its writes in the
//! conversation's action log and reports them through `changeset`.

use std::path::{Path, PathBuf};
use std::process::Command;
use tod_store::conversation::{
    ActionActor, ActionKind, ConversationRepo, Entity, Focus, TurnRole, actor_for,
};
use tod_store::fleet::{FleetPaths, FleetStore, schema};
use tod_store::interview::ACTOR_ENV;
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use uuid::Uuid;

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A data root with one Spec node and one conversation focused on it. The
/// store is closed again so `tod-cli` opens it itself, as with no app running.
fn setup() -> (Root, Uuid, Uuid) {
    let root = std::env::temp_dir().join(format!("tod-cli-changeset-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let node = Uuid::new_v4();
    {
        let fleet = FleetStore::open(&root).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        for mutation in [
            OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Auth".into(),
            },
            OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Spec],
            },
        ] {
            fleet.enqueue_outline(mutation).unwrap();
        }
    }
    let conversation = with_repo(&root, |repo| {
        let id = repo.create(Focus::Node(node), None, None, None).unwrap().id;
        repo.append_turn(id, TurnRole::User, "Harden auth.").unwrap();
        id
    });
    (Root(root), node, conversation)
}

/// Run `f` on the conversation repo over a fresh writer connection.
fn with_repo<R>(root: &Path, f: impl FnOnce(ConversationRepo<'_>) -> R) -> R {
    let conn = schema::open_writer_connection(FleetPaths::new(root).unwrap().db()).unwrap();
    f(ConversationRepo::new(&conn))
}

/// Run `tod-cli` with `actor` as `TOD_INTERVIEW_ACTOR` (unset when `None`).
fn cli(root: &Path, actor: Option<&str>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tod-cli"));
    cmd.arg("--data-root").arg(root).args(args);
    match actor {
        Some(actor) => cmd.env(ACTOR_ENV, actor),
        None => cmd.env_remove(ACTOR_ENV),
    };
    let out = cmd.output().expect("run tod-cli");
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(stderr)
    }
}

fn short(ack: &str) -> String {
    ack.strip_prefix("ok ").expect("one-line ack").to_string()
}

#[test]
fn a_conversation_agents_writes_are_recorded_and_listed() {
    let (root, node, conversation) = setup();
    let root_path = root.0.clone();
    let actor = actor_for(conversation);
    let run = |args: &[&str]| cli(&root_path, Some(&actor), args).unwrap();
    let node_str = node.to_string();

    assert_eq!(run(&["changeset", "list"]), "(none)");

    let obligation = short(&run(&[
        "obligations", "add", "--node", &node_str, "--kind", "req", "--body", "Passwords are hashed.",
    ]));
    run(&["obligations", "update", &obligation, "--body", "Passwords are hashed with argon2."]);
    let step = short(&run(&["plan", "add", "--node", &node_str, "--body", "Add argon2."]));
    let removed = short(&run(&["plan", "add", "--node", &node_str, "--body", "Scratch step."]));
    run(&["plan", "delete", &removed]);

    // The action log: one row per recorded mutation, all the agent's, on the
    // user's turn.
    let actions = with_repo(&root_path, |repo| repo.actions(conversation).unwrap());
    let summary: Vec<(ActionKind, Entity)> = actions.iter().map(|a| (a.kind, a.entity)).collect();
    assert_eq!(
        summary,
        [
            (ActionKind::Create, Entity::Obligation),
            (ActionKind::Edit, Entity::Obligation),
            (ActionKind::Create, Entity::PlanStep),
            (ActionKind::Create, Entity::PlanStep),
            (ActionKind::Delete, Entity::PlanStep),
        ]
    );
    assert!(actions.iter().all(|a| a.actor == ActionActor::Agent && a.turn_seq == 1));
    assert_eq!(actions[1].after.as_ref().unwrap().text(), "Passwords are hashed with argon2.");

    // Net per item: create+edit is one `added`; create+delete is hidden.
    let slug = {
        let json: serde_json::Value =
            serde_json::from_str(&run(&["--json", "node", "show", &node_str])).unwrap();
        json["slug"].as_str().unwrap().to_string()
    };
    let listed = run(&["changeset", "list"]);
    assert_eq!(
        listed,
        format!(
            "added obligation {obligation} on {slug}: Passwords are hashed with argon2.\n\
             added plan-step {step} on {slug}: Add argon2."
        )
    );

    // Flag, then list shows it; unflag clears it.
    assert_eq!(
        run(&["changeset", "flag", "--obligation", &obligation, "--why", "Maybe bcrypt?"]),
        format!("ok {obligation}")
    );
    let listed = run(&["changeset", "list"]);
    assert!(
        listed.contains(&format!("{obligation} on {slug}: Passwords are hashed with argon2. <unsure: Maybe bcrypt?>")),
        "{listed}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&run(&["--json", "changeset", "list"])).unwrap();
    assert_eq!(json[0]["flag"], "Maybe bcrypt?");

    run(&["changeset", "unflag", "--obligation", &obligation]);
    assert!(!run(&["changeset", "list"]).contains("unsure"));
    run(&["changeset", "flag", "--plan-step", &step, "--why", "Order?"]);
    assert!(run(&["changeset", "list"]).contains("Add argon2. <unsure: Order?>"));
    // An item the conversation deleted still resolves by its short id.
    assert_eq!(
        run(&["changeset", "unflag", "--plan-step", &removed]),
        format!("ok {removed}")
    );

    // Only items the conversation changed can be flagged, and exactly one
    // item must be named, with a reason.
    let err = cli(&root_path, Some(&actor), &["changeset", "flag", "--node", &slug, "--why", "x"])
        .unwrap_err();
    assert!(err.contains("only items it changed can be flagged"), "{err}");
    let err = cli(&root_path, Some(&actor), &["changeset", "flag", "--obligation", &obligation])
        .unwrap_err();
    assert!(err.contains("--why"), "{err}");
    let err = cli(
        &root_path,
        Some(&actor),
        &["changeset", "flag", "--obligation", &obligation, "--plan-step", &step, "--why", "x"],
    )
    .unwrap_err();
    assert!(err.contains("exactly one"), "{err}");
}

#[test]
fn changeset_needs_a_conversation_actor() {
    let (root, _, conversation) = setup();
    for actor in [None, Some("user"), Some(&*Uuid::new_v4().to_string())] {
        let err = cli(&root.0, actor, &["changeset", "list"]).unwrap_err();
        assert!(err.contains("only works inside a conversation"), "{actor:?}: {err}");
    }
    let err = cli(
        &root.0,
        None,
        &["changeset", "flag", "--node", &conversation.to_string(), "--why", "x"],
    )
    .unwrap_err();
    assert!(err.contains("only works inside a conversation"), "{err}");

    // A well-formed actor naming no conversation is an error, not a fallback.
    let missing = actor_for(Uuid::new_v4());
    let err = cli(&root.0, Some(&missing), &["changeset", "list"]).unwrap_err();
    assert!(err.contains("not found"), "{err}");
}
