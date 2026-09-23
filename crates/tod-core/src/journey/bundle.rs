//! Builds a submission bundle from a journey (spec §5.3, implementation plan
//! step 6e): a `Manifest`, a `Settings` snapshot, the journey's own records up
//! to the queued seq, and one `Resolved` record per distinct reference those
//! records carry.
//!
//! What "reference" means here is deliberately narrow: as of step 6d, no
//! `Event` variant carries a [`tod_journey::Reference`] itself — `Reference`
//! only exists as the payload type of the bundle-only `Event::Resolved`. So
//! this exporter derives references from the data the current events *do*
//! carry (an `AgentTurn`/`ProtocolDecision`/`SessionRotated`'s `conversation`
//! id, and a `DataChanged`'s `RowRef`s) rather than reading a `Reference` off
//! the source records directly. See the module-level TODOs below for the
//! categories the spec names that the current record shapes cannot yet
//! support without changing earlier steps.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use tod_journey::bundle::BundleWriter;
use tod_journey::{Actor, Event, JourneyKey, JourneyReader, Manifest, Reference, Resolution};
use uuid::Uuid;

use tod_agent::AgentPlatform;
use tod_store::conversation::ConversationRepo;
use tod_store::fleet::FleetStore;
use tod_store::journey_submissions::SubmissionEntry;
use tod_store::settings::TodSettings;

use crate::journey::settings_snapshot;
use crate::media::MediaPaths;
use crate::process_bundle::TodInstallPaths;

/// Current fleet schema version, embedded in the manifest so a bundle can be
/// read against the right migration history.
fn current_schema_version() -> u32 {
    tod_store::fleet::schema::CURRENT_USER_VERSION as u32
}

fn parse_platform(platform: Option<&str>) -> Option<AgentPlatform> {
    match platform {
        Some("claude") => Some(AgentPlatform::Claude),
        Some("cursor") => Some(AgentPlatform::Cursor),
        _ => None,
    }
}

/// One conversation-shaped reference, tracked in first-seen order while
/// walking the journey's records.
struct ConversationRef {
    id: Uuid,
    min_seq: i64,
    max_seq: i64,
}

/// One row-shaped reference (a `DataChanged` `RowRef`), tracked with the
/// latest op/state seen up to the cutoff seq (the journey's own history is
/// the only "current state" source this exporter has — see the TODO below).
#[derive(Clone)]
struct RowRefState {
    op: String,
    new_state: Option<String>,
}

