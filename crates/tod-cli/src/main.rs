//! `tod-cli` — the interface agents use to read and change tod's data.
//!
//! Every mutation goes through `tod_store`'s writer, the same path the GUI
//! uses, so invariants cannot be bypassed. Raw SQL is deliberately not
//! exposed: an agent gets a small verified vocabulary rather than the schema.
//!
//! Interview agents run with `TOD_INTERVIEW_ACTOR` set; their writes are
//! attributed to that session and refused when they would act on something
//! that changed since the session's context was built.

mod args;
mod interview;
mod obligations;

use std::path::PathBuf;
use std::process::ExitCode;
use tod_core::interview::client::InterviewClient;

const USAGE: &str = "\
tod-cli — read and modify tod data

USAGE:
    tod-cli --data-root <PATH> <NOUN> <COMMAND> [OPTIONS]

GLOBAL OPTIONS:
    --data-root <PATH>   Directory holding the tod database (required)
    --json               Emit JSON instead of text
    -h, --help           Show this help

NOUNS:
    obligations          Requirements and constraints attached to a node
    content              A node's goal, design, and plan
    questions            Interview questions for a node
    memory               Interview memory notes for a node
    interview            Interview session state

Run `tod-cli <NOUN> --help` for that noun's commands.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("tod-cli: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Parsed global options plus the remaining noun/command arguments.
pub struct Invocation {
    pub data_root: PathBuf,
    pub json: bool,
    pub rest: Vec<String>,
}

impl Invocation {
    pub fn client(&self) -> InterviewClient {
        InterviewClient::from_env(&self.data_root)
    }
}

fn run(args: &[String]) -> anyhow::Result<String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") && args.len() == 1 {
        return Ok(USAGE.trim_end().to_string());
    }

    let mut data_root: Option<PathBuf> = None;
    let mut json = false;
    let mut rest: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-root" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--data-root requires a path"))?;
                data_root = Some(PathBuf::from(value));
            }
            "--json" => json = true,
            other => rest.push(other.to_string()),
        }
        i += 1;
    }

    if rest.is_empty() {
        return Ok(USAGE.trim_end().to_string());
    }

    let data_root = data_root.ok_or_else(|| {
        anyhow::anyhow!(
            "--data-root <PATH> is required (the agent context message supplies the value)"
        )
    })?;
    if !data_root.is_dir() {
        anyhow::bail!("data root {} does not exist", data_root.display());
    }

    let noun = rest.remove(0);
    let invocation = Invocation {
        data_root,
        json,
        rest,
    };

    match noun.as_str() {
        "obligations" => obligations::run(invocation),
        "content" => interview::content(invocation),
        "questions" => interview::questions(invocation),
        "memory" => interview::memory(invocation),
        "interview" => interview::interview(invocation),
        other => anyhow::bail!(
            "unknown noun `{other}` (expected: obligations, content, questions, memory, interview)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::run;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tod_core::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
    use tod_store::fleet::FleetStore;
    use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
    use uuid::Uuid;

    /// A data root holding one Spec node and its interview session. The store
    /// is closed again so `tod-cli` opens it itself, as with no app running.
    fn data_root() -> (PathBuf, Uuid, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-cli-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = Arc::new(FleetStore::open(&root).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        for mutation in [
            OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Node".into(),
            },
            OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Spec],
            },
        ] {
            fleet.enqueue_outline(mutation).unwrap();
        }
        let session = SessionStore::open(fleet.clone())
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id: node,
                    agent_config_id: None,
                    display_name: "Interview".into(),
                    phase: "task-requirements-interview".into(),
                },
                InterviewSessionStatus::Active,
                None,
            )
            .unwrap()
            .id;
        drop(fleet);
        (root, node, session)
    }

    fn cli(root: &PathBuf, args: &[&str]) -> anyhow::Result<String> {
        let mut full = vec!["--data-root".to_string(), root.display().to_string()];
        full.extend(args.iter().map(|a| a.to_string()));
        run(&full)
    }

    #[test]
    fn obligations_round_trip_with_short_ids_and_sections() {
        let (root, node, _) = data_root();
        let node = node.to_string();
        let added = cli(
            &root,
            &[
                "obligations", "add", "--node", &node, "--kind", "req", "--body",
                "Keep it simple.", "--section", "Core",
            ],
        )
        .unwrap();
        let short = added.strip_prefix("ok ").expect("one-line ack").to_string();
        assert_eq!(short.len(), 8);

        let listed = cli(&root, &["obligations", "list", "--node", &node]).unwrap();
        assert_eq!(listed, format!("[{short}] requirement (Core): Keep it simple."));

        let updated = cli(
            &root,
            &["obligations", "update", &short, "--body", "Keep it simpler.", "--section", ""],
        )
        .unwrap();
        assert_eq!(updated, format!("ok {short}"));
        let shown = cli(&root, &["obligations", "show", &short]).unwrap();
        assert!(shown.contains("Keep it simpler."), "{shown}");
        assert!(!shown.contains("(Core)"), "section cleared: {shown}");

        assert_eq!(
            cli(&root, &["obligations", "delete", &short]).unwrap(),
            format!("ok {short}")
        );
        assert_eq!(
            cli(&root, &["obligations", "list", "--node", &node]).unwrap(),
            "(none)"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn memory_questions_and_interview_commands() {
        let (root, node, session) = data_root();
        let node = node.to_string();
        let added = cli(
            &root,
            &[
                "memory", "add", "--node", &node, "--kind", "parked", "--phase", "design",
                "--body", "Use the vendor API.",
            ],
        )
        .unwrap();
        assert_eq!(added, "ok m-1");
        let memory = cli(&root, &["memory", "list", "--node", &node, "--kind", "parked"]).unwrap();
        assert_eq!(memory, "m-1 parked (open): Use the vendor API.");
        assert_eq!(
            cli(&root, &["memory", "update", "--node", &node, "m-1", "--status", "done"]).unwrap(),
            "ok m-1"
        );
        let err = cli(
            &root,
            &["memory", "add", "--node", &node, "--kind", "parked", "--body", "No phase."],
        )
        .unwrap_err();
        assert!(err.to_string().contains("--phase"), "{err}");

        assert_eq!(
            cli(&root, &["questions", "list", "--node", &node]).unwrap(),
            "(none)"
        );
        let err = cli(
            &root,
            &["questions", "withdraw", "--node", &node, "q-9", "--reason", "gone"],
        )
        .unwrap_err();
        assert!(err.to_string().contains("q-9 not found"), "{err}");

        let session = session.to_string();
        assert_eq!(
            cli(
                &root,
                &["interview", "exhausted", "--session", &session, "--reason", "All asked."]
            )
            .unwrap(),
            "ok"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
