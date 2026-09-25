//! Conversation rows, turns, actions, and flags.

use super::types::*;
use crate::outline::uuid_blob::{blob_to_uuid, blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use uuid::Uuid;

/// How many words of the first user turn a picker entry shows.
const OPENING_WORDS: usize = 8;

const CONVERSATION_COLUMNS: &str = "id, focus_kind, focus_id, focus_node_id, agent_session_id, \
     session_name, platform, model, effort, created_at, updated_at, protocol, agent_run_id, from_state, to_state";

const ACTION_COLUMNS: &str = "id, conversation_id, source, turn_seq, actor, kind, entity, entity_id, \
     node_id, mutation, before, after, archive_id, reverses, reversed_by, at";

pub struct ConversationRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ConversationRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create(
        &self,
        focus: Focus,
        protocol: ProtocolKind,
        platform: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Conversation> {
        self.create_with_id(Uuid::new_v4(), focus, protocol, platform, model, effort)
    }

    /// [`Self::create`] with a caller-chosen id.
    pub fn create_with_id(
        &self,
        id: Uuid,
        focus: Focus,
        protocol: ProtocolKind,
        platform: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Conversation> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO conversations
             (id, focus_kind, focus_id, focus_node_id, platform, model, effort, protocol,
              created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                uuid_to_blob(id),
                focus.kind_str(),
                focus.focus_id().map(uuid_to_blob),
                focus_node_column(focus).map(uuid_to_blob),
                platform,
                model,
                effort,
                protocol.as_str(),
                now
            ],
        )?;
        self.get(id)?.context("conversation vanished after insert")
    }

    /// Record the lifecycle transition a gate check or on-entry run is about.
    pub fn set_transition(&self, id: Uuid, from_state: &str, to_state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET from_state = ?2, to_state = ?3 WHERE id = ?1",
            params![uuid_to_blob(id), from_state, to_state],
        )?;
        Ok(())
    }

    /// The focus's most recently updated conversation running `protocol` —
    /// how a protocol with one conversation per focus (implementation) finds
    /// the one to reopen.
    pub fn latest_for_focus_with_protocol(
        &self,
        focus: Focus,
        protocol: ProtocolKind,
    ) -> Result<Option<Conversation>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {CONVERSATION_COLUMNS} FROM conversations
                     WHERE focus_kind = ?1 AND focus_id IS ?2 AND protocol = ?3
                     ORDER BY updated_at DESC, created_at DESC
                     LIMIT 1"
                ),
                params![
                    focus.kind_str(),
                    focus.focus_id().map(uuid_to_blob),
                    protocol.as_str()
                ],
                map_conversation,
            )
            .optional()?
            .transpose()
    }

    /// Store a report against the turn in progress — the latest turn in the
    /// transcript, since the agent's own turn is appended only when it ends —
    /// replacing any earlier one for that turn. The agent records it through
    /// `tod-cli` while it works.
    pub fn record_report(&self, conversation_id: Uuid, body: &serde_json::Value) -> Result<i64> {
        let turn_seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM conversation_turns WHERE conversation_id = ?1",
            params![uuid_to_blob(conversation_id)],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO conversation_reports (conversation_id, turn_seq, body)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (conversation_id, turn_seq) DO UPDATE SET body = excluded.body",
            params![uuid_to_blob(conversation_id), turn_seq, body.to_string()],
        )?;
        Ok(turn_seq)
    }

    /// The latest report recorded at or after `turn_seq`: what a turn that
    /// started there reported.
    pub fn report_since(
        &self,
        conversation_id: Uuid,
        turn_seq: i64,
    ) -> Result<Option<serde_json::Value>> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM conversation_reports
                 WHERE conversation_id = ?1 AND turn_seq >= ?2
                 ORDER BY turn_seq DESC LIMIT 1",
                params![uuid_to_blob(conversation_id), turn_seq],
                |row| row.get(0),
            )
            .optional()?;
        body.map(|body| Ok(serde_json::from_str(&body)?)).transpose()
    }

    /// The conversation's most recent report.
    pub fn latest_report(&self, conversation_id: Uuid) -> Result<Option<serde_json::Value>> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM conversation_reports
                 WHERE conversation_id = ?1 ORDER BY turn_seq DESC LIMIT 1",
                params![uuid_to_blob(conversation_id)],
                |row| row.get(0),
            )
            .optional()?;
        body.map(|body| Ok(serde_json::from_str(&body)?)).transpose()
    }

    /// Point a conversation at the fleet run its agent process belongs to.
    pub fn set_agent_run(&self, id: Uuid, agent_run_id: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET agent_run_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(id), agent_run_id, now_ms()],
        )?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Conversation>> {
        self.conn
            .query_row(
                &format!("SELECT {CONVERSATION_COLUMNS} FROM conversations WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_conversation,
            )
            .optional()?
            .transpose()
    }

    /// The focus's conversations, most recently updated first.
    pub fn list_for_focus(&self, focus: Focus) -> Result<Vec<ConversationSummary>> {
        let conversations = self.for_focus(focus, None)?;
        conversations
            .into_iter()
            .map(|conversation| {
                let change_count = super::project::net_changes(self.conn, conversation.id)?.len();
                let opening = self.opening(conversation.id)?;
                Ok(ConversationSummary {
                    conversation,
                    change_count,
                    opening,
                })
            })
            .collect()
    }

    /// The focus's most recently updated conversation.
    pub fn latest_for_focus(&self, focus: Focus) -> Result<Option<Conversation>> {
        Ok(self.for_focus(focus, Some(1))?.into_iter().next())
    }

    fn for_focus(&self, focus: Focus, limit: Option<i64>) -> Result<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CONVERSATION_COLUMNS} FROM conversations
             WHERE focus_kind = ?1 AND focus_id IS ?2
             ORDER BY updated_at DESC, created_at DESC
             LIMIT ?3"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    focus.kind_str(),
                    focus.focus_id().map(uuid_to_blob),
                    limit.unwrap_or(-1)
                ],
                map_conversation,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    fn opening(&self, conversation_id: Uuid) -> Result<String> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM conversation_turns
                 WHERE conversation_id = ?1 AND role = 'user' ORDER BY seq LIMIT 1",
                params![uuid_to_blob(conversation_id)],
                |row| row.get(0),
            )
            .optional()?;
        let body = body.unwrap_or_default();
        let words: Vec<&str> = body.split_whitespace().collect();
        let mut opening = words
            .iter()
            .take(OPENING_WORDS)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        if words.len() > OPENING_WORDS {
            opening.push('…');
        }
        Ok(opening)
    }

    pub fn turns(&self, conversation_id: Uuid) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, body, parts, sent_context, created_at FROM conversation_turns
             WHERE conversation_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(conversation_id)], read_turn_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(parse_turn_row).collect()
    }

    /// Turns in `[from_seq, to_seq]`, inclusive — the range a bundle exporter
    /// resolves a turn-range reference against.
    pub fn turns_range(
        &self,
        conversation_id: Uuid,
        from_seq: i64,
        to_seq: i64,
    ) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, body, parts, sent_context, created_at FROM conversation_turns
             WHERE conversation_id = ?1 AND seq BETWEEN ?2 AND ?3 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(
                params![uuid_to_blob(conversation_id), from_seq, to_seq],
                read_turn_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(parse_turn_row).collect()
    }

    pub fn append_turn(&self, conversation_id: Uuid, role: TurnRole, body: &str) -> Result<Turn> {
        self.append_turn_with_parts_and_context(conversation_id, role, body, &[], None)
    }

    /// [`Self::append_turn`] for an agent reply that came with its parts.
    pub fn append_turn_with_parts(
        &self,
        conversation_id: Uuid,
        role: TurnRole,
        body: &str,
        parts: &[ReplyPart],
    ) -> Result<Turn> {
        self.append_turn_with_parts_and_context(conversation_id, role, body, parts, None)
    }

    /// [`Self::append_turn_with_parts`], also recording the part of what was
    /// sent to the agent that is not the user's own text (the protocol delta
    /// prepended to a user turn). `None` for a continuation turn, whose body
    /// already equals what was sent, and for every non-user turn.
    pub fn append_turn_with_parts_and_context(
        &self,
        conversation_id: Uuid,
        role: TurnRole,
        body: &str,
        parts: &[ReplyPart],
        sent_context: Option<&str>,
    ) -> Result<Turn> {
        let now = now_ms();
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM conversation_turns WHERE conversation_id = ?1",
            params![uuid_to_blob(conversation_id)],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO conversation_turns
                (id, conversation_id, seq, role, body, parts, sent_context, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid_to_blob(Uuid::new_v4()),
                uuid_to_blob(conversation_id),
                seq,
                role.as_str(),
                body,
                (!parts.is_empty())
                    .then(|| serde_json::to_string(parts))
                    .transpose()?,
                sent_context,
                now
            ],
        )?;
        self.touch(conversation_id, now)?;
        Ok(Turn {
            seq,
            role,
            body: body.to_string(),
            parts: parts.to_vec(),
            sent_context: sent_context.map(str::to_string),
            created_at: now,
        })
    }

    /// Close every turn that was left waiting on an agent when the app last
    /// stopped: a conversation whose last entry is a user message or a
    /// continuation gets an error turn saying so. Run once at startup, before
    /// any turn can be in flight. Returns the conversations it closed.
    pub fn close_interrupted_turns(&self, body: &str) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.conversation_id FROM conversation_turns t
             WHERE t.role IN ('user', 'continuation')
               AND t.seq = (SELECT MAX(seq) FROM conversation_turns
                            WHERE conversation_id = t.conversation_id)",
        )?;
        let ids = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|blob| blob_to_uuid(&blob))
            .collect::<Result<Vec<_>>>()?;
        for id in &ids {
            self.append_turn(*id, TurnRole::Error, body)?;
        }
        Ok(ids)
    }

    /// Record the provider session the conversation now continues (`None`
    /// clears it, so the next message starts a fresh one).
    pub fn set_agent_session(
        &self,
        conversation_id: Uuid,
        agent_session_id: Option<&str>,
        session_name: Option<&str>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE conversations SET agent_session_id = ?1,
                    session_name = COALESCE(?2, session_name), updated_at = ?3
             WHERE id = ?4",
            params![
                agent_session_id,
                session_name,
                now_ms(),
                uuid_to_blob(conversation_id)
            ],
        )?;
        anyhow::ensure!(n == 1, "conversation {conversation_id} not found");
        Ok(())
    }

    /// Record the context the conversation's first turn sent.
    pub fn set_opening_context(&self, conversation_id: Uuid, context: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE conversations SET opening_context = ?1 WHERE id = ?2",
            params![context, uuid_to_blob(conversation_id)],
        )?;
        anyhow::ensure!(n == 1, "conversation {conversation_id} not found");
        Ok(())
    }

    /// The context the conversation's first turn sent; `None` before it was
    /// sent, and for conversations started before it was recorded.
    pub fn opening_context(&self, conversation_id: Uuid) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT opening_context FROM conversations WHERE id = ?1",
                params![uuid_to_blob(conversation_id)],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Whether the conversation's first turn recorded its context, without
    /// reading it.
    pub fn has_opening_context(&self, conversation_id: Uuid) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM conversations
                            WHERE id = ?1 AND opening_context IS NOT NULL)",
            params![uuid_to_blob(conversation_id)],
            |row| row.get(0),
        )?)
    }

    /// The seq of the latest user turn (0 before the first): the turn an
    /// agent's actions are attributed to.
    pub fn max_user_seq(&self, conversation_id: Uuid) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM conversation_turns
             WHERE conversation_id = ?1 AND role = 'user'",
            params![uuid_to_blob(conversation_id)],
            |row| row.get(0),
        )?)
    }

    pub(super) fn touch(&self, conversation_id: Uuid, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, uuid_to_blob(conversation_id)],
        )?;
        Ok(())
    }

    /// Every action in the conversation, oldest first.
    pub fn actions(&self, conversation_id: Uuid) -> Result<Vec<ActionRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ACTION_COLUMNS} FROM conversation_actions
             WHERE conversation_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(conversation_id)], RawAction::read)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(RawAction::parse).collect()
    }

    pub fn action(&self, id: i64) -> Result<Option<ActionRow>> {
        self.conn
            .query_row(
                &format!("SELECT {ACTION_COLUMNS} FROM conversation_actions WHERE id = ?1"),
                params![id],
                RawAction::read,
            )
            .optional()?
            .map(RawAction::parse)
            .transpose()
    }

    /// The most recent conversation whose recorded actions changed
    /// `entity_id` — what `E` on that item's row opens as its transcript.
    pub fn latest_conversation_for_entity(&self, entity_id: Uuid) -> Result<Option<Uuid>> {
        let blob: Option<Option<Vec<u8>>> = self
            .conn
            .query_row(
                "SELECT conversation_id FROM conversation_actions
                 WHERE entity_id = ?1 ORDER BY id DESC LIMIT 1",
                params![uuid_to_blob(entity_id)],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?;
        blob.flatten().map(|blob| blob_to_uuid_sql(&blob)).transpose().map_err(Into::into)
    }

    /// Unsure flags, keyed by item.
    pub fn flags(&self, conversation_id: Uuid) -> Result<HashMap<(Entity, Uuid), String>> {
        let mut stmt = self.conn.prepare(
            "SELECT entity, entity_id, reason FROM conversation_flags WHERE conversation_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(conversation_id)], |row| {
                let blob: Vec<u8> = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    blob_to_uuid_sql(&blob)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(entity, id, reason)| Ok(((Entity::parse(&entity)?, id), reason)))
            .collect()
    }
}

