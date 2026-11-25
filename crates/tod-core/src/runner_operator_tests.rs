//! The operator verbs against a test store and a played daemon.

use super::*;
use crate::autopilot::local::{Live, RunnerState};
use crate::autopilot::{BudgetLimit, Outcome};
use crate::interview::test_support::{Fixture, fixture};
use std::cell::RefCell;
use tod_store::decisions::{DecisionRepo, NewDecision, OptionMove};
use tod_store::interview::InterviewCommand;
use tod_store::node_timeline::TimelineRepo;

/// The daemon, played: the states it reports, and what it was asked.
#[derive(Default)]
struct FakeDaemon {
    states: RefCell<Vec<RunnerState>>,
    calls: RefCell<Vec<String>>,
}

impl FakeDaemon {
    fn with(node: Uuid, running: bool, pausing: bool, outcome: Option<Outcome>) -> Self {
        let fake = Self::default();
        fake.states.borrow_mut().push(RunnerState { node, running, pausing, since: 0, live: Live::default(), outcome });
        fake
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Daemon for FakeDaemon {
    fn snapshot(&self) -> Result<Vec<RunnerState>> {
        Ok(self.states.borrow().clone())
    }
    fn start(&self, node: Uuid, renew_budget: bool) -> Result<()> {
        self.calls.borrow_mut().push(format!("start {node} renew={renew_budget}"));
        Ok(())
    }
    fn pause(&self, node: Uuid) -> Result<()> {
        self.calls.borrow_mut().push(format!("pause {node}"));
        Ok(())
    }
    fn stop(&self, node: Uuid) -> Result<()> {
        self.calls.borrow_mut().push(format!("stop {node}"));
        Ok(())
    }
}

fn ctx<'a>(fx: &'a Fixture, daemon: Option<&'a dyn Daemon>) -> Ctx<'a> {
    Ctx { fleet: &fx.fleet, root: &fx.root, daemon }
}

fn menu(fx: &Fixture, daemon: &FakeDaemon) -> MenuReport {
    menu_for(&ctx(fx, Some(daemon)), fx.node).unwrap()
}

fn verbs(entries: &[MenuEntry]) -> Vec<&'static str> {
    entries.iter().map(|e| e.verb).collect()
}

fn notes(fx: &Fixture) -> Vec<TimelineEntry> {
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().ok();
    fx.fleet
        .read(|conn| TimelineRepo::new(conn).list(fx.node))
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == KIND_NOTE)
        .collect()
}

fn go(fx: &Fixture, daemon: &FakeDaemon, verb: Verb, opts: &Options, who: &Who) -> Result<Done> {
    perform(&ctx(fx, Some(daemon)), fx.node, verb, opts, who, &mut |_| {})
}

fn active(fx: &Fixture) {
    crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "active").unwrap();
}

#[test]
fn every_menu_entry_has_a_verb_and_back() {
    for verb in Verb::ALL {
        assert_eq!(Verb::of(verb.action()), verb);
        assert_eq!(Verb::parse(verb.name()), Some(verb));
    }
    let names: std::collections::HashSet<_> = Verb::ALL.iter().map(|v| v.name()).collect();
    assert_eq!(names.len(), Verb::ALL.len());
}

#[test]
fn who_comes_from_the_environment() {
    let env = |v: Option<&'static str>| move |n: &str| (n == ACTOR_ENV).then(|| v.map(str::to_string)).flatten();
    assert_eq!(Who::from_env(env(None)).unwrap(), Who::Operator);
    assert_eq!(Who::from_env(env(Some("  "))).unwrap(), Who::Operator);
    let shepherd = Who::from_env(env(Some("shepherd:abc"))).unwrap();
    assert_eq!(shepherd, Who::Shepherd("abc".into()));
    assert_eq!(shepherd.actor(), "shepherd:abc");
    assert_eq!(Who::Operator.actor(), ACTOR_USER);
    assert!(Who::from_env(env(Some("shepherd:"))).is_err());
    assert!(Who::from_env(env(Some("someone"))).is_err());
}

#[test]
fn the_operator_tier_is_on_only_for_shepherd() {
    let env = |v: &'static str| move |n: &str| (n == OPERATOR_ENV).then(|| v.to_string());
    assert!(enabled(env("shepherd")));
    assert!(!enabled(env("yes")));
    assert!(!enabled(|_| None));
}

