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
mod drafting;
mod interview;
mod node;
mod obligations;
mod plan;
mod visual_design;

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
    node                   Outline nodes: create, inspect, search, move, delete
    obligations            Requirements and constraints attached to a node
    drafting               Dumps, choices, and buildable while a node's spec is drafted
    content                A node's goal, design, and notes
    plan                   Structured, dependency-graph plan steps for a node
    questions              Interview questions for a node
    memory                 Interview memory notes for a node
    interview              Interview session state
    visual-design          the UI mockup associated with one obligation

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
    /// Writes are an agent's unless `TOD_INTERVIEW_ACTOR` names a session:
    /// `tod-cli` is the agents' interface, and the GUI never goes through it.
    pub fn client(&self) -> InterviewClient {
        InterviewClient::from_env_or(&self.data_root, tod_store::interview::ACTOR_AGENT)
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
        "node" => node::run(invocation),
        "obligations" => obligations::run(invocation),
        "drafting" => drafting::run(invocation),
        "content" => interview::content(invocation),
        "plan" => plan::run(invocation),
        "questions" => interview::questions(invocation),
        "memory" => interview::memory(invocation),
        "interview" => interview::interview(invocation),
        "visual-design" => visual_design::run(invocation),
        other => anyhow::bail!(
            "unknown noun `{other}` (expected: node, obligations, drafting, content, plan, questions, memory, interview, visual-design)"
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
                    display_name: "Interview".into(),
                    phase: "task-requirements-interview".into(),
                },
                InterviewSessionStatus::Active,
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
    fn node_search_finds_by_fuzzy_title() {
        let (root, node, _) = data_root();

        // `data_root()` already created a node titled "Node"; add a second,
        // more distinctively titled node to search for.
        let fleet = Arc::new(FleetStore::open(&root).unwrap());
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let login = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(login),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Reusable Login Component".into(),
            })
            .unwrap();
        drop(fleet);

        // Missing a letter ("logn" for "login") should still match via the
        // subsequence fallback.
        let results = cli(&root, &["node", "search", "--query", "logn"]).unwrap();
        let first_line = results.lines().next().unwrap();
        assert!(first_line.contains(&login.to_string()), "{results}");
        assert!(first_line.contains("Reusable Login Component"), "{results}");
        assert!(!results.contains(&node.to_string()), "{results}");

        let _ = std::fs::remove_dir_all(root);
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
        assert_eq!(
            listed,
            format!("[{short}] requirements/requirement (Core) <agent>: Keep it simple.")
        );

        // Attention needs its reason; both show in listings.
        let err = cli(&root, &["obligations", "update", &short, "--attention", "high"]).unwrap_err();
        assert!(err.to_string().contains("--why"), "{err}");
        cli(
            &root,
            &["obligations", "update", &short, "--attention", "high", "--why", "A taste call"],
        )
        .unwrap();
        let listed = cli(&root, &["obligations", "list", "--node", &node]).unwrap();
        assert!(listed.contains("<agent, high: A taste call>"), "{listed}");

        // References must name a node that exists.
        let err = cli(&root, &["obligations", "add", "--node", &node, "--kind", "req", "--body", "…"])
            .unwrap_err();
        assert!(err.to_string().contains("has no words"), "{err}");
        let err = cli(
            &root,
            &["obligations", "add", "--node", &node, "--kind", "req", "--body", "Uses [[no-such-node]]."],
        )
        .unwrap_err();
        assert!(err.to_string().contains("[[no-such-node]]"), "{err}");
        assert_eq!(cli(&root, &["obligations", "check-refs"]).unwrap(), "(none)");

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
    fn drafting_dumps_choices_and_buildable() {
        let (root, node, _) = data_root();
        let node = node.to_string();
        assert_eq!(
            cli(&root, &["drafting", "dump", "--node", &node, "--body", "Notes sync."]).unwrap(),
            "ok d-1"
        );
        let dumps = cli(&root, &["drafting", "dumps", "--node", &node]).unwrap();
        assert_eq!(dumps, "d-1 (not routed): Notes sync.");

        assert_eq!(cli(&root, &["drafting", "choices", "--node", &node]).unwrap(), "(none)");
        let err = cli(
            &root,
            &["drafting", "buildable", "--node", &node, "--outcome", "maybe"],
        )
        .unwrap_err();
        assert!(err.to_string().contains("pass"), "{err}");
        assert_eq!(
            cli(
                &root,
                &["drafting", "buildable", "--node", &node, "--outcome", "pass", "--detail", "Clear."]
            )
            .unwrap(),
            "ok"
        );
        let err = cli(&root, &["drafting", "withdraw-choice", "--node", &node, "c-4"]).unwrap_err();
        assert!(err.to_string().contains("c-4"), "{err}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn node_round_trip_with_slugs() {
        let (root, node, _) = data_root();
        let node_str = node.to_string();
        let node_slug = {
            let output = cli(&root, &["--json", "node", "show", &node_str]).unwrap();
            let json: serde_json::Value = serde_json::from_str(&output).unwrap();
            json["slug"].as_str().unwrap().to_string()
        };

        let created = cli(
            &root,
            &[
                "--json", "node", "create", "--parent", &node_slug, "--title", "Child One",
            ],
        )
        .unwrap();
        let created_json: serde_json::Value = serde_json::from_str(&created).unwrap();
        let child_id = created_json["id"].as_str().unwrap().to_string();
        let child_slug = created_json["slug"].as_str().unwrap().to_string();

        let listed = cli(&root, &["node", "list", "--parent", &node_slug]).unwrap();
        assert_eq!(listed, format!("{child_slug}  Child One"));

        // Addressable by id as well as by slug.
        let shown = cli(&root, &["node", "show", &child_id]).unwrap();
        assert!(shown.contains("Child One"), "{shown}");
        assert!(shown.contains(&node_str), "{shown}");

        // Renaming may resync the (non-manual) slug, so keep addressing by id.
        let renamed = cli(&root, &["node", "rename", &child_id, "--title", "Child Renamed"])
            .unwrap();
        assert!(renamed.starts_with("ok "), "{renamed}");

        // Move it to be a top-level (root) node in the same list.
        let moved = cli(&root, &["node", "move", &child_id, "--parent", "root"]).unwrap();
        assert!(moved.starts_with("ok "), "{moved}");
        let listed = cli(&root, &["node", "list", "--parent", &node_slug]).unwrap();
        assert_eq!(listed, "(none)");

        assert!(
            cli(&root, &["node", "delete", &child_id])
                .unwrap()
                .starts_with("ok "),
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn visual_design_save_writes_file_and_links_obligation() {
        let (root, node, _) = data_root();
        let node_str = node.to_string();

        let created = cli(
            &root,
            &[
                "obligations",
                "add",
                "--node",
                &node_str,
                "--kind",
                "requirement",
                "--phase",
                "design",
                "--body",
                "Login screen mockup",
            ],
        )
        .unwrap();
        let short = created
            .strip_prefix("ok ")
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .to_string();

        let html_path = root.join("mockup.html");
        std::fs::write(&html_path, "<div style=\"display:flex\">Hello</div>").unwrap();

        let saved = cli(
            &root,
            &[
                "visual-design",
                "save",
                "--obligation",
                &short,
                "--html-file",
                html_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        assert!(saved.starts_with("ok "), "{saved}");

        let saved_file = root.join("visual-design").join(&node_str);
        let entries: Vec<_> = std::fs::read_dir(&saved_file).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one saved mockup file");
        let saved_file = entries.into_iter().next().unwrap().unwrap().path();
        assert_eq!(
            std::fs::read_to_string(&saved_file).unwrap(),
            "<div style=\"display:flex\">Hello</div>"
        );

        let shown = cli(&root, &["visual-design", "show", "--obligation", &short]).unwrap();
        assert!(shown.contains("mockup.html") || saved_file.display().to_string() == shown, "{shown}");

        // Saving again overwrites rather than creating a second file.
        let saved2 = cli(
            &root,
            &[
                "visual-design",
                "save",
                "--obligation",
                &short,
                "--html-file",
                html_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        assert!(saved2.starts_with("ok "), "{saved2}");
        let entries: Vec<_> = std::fs::read_dir(root.join("visual-design").join(&node_str))
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1, "expected the mockup file to be overwritten, not duplicated");

        // A <script> tag is rejected.
        let script_path = root.join("bad.html");
        std::fs::write(&script_path, "<script>alert(1)</script>").unwrap();
        let err = cli(
            &root,
            &[
                "visual-design",
                "save",
                "--obligation",
                &short,
                "--html-file",
                script_path.to_str().unwrap(),
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("<script>"), "{err}");

        // Clearing removes the link.
        let cleared = cli(&root, &["visual-design", "clear", "--obligation", &short]).unwrap();
        assert!(cleared.starts_with("ok "), "{cleared}");
        let shown = cli(&root, &["visual-design", "show", "--obligation", &short]).unwrap();
        assert_eq!(shown, "(none)");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plan_round_trip_with_dependencies_and_obligations() {
        let (root, node, _) = data_root();
        let node = node.to_string();

        let req_added = cli(
            &root,
            &[
                "obligations", "add", "--node", &node, "--kind", "req", "--body",
                "Ship the thing.",
            ],
        )
        .unwrap();
        let req_short = req_added.strip_prefix("ok ").unwrap().to_string();

        let step_a = cli(&root, &["plan", "add", "--node", &node, "--body", "Do part A"]).unwrap();
        let step_a_short = step_a.strip_prefix("ok ").unwrap().to_string();
        let step_b = cli(
            &root,
            &[
                "plan", "add", "--node", &node, "--body", "Do part B", "--depends-on",
                &step_a_short, "--satisfies", &req_short,
            ],
        )
        .unwrap();
        let step_b_short = step_b.strip_prefix("ok ").unwrap().to_string();

        let listed = cli(&root, &["plan", "list", "--node", &node]).unwrap();
        assert!(listed.contains(&format!("[{step_a_short}] pending: Do part A")), "{listed}");
        assert!(
            listed.contains(&format!(
                "[{step_b_short}] pending deps=[{step_a_short}] satisfies=[{req_short}]: Do part B"
            )),
            "{listed}"
        );

        // A cycle is rejected.
        let cycle_err = cli(&root, &["plan", "depend", &step_a_short, "--on", &step_b_short])
            .unwrap_err();
        assert!(cycle_err.to_string().contains("cycle"), "{cycle_err}");

        // Only step A is ready until it's implemented.
        assert_eq!(cli(&root, &["plan", "ready", "--node", &node]).unwrap(), step_a_short);

        assert_eq!(
            cli(&root, &["plan", "update", &step_a_short, "--status", "implemented"]).unwrap(),
            format!("ok {step_a_short}")
        );
        // Step B auto-promotes to ready once its dependency is implemented.
        assert_eq!(cli(&root, &["plan", "ready", "--node", &node]).unwrap(), step_b_short);

        assert_eq!(
            cli(&root, &["plan", "delete", &step_b_short]).unwrap(),
            format!("ok {step_b_short}")
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
