//! The change-feed thread: turns `tod_store::journey_changes` rows into
//! journey events (`doc/journeys/implementation-plan.md` step 2c).
//!
//! The row-to-event logic ([`process_once`]) is pulled out of the thread
//! loop so tests can drive it directly against a real store, without
//! spinning a thread or waiting on the broadcast channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tod_journey::{Actor, Event, JourneyKey, Regression as JourneyRegression, RowRef};
use tod_store::fleet::FleetStore;
use tod_store::journey_changes::{self, ChangeRow};
use tod_store::settings::JourneySettings;
use uuid::Uuid;

use crate::journey::Recorder;
use crate::lifecycle_validity;

const MARK_FILE: &str = "changes.mark";
/// How often the thread persists its high-water mark to disk and asks the
/// writer to prune `journey_changes` through it, beyond doing so whenever a
/// signal arrives.
const PRUNE_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn mark_path(journeys_dir: &Path) -> PathBuf {
    journeys_dir.join(MARK_FILE)
}

/// The persisted high-water mark, or 0 if there is none yet.
pub fn read_mark(journeys_dir: &Path) -> i64 {
    std::fs::read_to_string(mark_path(journeys_dir))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_mark(journeys_dir: &Path, id: i64) {
    let _ = std::fs::create_dir_all(journeys_dir);
    let tmp = mark_path(journeys_dir).with_extension("mark.tmp");
    if std::fs::write(&tmp, id.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, mark_path(journeys_dir));
    }
}

/// Per-node last-recorded validity target, kept in memory so `Validity` is
/// only recorded when it changes (`None` means "holds").
#[derive(Default)]
pub struct ValidityCache(HashMap<Uuid, Option<String>>);

/// Reads every `journey_changes` row after `high_water`, groups by node, and
/// records `DataChanged`, `Transition`/`Milestone`, and `Validity` events.
/// Returns the new high-water mark (unchanged when there was nothing new).
pub fn process_once(
    conn: &Connection,
    store: &FleetStore,
    recorder: &Recorder,
    settings: &JourneySettings,
    validity_cache: &mut ValidityCache,
    high_water: i64,
) -> anyhow::Result<i64> {
    let rows = journey_changes::changes_after(conn, high_water)?;
    if rows.is_empty() {
        return Ok(high_water);
    }

    let mut new_high_water = high_water;
    let mut by_node: HashMap<Uuid, Vec<&ChangeRow>> = HashMap::new();
    for row in &rows {
        new_high_water = new_high_water.max(row.id);
        by_node.entry(row.node_id).or_default().push(row);
    }

    for (node_id, node_rows) in &by_node {
        let refs: Vec<RowRef> = node_rows
            .iter()
            .map(|r| RowRef {
                table: r.table.clone(),
                row_id: r.row_id.clone(),
                op: r.op.clone(),
                old_state: r.old_state.clone(),
                new_state: r.new_state.clone(),
            })
            .collect();
        recorder.record(
            JourneyKey::Node(*node_id),
            Actor::App,
            Event::DataChanged { rows: refs },
        );

        for row in node_rows.iter().filter(|r| r.table == "node_lifecycle") {
            let from = row.old_state.clone().unwrap_or_default();
            let to = row.new_state.clone().unwrap_or_default();
            recorder.record(
                JourneyKey::Node(*node_id),
                Actor::App,
                Event::Transition {
                    from,
                    to: to.clone(),
                },
            );
            if settings.milestone_states.iter().any(|state| state == &to) {
                let seq = recorder.record_and_get_seq(
                    JourneyKey::Node(*node_id),
                    Actor::App,
                    Event::Milestone { state: to.clone() },
                );
                recorder.compact(JourneyKey::Node(*node_id));

                if let Some(seq) = seq {
                    if settings.send {
                        queue_milestone_submission(store, recorder, *node_id, &to, seq as i64);
                    }
                }
            }
        }

        match lifecycle_validity::regression(conn, *node_id) {
            Ok(regression) => {
                let target = regression.as_ref().map(|r| r.target.to_string());
                let changed = validity_cache.0.get(node_id) != Some(&target);
                if changed {
                    recorder.record(
                        JourneyKey::Node(*node_id),
                        Actor::App,
                        Event::Validity {
                            regression: regression.map(|r| JourneyRegression {
                                target: r.target.to_string(),
                                reasons: r.reasons,
                            }),
                        },
                    );
                    validity_cache.0.insert(*node_id, target);
                }
            }
            Err(err) => {
                tracing::warn!("journey: lifecycle_validity::regression failed: {err:#}");
            }
        }
    }

    Ok(new_high_water)
}

