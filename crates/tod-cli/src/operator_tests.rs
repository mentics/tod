//! The hidden operator tier end to end: hidden and visible help, refusal in an
//! agent session, attribution, `decisions answer`, and a runner verb against a
//! test store with a played daemon.

use crate::args::Args;
use crate::tests::{data_root, set_lifecycle};
use crate::{Invocation, run_with};
use std::cell::RefCell;
use std::path::PathBuf;
use tod_core::autopilot::local::RunnerState;
use tod_core::interview::client::InterviewClient;
use tod_core::runner_operator::{Ctx, Daemon, Who};
use tod_store::decisions::DecisionRepo;
use tod_store::fleet::FleetStore;
use tod_store::interview::ACTOR_USER;
use uuid::Uuid;

fn env(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let pairs: Vec<(&'static str, &'static str)> = pairs.to_vec();
    move |name: &str| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.to_string())
}

const ON: (&str, &str) = ("TOD_OPERATOR", "shepherd");

fn cli_with(root: &PathBuf, env: impl Fn(&str) -> Option<String>, args: &[&str]) -> anyhow::Result<String> {
    let mut full = vec!["--data-root".to_string(), root.display().to_string()];
    full.extend(args.iter().map(|a| a.to_string()));
    run_with(&full, &env)
}

fn help_with(env: impl Fn(&str) -> Option<String>, args: &[&str]) -> anyhow::Result<String> {
    run_with(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>(), &env)
}

#[test]
fn without_the_tier_nothing_mentions_it_and_invoking_it_is_unknown() {
    let off = env(&[]);
    let top = help_with(&off, &["--help"]).unwrap();
    assert!(!top.contains("runner"), "{top}");
    assert!(!top.contains("operator tier"), "{top}");
    // `runner --help` is the top usage, as for any unknown noun.
    assert_eq!(help_with(&off, &["runner", "--help"]).unwrap(), top);
    let decisions = help_with(&off, &["decisions", "--help"]).unwrap();
    assert!(!decisions.contains("    answer <ID>"), "{decisions}");
    assert_eq!(decisions, crate::decisions::USAGE.trim_end());
    for words in [&["help", "runner"][..], &["help", "migrate"], &["help", "answer"], &["help", "sandbox"]] {
        let found = help_with(&off, words).unwrap();
        assert!(!found.contains("tod-cli runner") && !found.contains("decisions answer"), "{words:?}: {found}");
    }

    let (root, node, _) = data_root();
    let id = node.to_string();
    let err = cli_with(&root, &off, &["runner", "menu", &id]).unwrap_err().to_string();
    assert!(err.contains("unknown noun `runner`"), "{err}");
    let err = cli_with(&root, &off, &["decisions", "answer", "abcd1234", "--option", "1"]).unwrap_err().to_string();
    assert!(err.contains("unknown command `answer`"), "{err}");
    // Not even `TOD_ACTOR` turns it on, nor a different value.
    let err = cli_with(&root, env(&[("TOD_ACTOR", "shepherd:x")]), &["runner", "menu", &id]).unwrap_err().to_string();
    assert!(err.contains("unknown noun `runner`"), "{err}");
    let err = cli_with(&root, env(&[("TOD_OPERATOR", "yes")]), &["runner", "menu", &id]).unwrap_err().to_string();
    assert!(err.contains("unknown noun `runner`"), "{err}");
}

#[test]
fn with_the_tier_the_help_lists_it() {
    let on = env(&[ON]);
    let top = help_with(&on, &["--help"]).unwrap();
    assert!(top.contains("runner "), "{top}");
    assert!(top.contains("answer (operator tier)"), "{top}");
    assert_eq!(help_with(&on, &["runner", "--help"]).unwrap(), crate::operator::RUNNER_USAGE.trim_end());
    assert_eq!(help_with(&on, &["runner", "menu", "-h"]).unwrap(), crate::operator::RUNNER_USAGE.trim_end());
    assert_eq!(help_with(&on, &["help", "runner"]).unwrap(), crate::operator::RUNNER_USAGE.trim_end());
    let decisions = help_with(&on, &["decisions", "--help"]).unwrap();
    assert!(decisions.contains("    answer <ID> (--option <N> | --text <TEXT>)"), "{decisions}");
    let found = help_with(&on, &["help", "migrate"]).unwrap();
    assert!(found.contains("tod-cli runner migrate <NODE> --to <LOCATION>"), "{found}");
    let found = help_with(&on, &["help", "answer"]).unwrap();
    assert!(found.contains("tod-cli decisions answer <ID>"), "{found}");
    // The usage names every menu entry's verb.
    let usage = crate::operator::RUNNER_USAGE;
    for verb in tod_core::runner_operator::Verb::ALL {
        assert!(usage.contains(&format!("    {} <NODE>", verb.name())), "{}", verb.name());
    }
}

