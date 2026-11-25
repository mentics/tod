//! The budget measures a runaway, not a node's age (shepherd gap 122):
//! ENG-3559 stopped in `pr_draft` with "budget spent: 60 sessions" after four
//! days of legitimate work across every state, about 14 of them in that
//! stay, and the 16 hours of work a run was allowed aged long-lived nodes out
//! the same way. Sessions and time count per stay in a lifecycle state,
//! against ceilings for the whole run that only a cycle across states
//! reaches.

use super::*;

/// The sessions a whole lifecycle spends in its busiest stay, and in all.
fn sessions_of_a_lifecycle() -> (u32, u32) {
    let fx = setup();
    retire_outside_criteria(&fx);
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget { max_sessions: 1000, max_run_sessions: 1000, ..Budget::default() });
    assert_eq!(pilot.run(&fx.fleet, &mut agent).unwrap(), Outcome::Done);
    let state = pilot.state();
    let busiest = state.session_counts.by_state.values().copied().max().unwrap();
    assert_eq!(state.session_counts.by_state.values().sum::<u32>(), state.sessions);
    (busiest, state.sessions)
}

#[test]
fn a_long_lived_node_that_moves_through_states_never_stops_on_its_run_total() {
    let (busiest, total) = sessions_of_a_lifecycle();
    // A budget the whole run outspends, but no one stay does (a stay is
    // checked before its next step, so it may hold one less than this).
    let per_stay = busiest + 1;
    assert!(total > per_stay, "the lifecycle spends {total} sessions, its busiest state {busiest}");

    let fx = setup();
    retire_outside_criteria(&fx);
    // Days of earlier work already spent, in states the node has left, as
    // ENG-3559 had when it reached `pr_draft`.
    let mut earlier = AutopilotState::fresh();
    for (state, kind, n) in [("review", "review", 21), ("review", "fix", 6), ("verifying", "evaluate", 7), ("design", "phase", 3)] {
        for _ in 0..n {
            earlier.sessions += 1;
            earlier.session_counts.note(state, kind, 0);
        }
    }
    earlier.save(&fx.root, fx.node).unwrap();
    let budget = Budget { max_sessions: per_stay, max_run_sessions: RUN_SESSIONS, ..Budget::default() };
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, budget);
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert!(
        pilot.state().sessions >= 37 + total && pilot.state().sessions > budget.max_sessions,
        "the run's total ({}) is past the per-stay budget and never stopped it",
        pilot.state().sessions
    );
}

#[test]
fn a_runaway_within_one_stay_still_stops_and_says_what_its_sessions_were() {
    let fx = setup();
    retire_outside_criteria(&fx);
    // A stay that already spent the budget in the node's own state.
    let here = lifecycle::current_state(&fx.fleet, fx.node).unwrap();
    let mut state = AutopilotState::fresh();
    for (kind, n) in [("phase", 25), ("evaluate", 15)] {
        for _ in 0..n {
            state.sessions += 1;
            state.session_counts.note(&here, kind, 0);
        }
    }
    state.save(&fx.root, fx.node).unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::BudgetExhausted { limit } = &outcome else { panic!("{outcome:?}") };
    let BudgetLimit::Sessions { used, stay, breakdown, .. } = limit else { panic!("{limit:?}") };
    assert_eq!((*used, stay.as_deref()), (STAY_SESSIONS, Some(here.as_str())));
    assert_eq!(breakdown, &vec![("phase".to_string(), 25), ("evaluate".to_string(), 15)]);
    assert_eq!(agent.turns, 0, "nothing more was spent");
    assert_eq!(
        crate::run_state::budget_text(limit),
        format!("budget spent: 40 sessions in {here} (phase 25, evaluate 15)")
    );

    // A real run that spends its stay's budget stops in that stay too.
    let fx = setup();
    retire_outside_criteria(&fx);
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget { max_sessions: 1, ..Budget::default() }).run(&fx.fleet, &mut agent).unwrap();
    assert!(
        matches!(&outcome, Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 1, stay: Some(_), breakdown, .. } } if breakdown.len() == 1),
        "{outcome:?}"
    );
}