#[test]
fn the_menu_of_a_node_never_run_offers_start() {
    let fx = fixture();
    active(&fx);
    let report = menu(&fx, &FakeDaemon::default());
    assert_eq!(report.lifecycle, "active");
    assert_eq!(report.default.as_ref().map(|e| e.verb), Some("start"), "{report:?}");
    let entries = verbs(&report.entries);
    for verb in ["run-fresh", "check-now", "rerun-phase", "migrate"] {
        assert!(entries.contains(&verb), "{entries:?}");
    }
    assert!(!entries.contains(&"start") && !entries.contains(&"pause"), "{entries:?}");
    assert_eq!(report.run, "not started");
    assert!(report.text().contains("default: start (Start)"), "{}", report.text());
}

#[test]
fn the_menu_of_a_running_node_offers_pause_and_stop_now() {
    let fx = fixture();
    active(&fx);
    let report = menu(&fx, &FakeDaemon::with(fx.node, true, false, None));
    assert_eq!(report.run, "running");
    assert_eq!(report.default.as_ref().map(|e| e.verb), Some("pause"));
    let entries = verbs(&report.entries);
    assert!(entries.contains(&"stop-now"), "{entries:?}");
    assert!(!entries.contains(&"check-now"), "a running node is not checked: {report:?}");
    let pausing = menu(&fx, &FakeDaemon::with(fx.node, true, true, None));
    assert_eq!(pausing.run, "running, pausing");
}

#[test]
fn the_menu_after_a_spent_budget_offers_a_fresh_one_and_after_a_stop_resume() {
    let fx = fixture();
    active(&fx);
    let budget = Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 3, elapsed_secs: 10, stay: None, breakdown: Vec::new() } };
    let report = menu(&fx, &FakeDaemon::with(fx.node, false, false, Some(budget)));
    assert_eq!(report.default.as_ref().map(|e| e.verb), Some("resume-fresh-budget"), "{report:?}");
    assert!(verbs(&report.entries).contains(&"run-fresh"));

    let stopped = Outcome::Stopped { reason: "paused".into() };
    let report = menu(&fx, &FakeDaemon::with(fx.node, false, false, Some(stopped)));
    assert_eq!(report.default.as_ref().map(|e| e.verb), Some("resume"), "{report:?}");
    assert_eq!(report.run, "stopped");
    let entries = verbs(&report.entries);
    assert!(entries.contains(&"resume-fresh-budget") && entries.contains(&"run-fresh"), "{entries:?}");
}

/// The run stopped on its budget, saved as the daemon saves it: the
/// autopilot's state and the run's row.
fn stopped_on_budget(fx: &Fixture, state: crate::autopilot::AutopilotState) -> Outcome {
    let outcome = state.outcome.clone().expect("a budget stop");
    state.save(&fx.root, fx.node).unwrap();
    crate::run_state::record(
        &fx.fleet,
        fx.node,
        crate::run_state::for_outcome(&outcome, crate::run_state::Words::local()),
        crate::run_state::LOCAL_WRITER,
    )
    .unwrap();
    outcome
}