#[test]
fn the_operator_tier_is_refused_in_an_agent_session_even_when_on() {
    let (root, node, _) = data_root();
    set_lifecycle(&root, node, "active");
    let id = node.to_string();
    for agent in [
        ("TOD_INTERVIEW_ACTOR", "conversation:abc"),
        ("TOD_IMPLEMENT_NODE", "00000000-0000-0000-0000-000000000000"),
    ] {
        let agent_env = env(&[ON, agent]);
        let err = cli_with(&root, &agent_env, &["runner", "menu", &id]).unwrap_err().to_string();
        assert!(err.contains("operator only") && err.contains(agent.0), "{err}");
        let err = cli_with(&root, &agent_env, &["runner", "start", &id]).unwrap_err().to_string();
        assert!(err.contains("operator only"), "{err}");
        let err = cli_with(&root, &agent_env, &["decisions", "answer", "abcd1234", "--option", "1"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("operator only") && err.contains(agent.0), "{err}");
    }
    // Help is still help in an agent session.
    assert!(help_with(env(&[ON, ("TOD_INTERVIEW_ACTOR", "x")]), &["runner", "--help"]).is_ok());
}

fn ask(root: &PathBuf, node: Uuid, question: &str, options: &[&str], extra: &[&str]) -> String {
    let id = node.to_string();
    let mut args = vec!["decisions", "ask", "--node", id.as_str(), question];
    for option in options {
        args.extend(["--option", option]);
    }
    args.extend(extra);
    let out = cli_with(root, env(&[]), &args).unwrap();
    out.strip_prefix("ok ").unwrap().trim().to_string()
}

fn answer_actors(root: &PathBuf, short: &str) -> Vec<(Option<i64>, Option<String>, String)> {
    let client = InterviewClient::new(root, ACTOR_USER);
    client
        .read(|conn| {
            let repo = DecisionRepo::new(conn);
            let id = repo.resolve(short)?;
            Ok(repo
                .get_with_answers(id)?
                .unwrap()
                .answers
                .into_iter()
                .map(|a| (a.option, a.text, a.actor))
                .collect())
        })
        .unwrap()
}

fn timeline_titles(root: &PathBuf, node: Uuid) -> Vec<String> {
    InterviewClient::new(root, ACTOR_USER)
        .read(|conn| Ok(tod_store::node_timeline::TimelineRepo::new(conn).list(node)?))
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect()
}

#[test]
fn answering_an_option_records_the_shepherd() {
    let (root, node, _) = data_root();
    let short = ask(&root, node, "Which way?", &["left", "right"], &[]);
    let shepherd = env(&[ON, ("TOD_ACTOR", "shepherd:abc")]);
    let out = cli_with(&root, &shepherd, &["decisions", "answer", &short, "--option", "2"]).unwrap();
    assert!(out.starts_with(&format!("ok {short} answered \"right\"")), "{out}");
    assert_eq!(answer_actors(&root, &short), [(Some(2), None, "shepherd:abc".to_string())]);
    assert!(timeline_titles(&root, node).contains(&"shepherd abc answered a request".to_string()));
    let shown = cli_with(&root, &shepherd, &["decisions", "show", &short]).unwrap();
    assert!(shown.contains("answered"), "{shown}");
}

#[test]
fn answering_without_an_actor_records_the_operator() {
    let (root, node, _) = data_root();
    let short = ask(&root, node, "Which way?", &["left", "right"], &[]);
    cli_with(&root, env(&[ON]), &["decisions", "answer", &short, "--option", "1"]).unwrap();
    assert_eq!(answer_actors(&root, &short), [(Some(1), None, ACTOR_USER.to_string())]);
    assert!(timeline_titles(&root, node).contains(&"operator answered a request".to_string()));
}

#[test]
fn answering_free_text_records_it() {
    let (root, node, _) = data_root();
    let short = ask(&root, node, "What is the name?", &[], &[]);
    let out = cli_with(&root, env(&[ON, ("TOD_ACTOR", "shepherd:s2")]), &["decisions", "answer", &short, "--text", "Mabel"])
        .unwrap();
    assert!(out.contains("\"Mabel\""), "{out}");
    assert_eq!(answer_actors(&root, &short), [(None, Some("Mabel".to_string()), "shepherd:s2".to_string())]);
}

#[test]
fn an_answer_needs_an_option_or_text_and_a_valid_option() {
    let (root, node, _) = data_root();
    let short = ask(&root, node, "Which way?", &["left", "right"], &[]);
    let on = env(&[ON]);
    let err = cli_with(&root, &on, &["decisions", "answer", &short]).unwrap_err().to_string();
    assert!(err.contains("--option") && err.contains("--text"), "{err}");
    let err = cli_with(&root, &on, &["decisions", "answer", &short, "--option", "0"]).unwrap_err().to_string();
    assert!(err.contains("option number"), "{err}");
    let err = cli_with(&root, &on, &["decisions", "answer", &short, "--option", "7"]).unwrap_err().to_string();
    assert!(err.contains("no option 7"), "{err}");
    assert!(answer_actors(&root, &short).is_empty());
    let err = cli_with(&root, env(&[ON, ("TOD_ACTOR", "someone")]), &["decisions", "answer", &short, "--option", "1"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("TOD_ACTOR"), "{err}");
    assert!(answer_actors(&root, &short).is_empty());
}

#[test]
fn an_option_that_moves_the_node_back_moves_it() {
    let (root, node, _) = data_root();
    set_lifecycle(&root, node, "review");
    let short = ask(&root, node, "Rebuild it?", &["rebuild", "leave"], &["--moves-to", "1:active"]);
    let out = cli_with(&root, env(&[ON, ("TOD_ACTOR", "shepherd:abc")]), &["decisions", "answer", &short, "--option", "1"])
        .unwrap();
    assert!(out.contains("sent back to active"), "{out}");
    let state = InterviewClient::new(&root, ACTOR_USER)
        .read(|conn| Ok(tod_store::outline::repos::NodeRepo::new(conn).get_lifecycle(node)?.unwrap()))
        .unwrap();
    assert_eq!(state, "active");
    assert_eq!(answer_actors(&root, &short), [(Some(1), None, "shepherd:abc".to_string())]);
}

#[test]
fn runner_menu_of_a_local_node_without_the_daemon_says_so() {
    let (root, node, _) = data_root();
    set_lifecycle(&root, node, "active");
    let id = node.to_string();
    let err = cli_with(&root, env(&[ON]), &["runner", "menu", &id]).unwrap_err();
    assert!(format!("{err:#}").contains("tod-agentd is not running"), "{err:#}");
    let err = cli_with(&root, env(&[ON]), &["runner", "bogus", &id]).unwrap_err().to_string();
    assert!(err.contains("unknown command `bogus`"), "{err}");
}

/// The daemon, played.
#[derive(Default)]
struct FakeDaemon {
    states: Vec<RunnerState>,
    calls: RefCell<Vec<String>>,
}

impl Daemon for FakeDaemon {
    fn snapshot(&self) -> anyhow::Result<Vec<RunnerState>> {
        Ok(self.states.clone())
    }
    fn start(&self, node: Uuid, renew_budget: bool) -> anyhow::Result<()> {
        self.calls.borrow_mut().push(format!("start {node} renew={renew_budget}"));
        Ok(())
    }
    fn pause(&self, node: Uuid) -> anyhow::Result<()> {
        self.calls.borrow_mut().push(format!("pause {node}"));
        Ok(())
    }
    fn stop(&self, node: Uuid) -> anyhow::Result<()> {
        self.calls.borrow_mut().push(format!("stop {node}"));
        Ok(())
    }
}

fn dispatch(root: &PathBuf, daemon: &FakeDaemon, who: &Who, json: bool, command: &str, rest: &[&str]) -> anyhow::Result<String> {
    let fleet = FleetStore::open(root).unwrap();
    let ctx = Ctx { fleet: &fleet, root, daemon: Some(daemon) };
    let rest: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
    let inv = Invocation { data_root: root.clone(), json, rest: rest.clone() };
    let args = Args::parse(&rest).unwrap();
    let node = crate::node::resolve(&inv, &rest[0])?;
    crate::runner::dispatch(&inv, &ctx, who, command, &args, node)
}

#[test]
fn a_runner_verb_goes_to_the_daemon_and_the_menu_follows_it() {
    let (root, node, _) = data_root();
    set_lifecycle(&root, node, "active");
    let id = node.to_string();
    let daemon = FakeDaemon::default();

    let menu = dispatch(&root, &daemon, &Who::Operator, false, "menu", &[&id]).unwrap();
    assert!(menu.contains("lifecycle: active") && menu.contains("default: start (Start)"), "{menu}");
    let json: serde_json::Value =
        serde_json::from_str(&dispatch(&root, &daemon, &Who::Operator, true, "menu", &[&id]).unwrap()).unwrap();
    assert_eq!(json["default"]["verb"], "start");
    assert_eq!(json["run"], "not started");

    let out = dispatch(&root, &daemon, &Who::Shepherd("abc".into()), false, "start", &[&id]).unwrap();
    assert!(out.contains("Started"), "{out}");
    assert_eq!(daemon.calls.borrow().as_slice(), [format!("start {node} renew=false")]);
    assert!(timeline_titles(&root, node).contains(&"shepherd abc ran \"Start\"".to_string()));

    let json: serde_json::Value =
        serde_json::from_str(&dispatch(&root, &daemon, &Who::Operator, true, "run-fresh", &[&id]).unwrap()).unwrap();
    assert_eq!(json["verb"], "run-fresh");
    assert_eq!(json["failed"], false);

    // Not offered while it runs.
    let running = FakeDaemon {
        states: vec![RunnerState {
            node,
            running: true,
            pausing: false,
            since: 0,
            live: Default::default(),
            outcome: None,
        }],
        ..Default::default()
    };
    let err = dispatch(&root, &running, &Who::Operator, false, "start", &[&id]).unwrap_err().to_string();
    assert!(err.contains("not offered") && err.contains("pause"), "{err}");
    let err = dispatch(&root, &running, &Who::Operator, false, "migrate", &[&id, "--to", "x"]).unwrap_err().to_string();
    assert!(err.contains("run is active"), "{err}");
    let err = dispatch(&root, &running, &Who::Operator, false, "migrate", &[&id, "--to", "x", "--commit", "--discard"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("alternatives"), "{err}");
}

#[test]
fn stats_is_hidden_without_the_tier_refused_in_an_agent_session_and_reads_a_store() {
    let off = env(&[]);
    let (root, node, _) = data_root();
    let err = cli_with(&root, &off, &["stats"]).unwrap_err().to_string();
    assert!(err.contains("unknown noun `stats`"), "{err}");
    assert!(!help_with(&off, &["--help"]).unwrap().contains("stats"));

    let on = env(&[ON]);
    assert!(help_with(&on, &["--help"]).unwrap().contains("stats "));
    assert_eq!(help_with(&on, &["stats", "--help"]).unwrap(), crate::operator::STATS_USAGE.trim_end());

    let agent = env(&[ON, ("TOD_INTERVIEW_ACTOR", "conversation:abc")]);
    let err = cli_with(&root, &agent, &["stats"]).unwrap_err().to_string();
    assert!(err.contains("agent session") || err.contains("refused"), "{err}");

    let text = cli_with(&root, &on, &["stats"]).unwrap();
    for head in ["COST", "REVIEW LOOP", "PR LOOP", "REQUESTS", "STOPS", "SANDBOX CHURN"] {
        assert!(text.contains(head), "{head}: {text}");
    }
    let one = cli_with(&root, &on, &["stats", "--section", "requests"]).unwrap();
    assert!(one.starts_with("REQUESTS") && !one.contains("COST"), "{one}");
    let json = cli_with(&root, &on, &["--json", "stats", &node.to_string(), "--since", "7d", "--section", "stops"]).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&json).unwrap()["stops"].is_object(), "{json}");
    let err = cli_with(&root, &on, &["stats", "--section", "nope"]).unwrap_err().to_string();
    assert!(err.contains("unknown section `nope`"), "{err}");
}

#[test]
fn the_skill_doc_shows_stats_and_every_section() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.claude/skills/shepherd-common/operator-cli.md");
    let doc = std::fs::read_to_string(&path).unwrap();
    assert!(doc.contains("tod-cli stats [<NODE>] [--section <NAME>] [--since <DURATION>]"));
    for section in tod_core::shepherd_stats::Section::ALL {
        assert!(doc.contains(section.name()), "operator-cli.md never names the `{}` section", section.name());
        assert!(crate::operator::STATS_USAGE.contains(section.name()), "usage never names `{}`", section.name());
    }
}