type RawTurnRow = (i64, String, String, Option<String>, Option<String>, i64);

fn read_turn_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawTurnRow> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, Option<String>>(3)?,
        row.get::<_, Option<String>>(4)?,
        row.get::<_, i64>(5)?,
    ))
}

fn parse_turn_row(row: RawTurnRow) -> Result<Turn> {
    let (seq, role, body, parts, sent_context, created_at) = row;
    // Parts are for display; a row that does not parse shows its body.
    let parts = parts
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    Ok(Turn {
        seq,
        role: TurnRole::parse(&role)?,
        body,
        parts,
        sent_context,
        created_at,
    })
}

/// `focus_node_id` is stored only for items that live on a node.
fn focus_node_column(focus: Focus) -> Option<Uuid> {
    match focus {
        Focus::Obligation { node, .. } | Focus::PlanStep { node, .. } => Some(node),
        Focus::Project | Focus::Node(_) => None,
    }
}

fn opt_uuid(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<Vec<u8>>>(index)?
        .map(|blob| blob_to_uuid_sql(&blob))
        .transpose()
}

fn map_conversation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Conversation>> {
    let id_blob: Vec<u8> = row.get(0)?;
    let id = blob_to_uuid_sql(&id_blob)?;
    let kind: String = row.get(1)?;
    let focus_id = opt_uuid(row, 2)?;
    let focus_node = opt_uuid(row, 3)?;
    let agent_session_id = row.get(4)?;
    let session_name = row.get(5)?;
    let platform = row.get(6)?;
    let model = row.get(7)?;
    let effort = row.get(8)?;
    let created_at = row.get(9)?;
    let updated_at = row.get(10)?;
    let protocol: String = row.get(11)?;
    let agent_run_id = row.get(12)?;
    let from_state = row.get(13)?;
    let to_state = row.get(14)?;
    Ok(Focus::from_columns(&kind, focus_id, focus_node).and_then(|focus| {
        Ok(Conversation {
            id,
            focus,
            protocol: ProtocolKind::parse(&protocol)?,
            agent_run_id,
            agent_session_id,
            session_name,
            platform,
            model,
            effort,
            from_state,
            to_state,
            created_at,
            updated_at,
        })
    }))
}

