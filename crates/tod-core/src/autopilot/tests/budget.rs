//! The session budget measures a runaway, not a node's age (shepherd gap
//! 122): ENG-3559 stopped in `pr_draft` with "budget spent: 60 sessions"
//! after four days of legitimate work across every state, about 14 of them
//! in that stay. Sessions count per stay in a lifecycle state, against a
//! ceiling for the whole run that only a cycle across states reaches.

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
            earlier.session_counts.note(state, kind);
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
            state.session_counts.note(&here, kind);
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
    assert_eq!(pilot.over_budget_in("review"), None);
    assert_eq!(pilot.state().session_counts, Default::default());
}

#[test]
fn redoing_a_phase_starts_its_stay_again_and_keeps_the_run_count() {
    let fx = bare();
    let mut state = AutopilotState::fresh();
    for _ in 0..5 {
        state.sessions += 1;
        state.session_counts.note("design", "phase");
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