/// Queues a bundle submission for a just-recorded `Milestone` event
/// (`doc/journeys/spec.md` §6): inserts a `journey_submissions` entry with
/// reason `milestone:<state>` and records the corresponding
/// `Event::Submission`, mirroring `submit_report`'s pattern in
/// `tod-ui`'s `app::window`. Before inserting, abandons any still-`queued`
/// milestone entry for the same node — a later bundle contains everything an
/// earlier, unsent one did — but never touches a `report` entry.
fn queue_milestone_submission(store: &FleetStore, recorder: &Recorder, node_id: Uuid, state: &str, seq: i64) {
    match store.queued_milestones_for_node(node_id) {
        Ok(stale) => {
            for entry in stale {
                if let Err(err) = store.set_journey_submission_status(
                    entry.bundle_id,
                    tod_store::journey_submissions::STATUS_ABANDONED,
                ) {
                    tracing::warn!(
                        "journey: failed to abandon superseded milestone submission {}: {err:#}",
                        entry.bundle_id
                    );
                }
            }
        }
        Err(err) => {
            tracing::warn!("journey: failed to list queued milestones for node {node_id}: {err:#}");
            return;
        }
    }

    let bundle_id = Uuid::new_v4();
    let reason = format!("milestone:{state}");
    match store.queue_journey_submission(bundle_id, Some(node_id), seq, &reason) {
        Ok(entry) => {
            recorder.record(
                JourneyKey::Node(node_id),
                Actor::App,
                Event::Submission {
                    bundle: entry.bundle_id,
                    status: "queued".to_string(),
                },
            );
        }
        Err(err) => {
            tracing::warn!("journey: failed to queue milestone submission for node {node_id}: {err:#}");
        }
    }
}