#[test]
fn a_move_to_another_state_starts_the_stay_again_and_the_run_ceiling_catches_a_cycle() {
    let fx = bare();
    let budget = Budget { max_sessions: 4, max_run_sessions: 10, ..Budget::default() };
    let mut pilot = autopilot(&fx, budget);
    for _ in 0..3 {
        pilot.note_session("review", "review");
    }
    pilot.note_session("review", "fix");
    let spent = pilot.over_budget_in("review").expect("the stay spent its budget");
    assert_eq!(crate::run_state::budget_text(&spent), "budget spent: 4 sessions in review (review 3, fix 1)");
    // Moved on: the stay in `pr` has spent nothing.
    assert_eq!(pilot.over_budget_in("pr"), None);
    // Back and forth between two states, never four in one stay: only the
    // run's ceiling stops it, and says where the sessions went.
    for state in ["verifying", "review", "verifying", "review", "verifying", "review"] {
        assert_eq!(pilot.over_budget_in(state), None, "{state}");
        pilot.note_session(state, "review");
    }
    let Some(limit @ BudgetLimit::Sessions { used: 10, stay: None, .. }) = pilot.over_budget_in("review") else {
        panic!("{:?}", pilot.over_budget_in("review"))
    };
    assert_eq!(
        crate::run_state::budget_text(&limit),
        "budget spent: 10 sessions over the run (review 7, verifying 3)"
    );
    // A fresh budget starts every count again.
    pilot.renew_budget().unwrap();
    assert_eq!(pilot.state().session_counts, Default::default());
    assert_eq!(pilot.over_budget_in("review"), None);
}

#[test]
fn redoing_a_phase_starts_its_stay_again_and_keeps_the_run_count() {
    let fx = bare();
    let mut state = AutopilotState::fresh();
    for _ in 0..5 {
        state.sessions += 1;
        state.session_counts.note("design", "phase", 0);
    }
    state.save(&fx.root, fx.node).unwrap();
    AutopilotState::request_rerun_phase(&fx.root, fx.node).unwrap();
    let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
    assert_eq!(saved.session_counts.in_stay("design"), 0);
    assert_eq!(saved.sessions, 5);
    assert_eq!(saved.session_counts.by_state.get("design"), Some(&5));
}