/// Gap 123: ENG-3038 was stopped with "budget spent: 61 sessions" under the
/// whole-run count before gap 122; after the deploy its saved counts are
/// within today's budget, yet `runner resume` was refused and the menu
/// offered only a fresh budget, trusting the old stop. The stop is judged
/// again: an old one within today's budget is an ordinary stop.
#[test]
fn an_old_budget_stop_within_todays_budget_is_plainly_resumed() {
    let fx = fixture();
    active(&fx);
    // Saved before stays were counted: no per-stay counts at all.
    let old: crate::autopilot::AutopilotState = serde_json::from_str(
        r#"{"started_at_ms":1,"active_ms":12871588,"sessions":61,"current":null,"steps":[],
            "outcome":{"kind":"budget_exhausted","limit":{"kind":"sessions","used":61,"elapsed_secs":12871}}}"#,
    )
    .unwrap();
    let outcome = stopped_on_budget(&fx, old);
    let daemon = FakeDaemon::with(fx.node, false, false, Some(outcome));
    let report = menu(&fx, &daemon);
    assert_eq!(report.default.as_ref().map(|e| e.verb), Some("resume"), "{report:?}");
    assert_eq!(report.run, "stopped");
    assert!(verbs(&report.entries).contains(&"resume-fresh-budget"), "{report:?}");
    // Who is next says it is an ordinary stop now, and why.
    let standing = crate::next_actor::compute(&fx.fleet, &fx.root).unwrap().remove(&fx.node).unwrap();
    let NextActor::You { why: crate::next_actor::YouReason::Stopped { reason }, .. } = &standing.actor else {
        panic!("{standing:?}")
    };
    assert!(reason.starts_with("budget spent: 61 sessions") && reason.contains(crate::next_actor::BUDGET_OUTGROWN), "{reason}");

    go(&fx, &daemon, Verb::Resume, &Options::default(), &Who::Shepherd("s1".into())).unwrap();
    assert_eq!(daemon.calls(), [format!("start {} renew=false", fx.node)]);
    // The stop itself is left as it was saved.
    let saved = crate::autopilot::AutopilotState::load(&fx.root, fx.node).unwrap();
    assert!(matches!(saved.outcome, Some(Outcome::BudgetExhausted { .. })));
}

/// A stay that truly spent today's per-stay budget still offers only a fresh
/// budget, and a plain Resume is refused.
#[test]
fn a_budget_stop_still_over_todays_budget_offers_only_a_fresh_one() {
    let fx = fixture();
    active(&fx);
    let mut state = crate::autopilot::AutopilotState::fresh();
    for _ in 0..crate::autopilot::STAY_SESSIONS {
        state.sessions += 1;
        state.session_counts.note("active", "implement", 0);
    }
    state.outcome = Some(Outcome::BudgetExhausted {
        limit: BudgetLimit::Sessions {
            used: crate::autopilot::STAY_SESSIONS,
            elapsed_secs: 0,
            stay: Some("active".into()),
            breakdown: state.session_counts.stay_breakdown(),
        },
    });
    let outcome = stopped_on_budget(&fx, state);
    let daemon = FakeDaemon::with(fx.node, false, false, Some(outcome));
    let report = menu(&fx, &daemon);
    assert_eq!(report.default.as_ref().map(|e| e.verb), Some("resume-fresh-budget"), "{report:?}");
    assert!(!verbs(&report.entries).contains(&"resume"), "{report:?}");
    let err = go(&fx, &daemon, Verb::Resume, &Options::default(), &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("is not offered"), "{err}");
    assert!(daemon.calls().is_empty());
}

#[test]
fn the_menu_of_a_done_node_has_no_default() {
    let fx = fixture();
    crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "done").unwrap();
    let report = menu(&fx, &FakeDaemon::default());
    assert_eq!(report.run, "done");
    assert!(report.default.is_none(), "{report:?}");
    let entries = verbs(&report.entries);
    assert!(!entries.contains(&"migrate") && !entries.contains(&"check-now"), "{entries:?}");
    assert!(report.text().contains("default: none"));
}

#[test]
fn a_local_node_without_the_daemon_is_an_error_that_says_so() {
    let fx = fixture();
    active(&fx);
    let err = menu_for(&ctx(&fx, None), fx.node).unwrap_err().to_string();
    assert!(err.contains("tod-agentd is not running"), "{err}");
    let err = perform(&ctx(&fx, None), fx.node, Verb::Start, &Options::default(), &Who::Operator, &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(err.contains("tod-agentd is not running"), "{err}");
    assert!(notes(&fx).is_empty(), "nothing was done, so nothing is noted");
}

#[test]
fn start_goes_to_the_daemon_and_is_noted_as_the_operator() {
    let fx = fixture();
    active(&fx);
    let daemon = FakeDaemon::default();
    let done = go(&fx, &daemon, Verb::Start, &Options::default(), &Who::Operator).unwrap();
    assert!(done.message.contains("Started"), "{done:?}");
    assert_eq!(daemon.calls(), [format!("start {} renew=false", fx.node)]);
    let notes = notes(&fx);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].title, "operator ran \"Start\"");
    assert!(notes[0].by_user && !notes[0].failed);
    assert_eq!(notes[0].phase, "active");
}

