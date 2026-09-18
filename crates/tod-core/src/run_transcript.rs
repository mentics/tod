//! An agent run's transcript, read from the platform's own record of the
//! session ([`tod_agent::read_transcript`] — a read of its files, with no
//! agent involved) and kept on the run (`agent_runs.cached_transcript`), so
//! it outlives the platform's copy: Claude Code deletes old session logs.
//!
//! [`spawn_capture`] captures a run's transcript as soon as the run ends,
//! and sweeps every run at startup and daily for what that missed — runs
//! that ended while the app was closed, and sessions continued outside it.
//! A stored transcript is read again only when its fingerprint (the
//! session's latest message) has moved on.
//!
//! Every agent session tod started is recorded too (`agent_sessions`, not
//! just fleet runs), and is read and kept the same way: [`session_capture`]
//! reads one, and the sweep covers them all.
//!
//! The platforms' formats are their own and change without notice. A read
//! that meets something it does not know keeps what it could read, and
//! reports the rest ([`FormatProblem`]) so the reader can be updated; the
//! transcript is then stored without a fingerprint, so it is read again —
//! and reported again — until the reader knows the format.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::Result;
use tod_agent::{
    AgentPlatform, FormatProblem, Transcript, TranscriptRead, read_transcript,
    transcript_fingerprint,
};
use tod_store::fleet::{AgentRun, AgentSession, FleetMutation, FleetStore};
use tokio::sync::broadcast::error::RecvError;

/// How often every run is checked again while the app stays open, for
/// sessions continued outside it.
const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The transcript stored on `run`, if it has one in the current form.
pub fn stored(run: &AgentRun) -> Option<Transcript> {
    run.cached_transcript
        .as_deref()
        .and_then(Transcript::from_stored)
}

/// Whether `run`'s transcript should be read: it has an agent-side session,
/// it has ended, and either nothing usable is stored or the session has
/// moved on since it was.
///
/// Reads the platform's record for the fingerprint, so call it off the UI
/// thread.
pub fn needs_capture(run: &AgentRun) -> bool {
    let Some(session_id) = run.agent_session_id.as_deref() else {
        return false;
    };
    if run.is_live() {
        return false;
    }
    let Some(platform) = platform(run) else {
        return false;
    };
    if stored(run).is_none() {
        return true;
    }
    transcript_fingerprint(platform, session_id)
        .is_some_and(|now| run.transcript_fingerprint.as_deref() != Some(now.as_str()))
}

/// The platform `run` was started on, when it recorded one.
pub fn platform(run: &AgentRun) -> Option<AgentPlatform> {
    run.platform.as_deref().and_then(tod_store::parse_platform)
}

/// Read `run`'s transcript and store it, with what in the platform's record
/// the reader did not know (also logged). `Ok(None)` when there is nothing
/// to read: the run has no session, or the platform has no record of it.
/// Reads files, so run it off the UI thread.
pub fn capture(fleet: &FleetStore, run: &AgentRun) -> Result<Option<TranscriptRead>> {
    let (Some(platform), Some(session_id)) = (platform(run), run.agent_session_id.as_deref())
    else {
        return Ok(None);
    };
    let fingerprint = transcript_fingerprint(platform, session_id);
    let Some(read) = read_transcript(platform, session_id)? else {
        return Ok(None);
    };
    for problem in &read.problems {
        tracing::warn!(
            run_id = %run.id,
            platform = platform.label(),
            "transcript format not recognized: {problem}"
        );
    }
    fleet.enqueue(FleetMutation::CacheAgentRunTranscript {
        run_id: run.id.clone(),
        transcript: read.transcript.to_stored(),
        // Read again until the reader knows the format.
        fingerprint: fingerprint.filter(|_| read.problems.is_empty()),
    })?;
    Ok(Some(read))
}

/// The platforms `session`'s record may be on: the one recorded, or each of
/// them for a session recorded before tod kept it.
pub fn session_platforms(session: &AgentSession) -> Vec<AgentPlatform> {
    match session.platform.as_deref().and_then(tod_store::parse_platform) {
        Some(platform) => vec![platform],
        None => vec![AgentPlatform::Claude, AgentPlatform::Cursor],
    }
}

/// Whether `session`'s transcript should be read: nothing usable is stored,
/// or the session has moved on since it was. Reads the platform's record for
/// the fingerprint, so call it off the UI thread.
pub fn session_needs_capture(session: &AgentSession) -> bool {
    let stored = session
        .cached_transcript
        .as_deref()
        .and_then(Transcript::from_stored);
    if stored.is_none() {
        return true;
    }
    session_platforms(session).into_iter().any(|platform| {
        transcript_fingerprint(platform, &session.agent_session_id).is_some_and(|now| {
            session.transcript_fingerprint.as_deref() != Some(now.as_str())
        })
    })
}

