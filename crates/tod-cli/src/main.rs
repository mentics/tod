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
mod changeset;
#[cfg(test)]
mod doc_sync;
mod incoming;
mod interview;
mod node;
mod obligations;
mod plan;
mod review;
mod secrets;
mod test_runs;
mod verdicts;
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
    content                A node's details, design, plan, and summary
    plan                   Structured, dependency-graph plan steps for a node
    questions              Interview questions for a node
    memory                 Interview memory notes for a node
    interview              Interview session state
    visual-design          the UI mockup associated with one obligation
    changeset              This conversation's net changes and unsure flags
    tests                  Record a test run for this implementation
    review                 Code review findings on a node, and their responses
    verdicts               What verification found for each obligation of a node
    incoming               Changes a node inherits, and the verdict that resolves them
    secrets                Run a command with stored secrets, without seeing them

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
            // Everything after `--` belongs to the command `secrets run`
            // starts, including its own `--json`.
            "--" => {
                rest.extend(args[i..].iter().cloned());
                break;
            }
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
        "content" => interview::content(invocation),
        "plan" => plan::run(invocation),
        "questions" => interview::questions(invocation),
        "memory" => interview::memory(invocation),
        "interview" => interview::interview(invocation),
        "visual-design" => visual_design::run(invocation),
        "changeset" => changeset::run(invocation),
        "tests" => test_runs::run(invocation),
        "review" => review::run(invocation),
        "verdicts" => verdicts::run(invocation),
        "incoming" => incoming::run(invocation),
        "secrets" => secrets::run(invocation),
        other => anyhow::bail!(
            "unknown noun `{other}` (expected: node, obligations, content, plan, questions, memory, interview, visual-design, changeset, tests, review, verdicts, incoming, secrets)"
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
    fn node_notes_append_without_touching_existing_ones() {
        let (root, node, _) = data_root();
        let node = node.to_string();
        assert_eq!(cli(&root, &["node", "notes", &node]).unwrap(), "(none)");

        let first = cli(&root, &["node", "add-note", &node, "--body", "First."]).unwrap();
        let first = first.strip_prefix("ok ").expect("one-line ack").to_string();
        cli(&root, &["node", "add-note", &node, "--body", "Second."]).unwrap();
        let err = cli(&root, &["node", "add-note", &node, "--body", "  "]).unwrap_err();
        assert!(err.to_string().contains("required"), "{err}");

        let listed = cli(&root, &["node", "notes", &node]).unwrap();
        assert!(listed.starts_with(&format!("[{first}]
First.

[")), "{listed}");
        assert!(listed.ends_with("]
Second."), "{listed}");

        let fleet = FleetStore::open(&root).unwrap();
        let notes = fleet
            .read(|conn| {
                Ok(tod_store::fleet::repos::task::TaskRepo::new(conn)
                    .notes(node.parse().unwrap())?)
            })
            .unwrap();
        assert_eq!(notes.len(), 2);
        drop(fleet);

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
            format!("[{short}] requirements/requirement (Core): Keep it simple.")
        );

        // Obligations carry no marks, so listings show none.
        assert!(!listed.contains('<'), "{listed}");

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

    /// A top-level Spec node titled `title` in the data root's one list, plus
    /// `children` Spec nodes, each a child of the one before (a chain).
    fn add_chain(root: &PathBuf, title: &str, children: &[&str]) -> Vec<Uuid> {
        let fleet = Arc::new(FleetStore::open(root).unwrap());
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let mut ids = Vec::new();
        let mut parent = None;
        for title in std::iter::once(&title).chain(children) {
            let id = Uuid::new_v4();
            fleet
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(id),
                    list_id,
                    parent_id: parent,
                    anchor_id: None,
                    position: if parent.is_some() {
                        CreatePosition::Child
                    } else {
                        CreatePosition::Below
                    },
                    title: title.to_string(),
                })
                .unwrap();
            fleet
                .enqueue_outline(OutlineMutation::EnableCapabilities {
                    node_id: id,
                    capabilities: vec![Capability::Spec],
                })
                .unwrap();
            ids.push(id);
            parent = Some(id);
        }
        ids
    }

    fn slug(root: &PathBuf, node: Uuid) -> String {
        let output = cli(root, &["--json", "node", "show", &node.to_string()]).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        json["slug"].as_str().unwrap().to_string()
    }

    #[test]
    fn obligations_and_plan_search_the_whole_project_without_a_node() {
        let (root, first, _) = data_root();
        let second = add_chain(&root, "Billing", &[])[0];
        let (first_str, second_str) = (first.to_string(), second.to_string());
        for (node, body) in [
            (&first_str, "Passwords are hashed with argon2."),
            (&second_str, "Invoices list every password reset fee."),
            (&second_str, "Totals round to the cent."),
        ] {
            cli(&root, &["obligations", "add", "--node", node, "--kind", "req", "--body", body])
                .unwrap();
            cli(&root, &["plan", "add", "--node", node, "--body", body]).unwrap();
        }

        // Neither a node nor a search: refused, with the reason.
        let err = cli(&root, &["obligations", "list"]).unwrap_err();
        assert!(err.to_string().contains("--search"), "{err}");
        let err = cli(&root, &["plan", "list"]).unwrap_err();
        assert!(err.to_string().contains("--search"), "{err}");
        let err = cli(&root, &["obligations", "list", "--search", "x", "--inherited"]).unwrap_err();
        assert!(err.to_string().contains("--node"), "{err}");

        let (first_slug, second_slug) = (slug(&root, first), slug(&root, second));
        let found = cli(&root, &["obligations", "list", "--search", "password"]).unwrap();
        let lines: Vec<&str> = found.lines().collect();
        assert_eq!(lines.len(), 2, "{found}");
        assert!(
            found.contains(&format!(" on {first_slug}: Passwords are hashed")),
            "{found}"
        );
        assert!(found.contains(&format!(" on {second_slug}: Invoices")), "{found}");
        assert!(!found.contains("Totals"), "{found}");

        let json = cli(&root, &["--json", "obligations", "list", "--search", "password"]).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert!(json[0]["node_slug"].is_string(), "{json}");

        let steps = cli(&root, &["plan", "list", "--search", "password"]).unwrap();
        assert_eq!(steps.lines().count(), 2, "{steps}");
        assert!(steps.contains(&format!(" on {first_slug}: Passwords")), "{steps}");
        assert!(steps.contains(&format!(" on {second_slug}: Invoices")), "{steps}");

        // With a node, a search narrows that node's list and names no node.
        let steps = cli(&root, &["plan", "list", "--node", &second_str, "--search", "cent"]).unwrap();
        assert!(steps.ends_with(": Totals round to the cent."), "{steps}");
        assert!(!steps.contains(" on "), "{steps}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn node_tree_indents_counts_and_respects_depth() {
        let (root, _, _) = data_root();
        let chain = add_chain(&root, "Top", &["Middle", "Bottom"]);
        let top = chain[0].to_string();
        cli(&root, &["obligations", "add", "--node", &top, "--kind", "req", "--body", "One."]).unwrap();
        cli(&root, &["obligations", "add", "--node", &top, "--kind", "con", "--body", "Two."]).unwrap();
        cli(&root, &["plan", "add", "--node", &chain[1].to_string(), "--body", "Step."]).unwrap();
        let slugs: Vec<String> = chain.iter().map(|id| slug(&root, *id)).collect();

        let full = cli(&root, &["node", "tree", &slugs[0]]).unwrap();
        assert_eq!(
            full,
            format!(
                "{}  Top  (obligations: 2, plan steps: 0)\n  {}  Middle  (obligations: 0, plan steps: 1)\n    {}  Bottom  (obligations: 0, plan steps: 0)",
                slugs[0], slugs[1], slugs[2]
            )
        );

        let one = cli(&root, &["node", "tree", &top, "--depth", "1"]).unwrap();
        assert_eq!(
            one,
            format!(
                "{}  Top  (obligations: 2, plan steps: 0)\n  {}  Middle  (obligations: 0, plan steps: 1, 1 more below)",
                slugs[0], slugs[1]
            )
        );

        let zero = cli(&root, &["node", "tree", &top, "--depth", "0"]).unwrap();
        assert_eq!(zero, format!("{}  Top  (obligations: 2, plan steps: 0, 1 more below)", slugs[0]));

        let json = cli(&root, &["--json", "node", "tree", &slugs[1]]).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2, "{json}");
        assert_eq!(json[1]["depth"], 1);

        assert!(cli(&root, &["node", "tree", &top, "--depth", "-1"]).is_err());
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