#[test]
fn a_shepherd_is_named_on_the_note_and_the_budget_verb_renews() {
    let fx = fixture();
    active(&fx);
    let budget = Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 3, elapsed_secs: 10, stay: None, breakdown: Vec::new() } };
    let daemon = FakeDaemon::with(fx.node, false, false, Some(budget));
    go(&fx, &daemon, Verb::ResumeFreshBudget, &Options::default(), &Who::Shepherd("s1".into())).unwrap();
    assert_eq!(daemon.calls(), [format!("start {} renew=true", fx.node)]);
    assert_eq!(notes(&fx)[0].title, "shepherd s1 ran \"Resume with a fresh budget\"");
}

#[test]
fn run_fresh_asks_for_a_new_session_then_starts() {
    let fx = fixture();
    active(&fx);
    let daemon = FakeDaemon::default();
    go(&fx, &daemon, Verb::RunFresh, &Options::default(), &Who::Operator).unwrap();
    assert!(crate::autopilot::AutopilotState::load(&fx.root, fx.node).unwrap().fresh_session);
    assert_eq!(daemon.calls(), [format!("start {} renew=false", fx.node)]);
}

#[test]
fn preparing_a_phase_rerun_clears_local_run_state_but_not_for_a_supervisor() {
    let fx = fixture();
    active(&fx);
    let mut state = crate::autopilot::AutopilotState::fresh();
    state.outcome = Some(Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 3, elapsed_secs: 10, stay: None, breakdown: Vec::new() } });
    state.save(&fx.root, fx.node).unwrap();

    prepare_fresh_run(&fx.fleet, &fx.root, fx.node, true, FreshRun::Phase).unwrap();
    let state = crate::autopilot::AutopilotState::load(&fx.root, fx.node).unwrap();
    assert!(!state.fresh_session);
    assert!(state.outcome.is_some());

    prepare_fresh_run(&fx.fleet, &fx.root, fx.node, false, FreshRun::Phase).unwrap();
    let state = crate::autopilot::AutopilotState::load(&fx.root, fx.node).unwrap();
    assert!(state.fresh_session);
    assert!(state.outcome.is_none());
}

#[test]
fn a_verb_the_menu_does_not_offer_is_refused_naming_what_it_does() {
    let fx = fixture();
    active(&fx);
    let daemon = FakeDaemon::default();
    let err = go(&fx, &daemon, Verb::Pause, &Options::default(), &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("not offered") && err.contains("start"), "{err}");
    assert!(daemon.calls().is_empty());
    // And a running node is not started again.
    let running = FakeDaemon::with(fx.node, true, false, None);
    let err = go(&fx, &running, Verb::Start, &Options::default(), &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("not offered") && err.contains("pause"), "{err}");
    assert!(running.calls().is_empty());
}

#[test]
fn pause_and_stop_now_go_to_the_daemon() {
    let fx = fixture();
    active(&fx);
    let daemon = FakeDaemon::with(fx.node, true, false, None);
    go(&fx, &daemon, Verb::Pause, &Options::default(), &Who::Operator).unwrap();
    go(&fx, &daemon, Verb::StopNow, &Options::default(), &Who::Operator).unwrap();
    assert_eq!(daemon.calls(), [format!("pause {}", fx.node), format!("stop {}", fx.node)]);
    assert_eq!(notes(&fx).len(), 2);
}

#[test]
fn migrate_needs_a_location_and_names_the_ones_there_are() {
    let fx = fixture();
    active(&fx);
    let daemon = FakeDaemon::default();
    let err = go(&fx, &daemon, Verb::Migrate, &Options::default(), &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("--to"), "{err}");
    let opts = Options { location: Some("nowhere".into()), ..Default::default() };
    let err = go(&fx, &daemon, Verb::Migrate, &opts, &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("no location is named `nowhere`"), "{err}");
    // A failed attempt is noted as failed.
    let notes = notes(&fx);
    assert!(!notes.is_empty() && notes.iter().all(|n| n.failed), "{notes:?}");
}

#[test]
fn migrate_refuses_while_the_run_is_active() {
    let fx = fixture();
    active(&fx);
    let daemon = FakeDaemon::with(fx.node, true, false, None);
    let opts = Options { location: Some("x".into()), ..Default::default() };
    let err = go(&fx, &daemon, Verb::Migrate, &opts, &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("run is active"), "{err}");
}