/// Read `session`'s transcript from its platform's record and keep it.
/// `Ok(None)` when no platform has a record of it.
pub fn session_capture(
    fleet: &FleetStore,
    session: &AgentSession,
) -> Result<Option<(AgentPlatform, TranscriptRead)>> {
    for platform in session_platforms(session) {
        let id = &session.agent_session_id;
        let fingerprint = transcript_fingerprint(platform, id);
        let Some(read) = read_transcript(platform, id)? else {
            continue;
        };
        for problem in &read.problems {
            tracing::warn!(
                agent_session_id = %id,
                platform = platform.label(),
                "transcript format not recognized: {problem}"
            );
        }
        fleet.enqueue(FleetMutation::CacheAgentSessionTranscript {
            agent_session_id: id.clone(),
            platform: tod_store::platform_storage(platform).to_string(),
            transcript: read.transcript.to_stored(),
            // Read again until the reader knows the format.
            fingerprint: fingerprint.filter(|_| read.problems.is_empty()),
        })?;
        return Ok(Some((platform, read)));
    }
    Ok(None)
}

/// What a capture met in a platform's record that the reader does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatNotice {
    pub platform: AgentPlatform,
    pub problems: Vec<FormatProblem>,
}

/// Capture each run's transcript when it ends, after a first sweep of every
/// run, and sweep again daily. Runs on its own threads for the life of the
/// app; call it once, after launch has settled which runs are still live.
///
/// `on_format_change` hears of each kind of problem once per app run, the
/// first time a capture meets it; it is called on the capture thread.
pub fn spawn_capture(
    fleet: Arc<FleetStore>,
    on_format_change: impl Fn(FormatNotice) + Send + 'static,
) {
    let (changed_tx, changed_rx) = mpsc::channel();
    let mut changes = fleet.subscribe_changes();
    let spawned = std::thread::Builder::new()
        .name("transcript-capture-changes".into())
        .spawn(move || {
            loop {
                match changes.blocking_recv() {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        if changed_tx.send(()).is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        });
    if let Err(err) = spawned {
        tracing::error!("transcript capture: could not watch for runs ending: {err}");
    }
    let spawned = std::thread::Builder::new()
        .name("transcript-capture".into())
        .spawn(move || {
            let mut reported = HashSet::new();
            let mut report = |platform: AgentPlatform, problems: Vec<FormatProblem>| {
                let problems: Vec<_> = problems
                    .into_iter()
                    .filter(|problem| reported.insert((platform, problem.what.clone())))
                    .collect();
                if !problems.is_empty() {
                    on_format_change(FormatNotice { platform, problems });
                }
            };
            sweep_sessions(&fleet, &mut report);
            let mut report = |run: &AgentRun, problems| {
                if let Some(platform) = platform(run) {
                    report(platform, problems);
                }
            };
            sweep(&fleet, &mut report);
            let mut live = live_runs(&fleet);
            let mut next_sweep = Instant::now() + SWEEP_INTERVAL;
            loop {
                match changed_rx.recv_timeout(next_sweep.saturating_duration_since(Instant::now()))
                {
                    Ok(()) => {
                        // One look covers every change that queued up meanwhile.
                        while changed_rx.try_recv().is_ok() {}
                        let now_live = live_runs(&fleet);
                        for run_id in live.difference(&now_live) {
                            match fleet.get_run(run_id) {
                                Ok(Some(run)) => capture_if_needed(&fleet, &run, &mut report),
                                Ok(None) => {}
                                Err(err) => {
                                    tracing::warn!(%run_id, "transcript capture: {err:#}");
                                }
                            }
                        }
                        live = now_live;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        sweep(&fleet, &mut report);
                        live = live_runs(&fleet);
                        next_sweep = Instant::now() + SWEEP_INTERVAL;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        // No more change notices; keep the daily sweep.
                        std::thread::sleep(next_sweep.saturating_duration_since(Instant::now()));
                        sweep(&fleet, &mut report);
                        next_sweep = Instant::now() + SWEEP_INTERVAL;
                    }
                }
            }
        });
    if let Err(err) = spawned {
        tracing::error!("transcript capture: could not start: {err}");
    }
}

/// Capture every recorded session whose transcript is missing or behind.
/// Only at startup: a session is read again when someone looks at it.
fn sweep_sessions(fleet: &FleetStore, report: &mut impl FnMut(AgentPlatform, Vec<FormatProblem>)) {
    let sessions = match fleet.list_agent_sessions() {
        Ok(sessions) => sessions,
        Err(err) => {
            tracing::error!("transcript capture: listing agent sessions failed: {err:#}");
            return;
        }
    };
    for session in &sessions {
        if !session_needs_capture(session) {
            continue;
        }
        match session_capture(fleet, session) {
            Ok(Some((platform, read))) if !read.problems.is_empty() => {
                report(platform, read.problems)
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(
                agent_session_id = %session.agent_session_id,
                "transcript capture failed: {err:#}"
            ),
        }
    }
}

/// Capture every run whose transcript is missing or behind its session.
fn sweep(fleet: &FleetStore, report: &mut impl FnMut(&AgentRun, Vec<FormatProblem>)) {
    match fleet.list_all_runs() {
        Ok(runs) => {
            for run in &runs {
                capture_if_needed(fleet, run, report);
            }
        }
        Err(err) => tracing::error!("transcript capture: listing runs failed: {err:#}"),
    }
}

fn capture_if_needed(
    fleet: &FleetStore,
    run: &AgentRun,
    report: &mut impl FnMut(&AgentRun, Vec<FormatProblem>),
) {
    if !needs_capture(run) {
        return;
    }
    match capture(fleet, run) {
        Ok(Some(read)) if !read.problems.is_empty() => report(run, read.problems),
        Ok(_) => {}
        Err(err) => tracing::warn!(run_id = %run.id, "transcript capture failed: {err:#}"),
    }
}

fn live_runs(fleet: &FleetStore) -> HashSet<String> {
    match fleet.list_unended_runs() {
        Ok(runs) => runs
            .into_iter()
            .filter(AgentRun::is_live)
            .map(|run| run.id)
            .collect(),
        Err(err) => {
            tracing::warn!("transcript capture: listing live runs failed: {err:#}");
            HashSet::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_agent::{AgentLaunchOptions, TranscriptTurn};
    use tod_store::outline::OutlineMutation;

    /// A store with one node, and a Claude config dir holding session `s1`.
    fn setup() -> (std::path::PathBuf, Arc<FleetStore>, String) {
        let root = std::env::temp_dir().join(format!("tod-run-transcript-{}", uuid::Uuid::new_v4()));
        let project = root.join("claude").join("projects").join("C--repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("s1.jsonl"),
            [
                r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"Hi."}}"#,
                r#"{"type":"assistant","uuid":"u2","message":{"role":"assistant","content":[{"type":"text","text":"Hello."}]}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        // SAFETY: no other test in this crate reads CLAUDE_CONFIG_DIR, and
        // it names the same fixture layout for any test that sets it.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", root.join("claude")) };

        let fleet = Arc::new(FleetStore::open(&root.join("data")).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node_id = fleet.flatten_outline(list_id).unwrap()[0].node.id.to_string();
        fleet
            .enqueue(FleetMutation::CreateAgentRun {
                node_id: node_id.clone(),
                run_kind: Some("interactive".into()),
                session_name: None,
                launch: Some(AgentLaunchOptions::for_platform(AgentPlatform::Claude)),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let run_id = fleet.list_all_runs().unwrap()[0].id.clone();
        fleet
            .enqueue(FleetMutation::SetAgentRunSessionId {
                run_id: run_id.clone(),
                agent_session_id: "s1".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        (root, fleet, run_id)
    }

    #[test]
    fn a_run_is_captured_when_it_ends_and_not_before() {
        let (root, fleet, run_id) = setup();
        let (notices_tx, notices) = mpsc::channel();
        spawn_capture(fleet.clone(), move |notice| {
            let _ = notices_tx.send(notice);
        });

        // Live: the startup sweep leaves it alone.
        std::thread::sleep(Duration::from_millis(300));
        assert!(fleet.get_run(&run_id).unwrap().unwrap().cached_transcript.is_none());

        fleet
            .enqueue(FleetMutation::EndAgentRun {
                run_id: run_id.clone(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let run = loop {
            let _ = fleet.writer().flush();
            let run = fleet.get_run(&run_id).unwrap().unwrap();
            if run.cached_transcript.is_some() || Instant::now() > deadline {
                break run;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        let transcript = stored(&run).expect("captured when the run ended");
        assert_eq!(
            transcript.turns[0],
            TranscriptTurn::User { text: "Hi.".into() }
        );
        assert_eq!(transcript.turns[1].text(), "Hello.");
        assert_eq!(run.transcript_fingerprint.as_deref(), Some("u2"));
        assert!(!needs_capture(&run), "current once captured");
        assert!(notices.try_recv().is_err(), "nothing unexpected in the log");

        // The session goes on, in a form the reader does not know.
        let log = root.join("claude").join("projects").join("C--repo").join("s1.jsonl");
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str("
{\"type\":\"telepathy\",\"uuid\":\"u3\"}
");
        std::fs::write(&log, text).unwrap();
        assert!(needs_capture(&run));
        let read = capture(&fleet, &run).unwrap().unwrap();
        fleet.writer().flush().unwrap();
        let run = fleet.get_run(&run_id).unwrap().unwrap();

        assert_eq!(
            read.problems,
            vec![FormatProblem {
                what: "an unknown record type \"telepathy\"".into(),
                count: 1
            }]
        );
        assert_eq!(stored(&run).unwrap().turns.len(), 2, "what it could read is kept");
        assert_eq!(run.transcript_fingerprint, None);
        assert!(needs_capture(&run), "read again until the reader knows the format");
        let _ = std::fs::remove_dir_all(&root);
    }
}