/// Builds a bundle for `entry`: the queued submission this bundle answers.
/// `node` (when the entry has one) is looked up from `store` for the
/// manifest's slug/title; `entry.node_id` decides whether the bundle reads
/// the node's journey or the project journey.
pub fn build_bundle(
    store: &FleetStore,
    journeys_dir: &Path,
    data_root: &Path,
    install: &TodInstallPaths,
    media: &MediaPaths,
    cli_path: &Path,
    settings: &TodSettings,
    entry: &SubmissionEntry,
) -> Result<Vec<u8>> {
    let key = match entry.node_id {
        Some(id) => JourneyKey::Node(id),
        None => JourneyKey::Project,
    };
    let include_transcripts = settings.journeys.include_transcripts;

    // Look up node title/slug (and its dev-container setting, for the
    // settings snapshot) when the bundle is about a node.
    let (slug, title, dev_container) = match entry.node_id {
        Some(id) => {
            let task = store
                .get_node(&id.to_string())
                .context("looking up node for bundle manifest")?;
            match task {
                Some(t) => (Some(t.slug), Some(t.title), None),
                None => (None, None, None),
            }
        }
        None => (None, None, None),
    };

    let mut writer = BundleWriter::new().context("opening bundle writer")?;

    let manifest = Manifest {
        bundle: entry.bundle_id,
        reason: entry.reason.clone(),
        node_id: entry.node_id,
        slug,
        title,
        queued_seq: entry.seq as u64,
        cli_build_stamp: crate::CLI_BUILD_STAMP.to_string(),
        git_commit: crate::GIT_COMMIT.to_string(),
        git_dirty: crate::GIT_DIRTY == "true",
        crate_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: current_schema_version(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        transcripts_included: include_transcripts,
    };
    writer
        .append(Actor::App, Event::Manifest { manifest })
        .context("writing manifest record")?;

    let snapshot = settings_snapshot(install, media, data_root, cli_path, settings, dev_container.as_ref());
    let snapshot_json = serde_json::to_string(&snapshot).context("serializing settings snapshot")?;
    writer
        .append(
            Actor::App,
            Event::Settings {
                snapshot: snapshot_json,
            },
        )
        .context("writing settings record")?;

    // Stream the journey's own records up to and including the queued seq,
    // stripping report screenshots when transcripts are withheld, and
    // collecting references as we go.
    let reader = JourneyReader::open(journeys_dir, key)
        .context("opening journey for bundle")?
        .up_to(entry.seq as u64);

    let mut conversations: Vec<ConversationRef> = Vec::new();
    let mut conversation_index: BTreeMap<Uuid, usize> = BTreeMap::new();
    // Ordered distinct rows, keyed by (table, row_id), with the state as of
    // the last record seen for that row up to the cutoff.
    let mut row_order: Vec<(String, String)> = Vec::new();
    let mut row_state: BTreeMap<(String, String), RowRefState> = BTreeMap::new();

    for mut record in reader {
        match &mut record.event {
            Event::Report {
                screenshot: screenshot @ Some(_),
                ..
            } if !include_transcripts => {
                *screenshot = None;
            }
            _ => {}
        }

        match &record.event {
            Event::AgentTurn { conversation, phase } => {
                let seq = match phase {
                    tod_journey::TurnPhase::Started { user_seq } => *user_seq as i64,
                    tod_journey::TurnPhase::Replied { seq } | tod_journey::TurnPhase::Failed { seq, .. } => {
                        *seq as i64
                    }
                    tod_journey::TurnPhase::Stopped => 0,
                };
                note_conversation(&mut conversations, &mut conversation_index, *conversation, seq);
            }
            Event::ProtocolDecision { conversation, .. } | Event::SessionRotated { conversation, .. } => {
                note_conversation(&mut conversations, &mut conversation_index, *conversation, 0);
            }
            Event::DataChanged { rows } => {
                for row in rows {
                    let key = (row.table.clone(), row.row_id.clone());
                    if !row_state.contains_key(&key) {
                        row_order.push(key.clone());
                    }
                    row_state.insert(
                        key,
                        RowRefState {
                            op: row.op.clone(),
                            new_state: row.new_state.clone(),
                        },
                    );
                }
            }
            _ => {}
        }

        // Re-numbered with the bundle's own seq counter (`writer.append_at`,
        // not `append_record`): the bundle is a fresh CBOR stream whose reader
        // requires strictly increasing seqs, and the journey's own seqs
        // would collide with the Manifest/Settings records already written
        // ahead of it. The time is kept: it is what the timeline and stats
        // are about.
        writer.append_at(record.at, record.actor, record.event).context("writing journey record to bundle")?;
    }

    // Emit resolutions in first-referenced order: conversations first (each
    // as turns / opening context / actions / transcript), then rows.
    for conv in &conversations {
        resolve_conversation(&mut writer, store, conv.id, conv.min_seq, conv.max_seq, include_transcripts)?;
    }
    for key in &row_order {
        let state = row_state.get(key).expect("row_order and row_state stay in sync");
        resolve_row(&mut writer, store, key, state, include_transcripts)?;
    }

    // Note: gate results are also carried inline by `Event::GateResult`
    // (full report and criterion results) in the streamed records above.

    writer.finish().context("finishing bundle")
}

fn note_conversation(
    conversations: &mut Vec<ConversationRef>,
    index: &mut BTreeMap<Uuid, usize>,
    id: Uuid,
    seq: i64,
) {
    match index.get(&id) {
        Some(&ix) => {
            let conv = &mut conversations[ix];
            if seq > 0 {
                conv.min_seq = conv.min_seq.min(seq);
                conv.max_seq = conv.max_seq.max(seq);
            }
        }
        None => {
            index.insert(id, conversations.len());
            conversations.push(ConversationRef {
                id,
                min_seq: if seq > 0 { seq } else { 1 },
                max_seq: seq.max(1),
            });
        }
    }
}

/// One resolved conversation's content: its turns in range, its opening
/// context, its actions, and (when available) its agent session transcript.
/// Bundled together under one `Resolved` record per sub-category since
/// nothing in the current event shapes names these separately.
#[derive(serde::Serialize)]
struct ResolvedConversationTurns {
    turns: Vec<tod_store::conversation::Turn>,
}

#[derive(serde::Serialize)]
struct ResolvedConversationActions {
    actions: Vec<ActionSummary>,
}

/// A JSON-friendly projection of `ActionRow` (which is not itself
/// serializable end-to-end via a stable schema here; only the fields useful
/// for review are carried).
#[derive(serde::Serialize)]
struct ActionSummary {
    id: i64,
    turn_seq: i64,
    kind: String,
    entity: String,
    entity_id: Uuid,
}

fn resolve_conversation(
    writer: &mut BundleWriter,
    store: &FleetStore,
    conversation_id: Uuid,
    min_seq: i64,
    max_seq: i64,
    include_transcripts: bool,
) -> Result<()> {
    let conversation = store
        .read(|conn| ConversationRepo::new(conn).get(conversation_id))
        .context("loading conversation for bundle resolution")?;

    // conversation-turns
    let turns_ref = Reference {
        kind: "conversation-turns".to_string(),
        id: conversation_id.to_string(),
        from_seq: Some(min_seq.max(1) as u64),
        to_seq: Some(max_seq.max(1) as u64),
    };
    let turns_content = if !include_transcripts {
        withheld_placeholder()
    } else {
        let turns = store
            .read(|conn| ConversationRepo::new(conn).turns_range(conversation_id, min_seq.max(1), max_seq.max(1)))
            .context("loading conversation turns for bundle")?;
        found_json(&ResolvedConversationTurns { turns })?
    };
    writer
        .append(
            Actor::App,
            Event::Resolved {
                reference: turns_ref,
                content: turns_content,
            },
        )
        .context("writing conversation-turns resolution")?;

    // conversation-opening-context
    let opening_ref = Reference {
        kind: "conversation-opening-context".to_string(),
        id: conversation_id.to_string(),
        from_seq: None,
        to_seq: None,
    };
    let opening_content = if !include_transcripts {
        withheld_placeholder()
    } else {
        let opening = store
            .read(|conn| ConversationRepo::new(conn).opening_context(conversation_id))
            .context("loading opening context for bundle")?;
        match opening {
            Some(text) => Resolution::Found {
                data: tod_journey::Blob {
                    mime: "text/plain".to_string(),
                    bytes: text.into_bytes(),
                },
            },
            None => Resolution::Missing,
        }
    };
    writer
        .append(
            Actor::App,
            Event::Resolved {
                reference: opening_ref,
                content: opening_content,
            },
        )
        .context("writing opening-context resolution")?;

    // conversation-actions (all actions on the conversation; the current
    // record shapes give no per-action ids to filter by — see the module
    // doc comment).
    let actions_ref = Reference {
        kind: "conversation-actions".to_string(),
        id: conversation_id.to_string(),
        from_seq: None,
        to_seq: None,
    };
    let actions = store
        .read(|conn| ConversationRepo::new(conn).actions(conversation_id))
        .context("loading conversation actions for bundle")?;
    let summaries: Vec<ActionSummary> = actions
        .into_iter()
        .map(|a| ActionSummary {
            id: a.id,
            turn_seq: a.turn_seq,
            kind: format!("{:?}", a.kind),
            entity: format!("{:?}", a.entity),
            entity_id: a.entity_id,
        })
        .collect();
    writer
        .append(
            Actor::App,
            Event::Resolved {
                reference: actions_ref,
                content: found_json(&ResolvedConversationActions { actions: summaries })?,
            },
        )
        .context("writing conversation-actions resolution")?;

    // agent session transcript
    let transcript_ref = Reference {
        kind: "conversation-transcript".to_string(),
        id: conversation_id.to_string(),
        from_seq: None,
        to_seq: None,
    };
    let transcript_content = match conversation.as_ref().and_then(|c| c.agent_session_id.as_deref()) {
        None => Resolution::Missing,
        Some(session_id) => {
            if !include_transcripts {
                withheld_placeholder()
            } else {
                let platform = conversation.as_ref().and_then(|c| parse_platform(c.platform.as_deref()));
                match platform {
                    None => Resolution::Missing,
                    Some(platform) => match tod_agent::read_transcript(platform, session_id) {
                        Ok(Some(read)) => found_json(&read.transcript)?,
                        Ok(None) | Err(_) => Resolution::Missing,
                    },
                }
            }
        }
    };
    writer
        .append(
            Actor::App,
            Event::Resolved {
                reference: transcript_ref,
                content: transcript_content,
            },
        )
        .context("writing conversation-transcript resolution")?;

    Ok(())
}

fn resolve_row(
    writer: &mut BundleWriter,
    store: &FleetStore,
    key: &(String, String),
    state: &RowRefState,
    include_transcripts: bool,
) -> Result<()> {
    let (table, row_id) = key;
    let reference = Reference {
        kind: "row".to_string(),
        id: format!("{table}:{row_id}"),
        from_seq: None,
        to_seq: None,
    };
    // Spec §5.3: resolve the row's current content from the database
    // (`Missing` if gone). Only if the lookup errors do we fall back to the
    // last state the journey itself recorded for the row.
    let content = if tod_store::journey_rows::is_transcript_table(table) && !include_transcripts {
        withheld_placeholder()
    } else {
        match store.read(|conn| tod_store::journey_rows::fetch_row(conn, table, row_id)) {
            Ok(Some(value)) => found_json(&value)?,
            Ok(None) => Resolution::Missing,
            Err(_) if state.op == "delete" || state.new_state.is_none() => Resolution::Missing,
            Err(_) => Resolution::Found {
                data: tod_journey::Blob {
                    mime: "application/json".to_string(),
                    bytes: state.new_state.clone().unwrap_or_default().into_bytes(),
                },
            },
        }
    };
    writer
        .append(Actor::App, Event::Resolved { reference, content })
        .context("writing row resolution")?;
    Ok(())
}

fn withheld_placeholder() -> Resolution {
    Resolution::Withheld {
        id: String::new(),
        size: 0,
        at: chrono::Utc::now().timestamp_micros(),
    }
}

fn found_json<T: serde::Serialize>(value: &T) -> Result<Resolution> {
    let bytes = serde_json::to_vec(value).context("serializing resolved content")?;
    Ok(Resolution::Found {
        data: tod_journey::Blob {
            mime: "application/json".to_string(),
            bytes,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::fixture;
    use crate::media::MediaPaths;
    use crate::process_bundle::TodInstallPaths;
    use tod_journey::{JourneyWriter, RowRef, TurnPhase};
    use tod_store::conversation::{Focus, ProtocolKind, TurnRole};
    use tod_store::interview::{ACTOR_USER, InterviewCommand};
    use tod_store::settings::TodSettings;

    fn install_paths(dir: &std::path::Path) -> TodInstallPaths {
        std::fs::write(dir.join("README.md"), "process").unwrap();
        TodInstallPaths::from_process_root(dir.to_path_buf()).expect("install paths")
    }

    fn media_paths(dir: &std::path::Path) -> MediaPaths {
        std::fs::create_dir_all(dir.join("context")).unwrap();
        MediaPaths::from_media_root(dir.to_path_buf()).expect("media paths")
    }

    /// Reads back a built bundle into its records.
    fn read_bundle(bytes: &[u8]) -> Vec<tod_journey::Record> {
        tod_journey::bundle::read_bundle(bytes).expect("open bundle").collect()
    }

    #[test]
    fn bundle_carries_manifest_settings_and_resolved_references() {
        let fx = fixture();
        let journeys_dir = fx.root.join("journeys");

        // A conversation with a couple of turns, one carrying sent_context.
        let conversation_id = Uuid::new_v4();
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: Focus::Node(fx.node),
                    protocol: ProtocolKind::Outline,
                    platform: Some("claude".into()),
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::AppendConversationTurn {
                    conversation_id,
                    role: TurnRole::User,
                    body: "add an obligation".into(),
                    parts: Vec::new(),
                    sent_context: Some("delta context".into()),
                },
            )
            .unwrap();
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::AppendConversationTurn {
                    conversation_id,
                    role: TurnRole::Agent,
                    body: String::new(),
                    parts: Vec::new(),
                    sent_context: None,
                },
            )
            .unwrap();

        // A real obligation row, so the "row" reference resolves to
        // something concrete.
        let obligation_id = fx.obligation("Passwords are hashed.");

        // A gate evaluation and an interview transcript row, inserted
        // directly (the triggers still fire).
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02X}")).collect::<String>();
        let transcript_id = Uuid::new_v4();
        let gate_row_id = {
            let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
            let criterion: Vec<u8> = conn
                .query_row("SELECT id FROM gate_criteria LIMIT 1", [], |r| r.get(0))
                .unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO node_gate_evaluations (node_id, criterion_id, outcome, detail, source, evaluated_at)
                 VALUES (?1, ?2, 'pass', 'live-gate-detail', 'agent', 1)",
                rusqlite::params![fx.node.as_bytes().to_vec(), criterion],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO interview_transcripts (id, node_id, phase, display_name, body, created_at, updated_at)
                 VALUES (?1, ?2, 'requirements', 'T', 'secret transcript body', 1, 1)",
                rusqlite::params![transcript_id.as_bytes().to_vec(), fx.node.as_bytes().to_vec()],
            )
            .unwrap();
            format!("{}:{}", hex(fx.node.as_bytes()), hex(&criterion))
        };
        fx.fleet.reload_if_stale().unwrap();

        // Hand-write a small node journey: an AgentTurn referencing the
        // conversation's two turns, and DataChanged for the obligation.
        {
            let mut writer = JourneyWriter::open(&journeys_dir, JourneyKey::Node(fx.node)).unwrap();
            writer.append(
                Actor::Agent {
                    conversation: conversation_id,
                },
                Event::AgentTurn {
                    conversation: conversation_id,
                    phase: TurnPhase::Started { user_seq: 1 },
                },
            );
            writer.append(
                Actor::Agent {
                    conversation: conversation_id,
                },
                Event::AgentTurn {
                    conversation: conversation_id,
                    phase: TurnPhase::Replied { seq: 2 },
                },
            );
            writer.append(
                Actor::App,
                Event::DataChanged {
                    rows: vec![RowRef {
                        table: "node_obligations".into(),
                        row_id: obligation_id.to_string(),
                        op: "insert".into(),
                        old_state: None,
                        new_state: Some(format!("{{\"id\":\"{obligation_id}\"}}")),
                    }],
                },
            );
            writer.append(
                Actor::App,
                Event::DataChanged {
                    rows: vec![
                        RowRef {
                            table: "node_gate_evaluations".into(),
                            row_id: gate_row_id.clone(),
                            op: "insert".into(),
                            old_state: None,
                            new_state: None,
                        },
                        RowRef {
                            table: "interview_transcripts".into(),
                            row_id: hex(transcript_id.as_bytes()),
                            op: "insert".into(),
                            old_state: None,
                            new_state: None,
                        },
                    ],
                },
            );
            // A row that gets deleted before the cutoff: resolves as missing.
            let deleted_id = Uuid::new_v4();
            writer.append(
                Actor::App,
                Event::DataChanged {
                    rows: vec![RowRef {
                        table: "node_obligations".into(),
                        row_id: deleted_id.to_string(),
                        op: "insert".into(),
                        old_state: None,
                        new_state: Some("{}".into()),
                    }],
                },
            );
            let cutoff = writer.append(
                Actor::App,
                Event::DataChanged {
                    rows: vec![RowRef {
                        table: "node_obligations".into(),
                        row_id: deleted_id.to_string(),
                        op: "delete".into(),
                        old_state: Some("{}".into()),
                        new_state: None,
                    }],
                },
            );

            // A record past the cutoff must not appear in the bundle.
            writer.append(
                Actor::App,
                Event::Milestone {
                    state: "should-not-appear".into(),
                },
            );

            build_and_check(&fx, &journeys_dir, cutoff, true);
            build_and_check(&fx, &journeys_dir, cutoff, false);
        }
    }

    fn build_and_check(fx: &crate::interview::test_support::Fixture, journeys_dir: &std::path::Path, cutoff: u64, include_transcripts: bool) {
        let process_dir = std::env::temp_dir().join(format!("tod-bundle-process-{}", Uuid::new_v4()));
        let media_dir = std::env::temp_dir().join(format!("tod-bundle-media-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&process_dir).unwrap();
        std::fs::create_dir_all(&media_dir).unwrap();
        let install = install_paths(&process_dir);
        let media = media_paths(&media_dir);

        let mut settings = TodSettings::default();
        settings.journeys.include_transcripts = include_transcripts;

        let entry = fx
            .fleet
            .queue_journey_submission(Uuid::new_v4(), Some(fx.node), cutoff as i64, "report")
            .unwrap();

        let bytes = build_bundle(
            &fx.fleet,
            journeys_dir,
            &fx.root,
            &install,
            &media,
            &fx.root.join("tod-cli"),
            &settings,
            &entry,
        )
        .unwrap();

        let records = read_bundle(&bytes);

        // No record past the cutoff seq made it into the bundle (bundle-only
        // records like Manifest/Settings/Resolved are stamped with their own
        // fresh 1.. sequence by BundleWriter, so this checks only the
        // streamed journey records, identifiable by not being bundle-only
        // event kinds).
        for record in &records {
            if let Event::Milestone { state } = &record.event {
                assert_ne!(state, "should-not-appear", "record past the cutoff leaked into the bundle");
            }
        }

        let manifest = records
            .iter()
            .find_map(|r| match &r.event {
                Event::Manifest { manifest } => Some(manifest.clone()),
                _ => None,
            })
            .expect("bundle has a manifest record");
        assert_eq!(manifest.node_id, Some(fx.node));
        assert_eq!(manifest.bundle, entry.bundle_id, "manifest names the queued submission");
        assert_eq!(manifest.queued_seq, cutoff);
        assert_eq!(manifest.transcripts_included, include_transcripts);
        assert!(!manifest.cli_build_stamp.is_empty());

        assert!(
            records.iter().any(|r| matches!(r.event, Event::Settings { .. })),
            "bundle has a settings record"
        );

        let resolved: Vec<_> = records
            .iter()
            .filter_map(|r| match &r.event {
                Event::Resolved { reference, content } => Some((reference.clone(), content.clone())),
                _ => None,
            })
            .collect();

        let turns_res = resolved
            .iter()
            .find(|(r, _)| r.kind == "conversation-turns")
            .expect("a conversation-turns resolution exists");
        if include_transcripts {
            assert!(matches!(turns_res.1, Resolution::Found { .. }));
        } else {
            assert!(matches!(turns_res.1, Resolution::Withheld { .. }));
        }

        let row_res = resolved
            .iter()
            .filter(|(r, _)| r.kind == "row")
            .collect::<Vec<_>>();
        assert!(
            row_res
                .iter()
                .any(|(_, c)| matches!(c, Resolution::Found { .. })),
            "the live obligation row resolves as found"
        );
        assert!(
            row_res.iter().any(|(_, c)| matches!(c, Resolution::Missing)),
            "the deleted row resolves as missing"
        );

        let text_of = |c: &Resolution| match c {
            Resolution::Found { data } => Some(String::from_utf8_lossy(&data.bytes).into_owned()),
            _ => None,
        };
        let gate = row_res
            .iter()
            .find(|(r, _)| r.id.starts_with("node_gate_evaluations:"))
            .expect("gate evaluation reference");
        let gate_text = text_of(&gate.1).expect("gate evaluation found");
        assert!(gate_text.contains("live-gate-detail"), "{gate_text}");
        let obligation = row_res
            .iter()
            .find(|(r, c)| r.id.starts_with("node_obligations:") && text_of(c).is_some())
            .expect("live obligation row");
        assert!(text_of(&obligation.1).unwrap().contains("Passwords are hashed."));
        let transcript = row_res
            .iter()
            .find(|(r, _)| r.id.starts_with("interview_transcripts:"))
            .expect("transcript row reference");
        if include_transcripts {
            assert!(text_of(&transcript.1).unwrap().contains("secret transcript body"));
        } else {
            assert!(matches!(transcript.1, Resolution::Withheld { .. }));
        }
    }
}