fn ask_two_options(fx: &Fixture, moves: Vec<OptionMove>) -> Uuid {
    let value = fx
        .fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: None,
                decision: NewDecision {
                    question: "Which way?".into(),
                    options: vec!["Left".into(), "Right".into()],
                    moves,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    Uuid::parse_str(value["id"].as_str().unwrap()).unwrap()
}

fn answer_actors(fx: &Fixture, id: Uuid) -> Vec<String> {
    fx.fleet.reload_if_stale().ok();
    fx.fleet
        .read(|conn| DecisionRepo::new(conn).get_with_answers(id))
        .unwrap()
        .unwrap()
        .answers
        .into_iter()
        .map(|a| a.actor)
        .collect()
}

#[test]
fn an_answer_is_recorded_as_the_shepherd_or_the_operator() {
    let fx = fixture();
    let a = ask_two_options(&fx, vec![]);
    let done = answer(&ctx(&fx, None), a, Some(2), None, &Who::Shepherd("s1".into())).unwrap();
    assert_eq!(done.answered, "\"Right\"");
    assert_eq!(answer_actors(&fx, a), ["shepherd:s1"]);
    let b = ask_two_options(&fx, vec![]);
    answer(&ctx(&fx, None), b, None, Some("neither".into()), &Who::Operator).unwrap();
    assert_eq!(answer_actors(&fx, b), [ACTOR_USER]);
    let titles: Vec<String> = notes(&fx).into_iter().map(|n| n.title).collect();
    assert!(titles.contains(&"shepherd s1 answered a request".to_string()), "{titles:?}");
    assert!(titles.contains(&"operator answered a request".to_string()), "{titles:?}");
}

#[test]
fn answering_an_answered_request_is_a_reported_change_not_a_second_first_answer() {
    let fx = fixture();
    let a = ask_two_options(&fx, vec![]);
    let first = answer(&ctx(&fx, None), a, Some(1), None, &Who::Operator).unwrap();
    assert_eq!(first.changed_from, None);
    let second = answer(&ctx(&fx, None), a, Some(2), None, &Who::Shepherd("s1".into())).unwrap();
    assert_eq!(second.changed_from.as_deref(), Some("\"Left\""));
    assert_eq!(answer_actors(&fx, a), [ACTOR_USER, "shepherd:s1"]);
}

#[test]
fn an_answer_that_moves_the_node_back_moves_it() {
    let fx = fixture();
    crate::lifecycle::set_lifecycle(&fx.fleet, fx.node, "review").unwrap();
    let id = ask_two_options(&fx, vec![OptionMove { option: 1, to: "active".into(), asked_in: None }]);
    let done = answer(&ctx(&fx, None), id, Some(1), None, &Who::Shepherd("s1".into())).unwrap();
    assert_eq!(done.moved_to.as_deref(), Some("active"));
    fx.fleet.reload_if_stale().ok();
    assert_eq!(crate::lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "active");
    assert_eq!(answer_actors(&fx, id), ["shepherd:s1"]);
}

#[test]
fn an_invalid_answer_is_an_error_and_noted_as_failed() {
    let fx = fixture();
    let id = ask_two_options(&fx, vec![]);
    let err = answer(&ctx(&fx, None), id, Some(9), None, &Who::Operator).unwrap_err().to_string();
    assert!(err.contains("no option 9"), "{err}");
    assert!(answer_actors(&fx, id).is_empty());
    assert!(notes(&fx).iter().any(|n| n.failed), "{:?}", notes(&fx));
}

#[test]
fn opening_a_session_pins_the_data_root_for_verbs_that_discover_paths() {
    // `start` on a node with no files yet calls `TodPaths::discover()`, which
    // `tod-cli` never pinned: "data root is not configured".
    let dir = tod_test_util::scratch_dir("operator-session-root");
    tod_store::paths::clear_data_root_override();
    let _session = Session::open(&dir).expect("open a session");
    let paths = tod_store::TodPaths::discover().expect("the root is pinned by opening a session");
    assert_eq!(paths.data_root(), dir.as_path());
    tod_store::paths::clear_data_root_override();
}
