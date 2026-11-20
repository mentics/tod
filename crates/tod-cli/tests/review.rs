//! End to end: the real `tod-cli` binary, run as a review conversation's
//! agent, records findings with their root cause and the review's coverage,
//! and `review done` is refused, naming what is missing, until the coverage
//! is complete.

use std::path::{Path, PathBuf};
use std::process::Command;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::{FleetPaths, FleetStore, schema};
use tod_store::outline::{CreatePosition, OutlineMutation};
use tod_store::review::LENSES;
use uuid::Uuid;

/// A data root with one node and a review conversation on it. The store is
/// closed again so `tod-cli` opens it itself, as with no app running.
fn setup() -> (PathBuf, Uuid, Uuid) {
    let root = tod_test_util::scratch_dir("tod-cli-review");
    let node = Uuid::new_v4();
    {
        let fleet = FleetStore::open(&root).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Import".into(),
            })
            .unwrap();
    }
    let conn = schema::open_writer_connection(FleetPaths::new(&root).unwrap().db()).unwrap();
    let review = ConversationRepo::new(&conn)
        .create(Focus::Node(node), ProtocolKind::Review, None, None, None)
        .unwrap()
        .id;
    (root, node, review)
}

/// Run `tod-cli` as review conversation `conversation`'s agent on `node`.
fn cli(root: &Path, node: Uuid, conversation: Option<Uuid>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tod-cli"));
    cmd.arg("--data-root")
        .arg(root)
        .args(args)
        .env("TOD_NO_DAEMON", "1")
        .env_remove("TOD_DATA_ROOT")
        .env("TOD_IMPLEMENT_NODE", node.to_string());
    match conversation {
        Some(id) => cmd.env("TOD_IMPLEMENT_CONVERSATION", id.to_string()),
        None => cmd.env_remove("TOD_IMPLEMENT_CONVERSATION"),
    };
    let out = cmd.output().expect("run tod-cli");
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    if out.status.success() { Ok(stdout) } else { Err(stderr) }
}

fn short(ack: &str) -> String {
    ack.strip_prefix("ok ").expect("one-line ack").to_string()
}

#[test]
fn a_finding_carries_its_root_cause() {
    let (root, node, review) = setup();
    let id = short(
        &cli(
            &root,
            node,
            Some(review),
            &[
                "review", "add", "--severity", "high", "--summary", "Picks an arbitrary org id",
                "--root-cause", "The org's supplier id is treated as unique",
            ],
        )
        .unwrap(),
    );
    let shown = cli(&root, node, Some(review), &["review", "show", &id]).unwrap();
    assert!(shown.contains("root cause: The org's supplier id is treated as unique"), "{shown}");
    let listed = cli(&root, node, Some(review), &["review", "list"]).unwrap();
    assert!(listed.contains("root cause: The org's supplier id is treated as unique"), "{listed}");
    let json = cli(&root, node, Some(review), &["--json", "review", "show", &id]).unwrap();
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["root_cause"], "The org's supplier id is treated as unique");
}

#[test]
fn done_is_refused_until_the_coverage_is_complete() {
    let (root, node, review) = setup();
    let run = |args: &[&str]| cli(&root, node, Some(review), args);

    let err = run(&["review", "done"]).unwrap_err();
    assert!(err.contains("an entry for 11 of the 11 lenses"), "{err}");
    assert!(err.contains("a saturation pass"), "{err}");

    let finding = short(&run(&["review", "add", "--severity", "medium", "--summary", "No ORDER BY"]).unwrap());
    let ack = run(&[
        "review", "coverage", "--lens", "data-assumptions", "--file", "digikey.py", "--checked",
        "org id lookup: queried prod, 113 orgs with more than one id", "--result",
        &format!("finding:{finding}"), "--read", "REVIEW.md",
    ])
    .unwrap();
    assert_eq!(ack, "ok coverage lens data-assumptions");
    run(&["review", "coverage", "--lens", "external-systems", "--result", "n/a", "--reason", "No API is called"])
        .unwrap();
    for lens in LENSES.iter().filter(|l| !matches!(l.slug, "data-assumptions" | "external-systems" | "reach")) {
        run(&["review", "coverage", "--lens", lens.slug, "--checked", "read every path", "--result", "none"]).unwrap();
    }
    let err = run(&["review", "done"]).unwrap_err();
    assert!(err.contains("an entry for 1 of the 11 lenses: reach."), "{err}");

    run(&["review", "coverage", "--lens", "reach", "--checked", "every caller", "--result", "none"]).unwrap();
    run(&["review", "coverage", "--pass", "saturation", "--checked", "where findings cluster", "--result", "none"])
        .unwrap();
    assert_eq!(run(&["review", "done"]).unwrap(), "ok review done");
}

#[test]
fn a_malformed_coverage_entry_is_refused() {
    let (root, node, review) = setup();
    let run = |args: &[&str]| cli(&root, node, Some(review), args);
    let err = run(&["review", "coverage", "--lens", "style", "--checked", "x", "--result", "none"]).unwrap_err();
    assert!(err.contains("unknown lens `style`"), "{err}");
    let err = run(&["review", "coverage", "--checked", "x", "--result", "none"]).unwrap_err();
    assert!(err.contains("--lens <LENS> or --pass saturation is required"), "{err}");
    let err = run(&["review", "coverage", "--lens", "reach", "--result", "n/a"]).unwrap_err();
    assert!(err.contains("needs --reason"), "{err}");
    let err = run(&["review", "coverage", "--lens", "reach", "--checked", "x", "--result", "none", "--reason", "y"])
        .unwrap_err();
    assert!(err.contains("--reason goes with --result n/a only"), "{err}");
    let err = run(&["review", "coverage", "--lens", "reach", "--checked", "x"]).unwrap_err();
    assert!(err.contains("--result is required"), "{err}");
    let err = run(&["review", "coverage", "--pass", "saturation", "--result", "n/a", "--reason", "y"]).unwrap_err();
    assert!(err.contains("always applies"), "{err}");

    // Outside a review conversation there is no review to cover.
    let err = cli(&root, node, None, &["review", "done"]).unwrap_err();
    assert!(err.contains("only works inside a review conversation"), "{err}");
}