/// Starts the change-feed thread: subscribes to `store.subscribe_changes()`
/// and calls [`process_once`] on every signal (treating a lagged receiver as
/// "drain now"), persisting the high-water mark and periodically pruning
/// `journey_changes` through it.
pub fn spawn(journeys_dir: PathBuf, store: Arc<FleetStore>, recorder: Recorder, settings: JourneySettings) {
    std::thread::Builder::new()
        .name("tod-journey-changes".into())
        .spawn(move || {
            let mut high_water = read_mark(&journeys_dir);
            let mut validity_cache = ValidityCache::default();
            let mut rx = store.subscribe_changes();

            // Catch up on anything that happened before this thread
            // subscribed, then prune once at startup.
            run_once(&store, &recorder, &settings, &mut validity_cache, &mut high_water);
            prune_and_mark(&store, &journeys_dir, high_water);
            let mut last_prune = Instant::now();

            loop {
                match rx.blocking_recv() {
                    Ok(()) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Drain now: fall through to processing.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                run_once(&store, &recorder, &settings, &mut validity_cache, &mut high_water);
                if last_prune.elapsed() >= PRUNE_INTERVAL {
                    prune_and_mark(&store, &journeys_dir, high_water);
                    last_prune = Instant::now();
                }
            }
        })
        .expect("failed to spawn tod-journey changes thread");
}

fn run_once(
    store: &FleetStore,
    recorder: &Recorder,
    settings: &JourneySettings,
    validity_cache: &mut ValidityCache,
    high_water: &mut i64,
) {
    let guard = store.projection();
    let projection = guard.lock().expect("fleet projection mutex");
    let conn = projection.connection();
    match process_once(&conn, store, recorder, settings, validity_cache, *high_water) {
        Ok(new_high_water) => *high_water = new_high_water,
        Err(err) => tracing::warn!("journey: change feed failed: {err:#}"),
    }
}

fn prune_and_mark(store: &FleetStore, journeys_dir: &Path, high_water: i64) {
    write_mark(journeys_dir, high_water);
    if let Err(err) = store.prune_journey_changes_through(high_water) {
        tracing::warn!("journey: prune failed: {err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::outline::{Capability, CreatePosition, KIND_REQUIREMENT, OutlineMutation};

    fn fixture() -> (PathBuf, FleetStore, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-journey-changes-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        (root, fleet, node)
    }

    #[test]
    fn records_data_changed_for_an_obligation_mutation() {
        let (root, fleet, node) = fixture();
        let journeys_dir = root.join("journeys");
        let recorder = crate::journey::recorder::spawn(journeys_dir.clone(), 1024);
        let settings = JourneySettings::default();
        let mut validity_cache = ValidityCache::default();

        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Spec],
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        fleet
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(Uuid::new_v4()),
                node_id: node,
                kind: KIND_REQUIREMENT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "must do the thing".into(),
                phase: "requirements".into(),
            })
            .unwrap();

        let guard = fleet.projection();
        let high_water = {
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            process_once(&conn, &fleet, &recorder, &settings, &mut validity_cache, 0).unwrap()
        };
        assert!(high_water > 0);

        // Drop the recorder and give the writer thread a moment to flush,
        // then read the journey back to confirm exactly one `DataChanged`.
        drop(recorder);
        std::thread::sleep(Duration::from_millis(200));
        let reader = tod_journey::JourneyReader::open(&journeys_dir, JourneyKey::Node(node)).unwrap();
        let records = reader.all();
        let data_changed: Vec<_> = records
            .iter()
            .filter(|r| matches!(r.event, Event::DataChanged { .. }))
            .collect();
        assert_eq!(data_changed.len(), 1, "{records:?}");
        if let Event::DataChanged { rows } = &data_changed[0].event {
            assert!(rows.iter().any(|r| r.table == "node_obligations" && r.op == "insert"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn verifying_transition_records_transition_and_milestone_and_compacts() {
        let (root, fleet, node) = fixture();
        let journeys_dir = root.join("journeys");
        let recorder = crate::journey::recorder::spawn(journeys_dir.clone(), 1024);
        let settings = JourneySettings::default();
        let mut validity_cache = ValidityCache::default();

        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Lifecycle],
            })
            .unwrap();
        {
            let guard = fleet.projection();
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            process_once(&conn, &fleet, &recorder, &settings, &mut validity_cache, 0).unwrap();
        }

        fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: node,
                state: "verifying".into(),
            })
            .unwrap();
        let high_water = {
            let guard = fleet.projection();
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            let hw = journey_changes::changes_after(&conn, 0)
                .unwrap()
                .last()
                .map(|r| r.id - 1)
                .unwrap_or(0);
            process_once(&conn, &fleet, &recorder, &settings, &mut validity_cache, hw).unwrap()
        };
        assert!(high_water > 0);

        drop(recorder);
        std::thread::sleep(Duration::from_millis(200));
        let reader = tod_journey::JourneyReader::open(&journeys_dir, JourneyKey::Node(node)).unwrap();
        let records = reader.all();
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.event, Event::Transition { to, .. } if to == "verifying")),
            "{records:?}"
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.event, Event::Milestone { state } if state == "verifying")),
            "{records:?}"
        );
        // Compaction happened: the tail is now empty (or the .zst exists).
        let zst = journeys_dir.join(format!("{node}.journey.zst"));
        assert!(zst.exists(), "expected a compacted .zst for the node journey");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn later_milestone_supersedes_the_earlier_still_queued_one_for_the_same_node() {
        let (root, fleet, node) = fixture();
        let journeys_dir = root.join("journeys");
        let recorder = crate::journey::recorder::spawn(journeys_dir.clone(), 1024);
        let mut settings = JourneySettings::default();
        settings.send = true;
        let mut validity_cache = ValidityCache::default();

        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Lifecycle],
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: node,
                state: "active".into(),
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: node,
                state: "verifying".into(),
            })
            .unwrap();

        {
            let guard = fleet.projection();
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            process_once(&conn, &fleet, &recorder, &settings, &mut validity_cache, 0).unwrap();
        }

        // Both "active" and "verifying" are milestone states (defaults), so
        // both fire in this one drain, in order. Only the later one should
        // still be queued: the earlier one is superseded.
        let queued = fleet.queued_milestones_for_node(node).unwrap();
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].reason, "milestone:verifying");

        drop(recorder);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_report_entry_for_the_same_node_is_never_superseded_by_a_milestone() {
        let (root, fleet, node) = fixture();
        let journeys_dir = root.join("journeys");
        let recorder = crate::journey::recorder::spawn(journeys_dir.clone(), 1024);
        let mut settings = JourneySettings::default();
        settings.send = true;
        let mut validity_cache = ValidityCache::default();

        let report_bundle = Uuid::new_v4();
        fleet
            .queue_journey_submission(report_bundle, Some(node), 1, "report")
            .unwrap();

        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Lifecycle],
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: node,
                state: "active".into(),
            })
            .unwrap();

        {
            let guard = fleet.projection();
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            process_once(&conn, &fleet, &recorder, &settings, &mut validity_cache, 0).unwrap();
        }

        let conn = rusqlite::Connection::open(fleet.writer().db_path()).unwrap();
        let repo = tod_store::journey_submissions::JourneySubmissionRepo::new(&conn);
        let report = repo.get_by_bundle(report_bundle).unwrap().unwrap();
        assert_eq!(report.status, tod_store::journey_submissions::STATUS_QUEUED);

        // The milestone still got its own queued entry.
        let queued = fleet.queued_milestones_for_node(node).unwrap();
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].reason, "milestone:active");

        drop(recorder);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn milestone_with_sending_off_never_queues_a_submission() {
        let (root, fleet, node) = fixture();
        let journeys_dir = root.join("journeys");
        let recorder = crate::journey::recorder::spawn(journeys_dir.clone(), 1024);
        let settings = JourneySettings::default(); // send: false
        let mut validity_cache = ValidityCache::default();

        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Lifecycle],
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: node,
                state: "active".into(),
            })
            .unwrap();

        {
            let guard = fleet.projection();
            let projection = guard.lock().unwrap();
            let conn = projection.connection();
            process_once(&conn, &fleet, &recorder, &settings, &mut validity_cache, 0).unwrap();
        }

        let queued = fleet.queued_milestones_for_node(node).unwrap();
        assert!(queued.is_empty(), "{queued:?}");

        // The milestone event itself was still recorded (existing Step 2
        // behavior), sending being off only gates the submission side-effect.
        drop(recorder);
        std::thread::sleep(Duration::from_millis(200));
        let reader = tod_journey::JourneyReader::open(&journeys_dir, JourneyKey::Node(node)).unwrap();
        let records = reader.all();
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.event, Event::Milestone { state } if state == "active")),
            "{records:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