#[test]
fn a_state_saved_before_stays_were_counted_loads_and_a_stop_saved_then_reads_as_before() {
    let old: AutopilotState =
        serde_json::from_str(r#"{"started_at_ms":1,"sessions":55,"current":null,"steps":[],"outcome":null}"#).unwrap();
    assert_eq!(old.session_counts, SessionCounts::default());
    assert_eq!(old.session_counts.in_stay("pr_draft"), 0);
    let limit: BudgetLimit = serde_json::from_str(r#"{"kind":"sessions","used":60,"elapsed_secs":3}"#).unwrap();
    assert_eq!(crate::run_state::budget_text(&limit), "budget spent: 60 sessions");
}

/// Gap 123: a budget stop is a verdict reached under the limits of its time.
/// ENG-3038 stopped with "budget spent: 61 sessions" under the old whole-run
/// count, and after gap 122 its saved counts are within today's budget: the
/// stop no longer holds it. Judged from the saved counts, never the stop.
#[test]
fn a_saved_budget_stop_is_judged_again_against_todays_budget() {
    // Saved before stays were counted: 61 sessions, no per-stay counts.
    let old: AutopilotState = serde_json::from_str(
        r#"{"started_at_ms":1,"active_ms":12871588,"sessions":61,"current":null,"steps":[],
            "outcome":{"kind":"budget_exhausted","limit":{"kind":"sessions","used":61,"elapsed_secs":12871}}}"#,
    )
    .unwrap();
    // Held to the run's ceilings alone (160 sessions, 64 hours): within them.
    assert_eq!(old.budget_spent_in("review", &Budget::default()), None);
    assert!(old.budget_stop_outgrown("review", &Budget::default()));
    // Under the limits it stopped with, the stop still holds.
    let then = Budget { max_run_sessions: 60, ..Budget::default() };
    assert!(!old.budget_stop_outgrown("review", &then));
    // So does one past today's ceiling.
    let mut past = old.clone();
    past.sessions = RUN_SESSIONS;
    assert!(!past.budget_stop_outgrown("review", &Budget::default()));

    // A stay that truly spent today's per-stay budget still holds, in that
    // state; a move to another state starts a stay with nothing spent.
    let mut stay = AutopilotState::fresh();
    for _ in 0..STAY_SESSIONS {
        stay.sessions += 1;
        stay.session_counts.note("review", "review", 0);
    }
    stay.outcome = Some(Outcome::BudgetExhausted {
        limit: BudgetLimit::Sessions { used: STAY_SESSIONS, elapsed_secs: 0, stay: Some("review".into()), breakdown: Vec::new() },
    });
    assert!(matches!(
        stay.budget_spent_in("review", &Budget::default()),
        Some(BudgetLimit::Sessions { used: STAY_SESSIONS, stay: Some(_), .. })
    ));
    assert!(!stay.budget_stop_outgrown("review", &Budget::default()));
    assert!(stay.budget_stop_outgrown("design", &Budget::default()));
    // Raised limits outgrow it, as a deploy or a setting may raise them.
    let raised = Budget { max_sessions: 2 * STAY_SESSIONS, ..Budget::default() };
    assert!(stay.budget_stop_outgrown("review", &raised));

    // Only a budget stop is judged: any other end is not "outgrown".
    stay.outcome = Some(Outcome::Stopped { reason: "x".into() });
    assert!(!stay.budget_stop_outgrown("design", &Budget::default()));
}

/// A `pr_draft` stay lasts as long as people take to review: once the pull
/// request rests with them, the next wave of feedback starts the stay's count
/// again (ENG-3559 spent days in `pr_draft`, a few sessions a wave).
#[test]
fn a_pull_request_at_rest_with_a_person_starts_the_stay_again() {
    let fx = bare();
    let budget = Budget { max_sessions: 3, max_run_sessions: 100, ..Budget::default() };
    let mut pilot = autopilot(&fx, budget);
    for kind in ["review", "quick_fix", "small_review"] {
        pilot.note_session("pr_draft", kind);
    }
    assert!(pilot.over_budget_in("pr_draft").is_some());
    pilot.pr_at_rest().unwrap();
    assert_eq!(pilot.over_budget_in("pr_draft"), None);
    let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
    assert_eq!((saved.sessions, saved.session_counts.in_stay("pr_draft")), (3, 0));
    assert_eq!(saved.session_counts.by_state.get("pr_draft"), Some(&3));
}

const HOUR_MS: u64 = 60 * 60 * 1000;

/// A run that has worked `hours` so far, not working now.
fn worked(pilot: &mut Autopilot, hours: u64) {
    pilot.state.active_since_ms = None;
    pilot.state.active_ms = hours * HOUR_MS;
}

/// The regression: a node that has worked more than the 16 hours a stay may
/// over several stays goes on, where the run's total used to stop it.
#[test]
fn a_node_with_more_than_sixteen_hours_of_work_over_several_stays_does_not_stop() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let here = lifecycle::current_state(&fx.fleet, fx.node).unwrap();
    // Twenty hours of earlier work, the last five of them in a stay in
    // `review`, a state the node has since left.
    let mut earlier = AutopilotState::fresh();
    earlier.active_ms = 20 * HOUR_MS;
    earlier.sessions = 30;
    for _ in 0..30 {
        earlier.session_counts.note("review", "review", 15 * HOUR_MS);
    }
    assert_ne!(here, "review");
    earlier.save(&fx.root, fx.node).unwrap();
    assert!(earlier.elapsed() > Budget::default().max_duration);
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    assert_eq!(pilot.run(&fx.fleet, &mut agent).unwrap(), Outcome::Done);
    assert!(pilot.state().elapsed() >= Duration::from_millis(20 * HOUR_MS));

    // Step by step: four stays of ten hours each, forty in all.
    let fx = bare();
    let mut pilot = autopilot(&fx, Budget::default());
    for (n, state) in ["design", "active", "review", "pr_draft"].into_iter().enumerate() {
        worked(&mut pilot, 10 * n as u64);
        assert_eq!(pilot.over_budget_in(state), None, "entering {state}");
        pilot.note_session(state, "phase");
        worked(&mut pilot, 10 * n as u64 + 10);
        assert_eq!(pilot.over_budget_in(state), None, "after ten hours in {state}");
    }
    assert_eq!(pilot.state().elapsed(), Duration::from_millis(40 * HOUR_MS));
}