/// An action row as read, before its text columns are parsed.
struct RawAction {
    id: i64,
    conversation_id: Option<Uuid>,
    source: String,
    turn_seq: i64,
    actor: String,
    kind: String,
    entity: String,
    entity_id: Uuid,
    node_id: Option<Uuid>,
    mutation: String,
    before: Option<String>,
    after: Option<String>,
    archive_id: Option<Uuid>,
    reverses: Option<i64>,
    reversed_by: Option<i64>,
    at: i64,
}

impl RawAction {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let entity_id: Vec<u8> = row.get(7)?;
        Ok(Self {
            id: row.get(0)?,
            conversation_id: opt_uuid(row, 1)?,
            source: row.get(2)?,
            turn_seq: row.get(3)?,
            actor: row.get(4)?,
            kind: row.get(5)?,
            entity: row.get(6)?,
            entity_id: blob_to_uuid_sql(&entity_id)?,
            node_id: opt_uuid(row, 8)?,
            mutation: row.get(9)?,
            before: row.get(10)?,
            after: row.get(11)?,
            archive_id: opt_uuid(row, 12)?,
            reverses: row.get(13)?,
            reversed_by: row.get(14)?,
            at: row.get(15)?,
        })
    }

    fn parse(self) -> Result<ActionRow> {
        let snapshot = |raw: Option<String>| -> Result<Option<EntitySnapshot>> {
            raw.map(|s| serde_json::from_str(&s).context("bad action snapshot"))
                .transpose()
        };
        Ok(ActionRow {
            id: self.id,
            conversation_id: self.conversation_id,
            source: self.source,
            turn_seq: self.turn_seq,
            actor: ActionActor::parse(&self.actor)?,
            kind: ActionKind::parse(&self.kind)?,
            entity: Entity::parse(&self.entity)?,
            entity_id: self.entity_id,
            node_id: self.node_id,
            mutation: serde_json::from_str(&self.mutation).context("bad action mutation")?,
            before: snapshot(self.before)?,
            after: snapshot(self.after)?,
            archive_id: self.archive_id,
            reverses: self.reverses,
            reversed_by: self.reversed_by,
            at: self.at,
        })
    }
}