#[test]
fn sixteen_hours_in_one_stay_stops_and_says_which_stay() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let here = lifecycle::current_state(&fx.fleet, fx.node).unwrap();
    // A stay in the node's own state that began at hour 2 and has worked 16.
    let mut state = AutopilotState::fresh();
    state.active_ms = 18 * HOUR_MS;
    state.sessions = 3;
    for _ in 0..3 {
        state.session_counts.note(&here, "phase", 2 * HOUR_MS);
    }
    state.save(&fx.root, fx.node).unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::BudgetExhausted { limit } = &outcome else { panic!("{outcome:?}") };
    assert!(
        matches!(limit, BudgetLimit::Time { used: 3, stay: Some(stay), elapsed_secs } if *stay == here && *elapsed_secs >= 16 * 3600),
        "{limit:?}"
    );
    assert_eq!(agent.turns, 0, "nothing more was spent");
    assert_eq!(crate::run_state::budget_text(limit), format!("budget spent: 16h of work in {here}"));
}

#[test]
fn the_run_ceiling_on_time_catches_a_cycle_across_states() {
    let fx = bare();
    let budget = Budget {
        max_duration: Duration::from_millis(10 * HOUR_MS),
        max_run_duration: Duration::from_millis(25 * HOUR_MS),
        ..Budget::default()
    };
    let mut pilot = autopilot(&fx, budget);
    let mut hours = 0;
    for state in ["verifying", "review", "verifying"] {
        worked(&mut pilot, hours);
        assert_eq!(pilot.over_budget_in(state), None, "{state} at {hours}h");
        hours += 9;
    }
    worked(&mut pilot, hours);
    let limit = pilot.over_budget_in("review").expect("the run's ceiling");
    assert_eq!(limit, BudgetLimit::Time { elapsed_secs: 27 * 3600, used: 0, stay: None });
    assert_eq!(crate::run_state::budget_text(&limit), "budget spent: 1d 3h of work over the run");
    // Eleven hours in one stay is the stay's limit.
    let fx = bare();
    let mut pilot = autopilot(&fx, budget);
    worked(&mut pilot, 0);
    assert_eq!(pilot.over_budget_in("review"), None);
    worked(&mut pilot, 11);
    let limit = pilot.over_budget_in("review");
    assert!(
        matches!(&limit, Some(BudgetLimit::Time { stay: Some(s), elapsed_secs, .. }) if s == "review" && *elapsed_secs == 11 * 3600),
        "{limit:?}"
    );
}

/// Every point that starts the stay's sessions again starts its time again
/// too: the pull request at rest, the user going on past a limit, redoing
/// the phase (and a move, above).
#[test]
fn the_stay_time_restarts_where_its_sessions_do() {
    let budget = Budget { max_duration: Duration::from_millis(10 * HOUR_MS), ..Budget::default() };

    let fx = bare();
    let mut pilot = autopilot(&fx, budget);
    worked(&mut pilot, 1);
    assert_eq!(pilot.over_budget_in("pr_draft"), None);
    worked(&mut pilot, 12);
    assert!(pilot.over_budget_in("pr_draft").is_some());
    pilot.pr_at_rest().unwrap();
    assert_eq!(pilot.over_budget_in("pr_draft"), None, "at rest");
    worked(&mut pilot, 21);
    assert_eq!(pilot.over_budget_in("pr_draft"), None, "nine hours since it rested");
    let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
    assert_eq!(saved.session_counts.stay_since_ms, 12 * HOUR_MS);

    let fx = bare();
    let mut pilot = autopilot(&fx, budget);
    worked(&mut pilot, 0);
    assert_eq!(pilot.over_budget_in("review"), None);
    worked(&mut pilot, 12);
    assert!(pilot.over_budget_in("review").is_some());
    pilot.state.outcome =
        Some(Outcome::NeedsHuman { reason: NeedsHuman::ReviewNotConverging { rounds: 5, standing: vec!["x".into()] } });
    pilot.user_go_on().unwrap();
    assert_eq!(pilot.over_budget_in("review"), None, "the user went on");

    let fx = bare();
    let mut state = AutopilotState::fresh();
    state.active_ms = 12 * HOUR_MS;
    state.session_counts.enter("design", 0);
    state.save(&fx.root, fx.node).unwrap();
    AutopilotState::request_rerun_phase(&fx.root, fx.node).unwrap();
    let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
    assert_eq!(saved.session_counts.stay_worked_ms("design", saved.worked_ms()), 0, "redone");
    assert_eq!(saved.elapsed(), Duration::from_millis(12 * HOUR_MS), "the run's time is kept");
}
