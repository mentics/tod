//! The incoming-changes queue: recorded changes a node inherits and has not
//! yet been checked against (`doc/conversation/incoming-changes.md` §2, §4).
//!
//! Entries are written in the same transaction as the action they point at
//! ([`fan_out`], called from the one place an action row is inserted) and
//! cancelled when that action is reversed ([`cancel`]). The fleet writer
//! signals its commit notify on every commit, so views that listen to it see
//! queue changes like any other store change.

use crate::conversation::{ConversationRepo, Entity, EntitySnapshot, NetOp};
use crate::outline::KIND_CONSTRAINT;
use crate::outline::references::subtree_node_ids;
use crate::outline::types::Capability;
use crate::outline::uuid_blob::{blob_to_uuid, blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use uuid::Uuid;

/// Lifecycle states at or after `ready`: a node in one of these has built
/// (or committed to building) on what it inherits.
pub const COMMITTED_STATES: [&str; 9] = [
    "ready",
    "active",
    "verifying",
    "review",
    "approved",
    "merged",
    "released",
    "learn",
    "done",
];

/// How a change reached a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Ancestor,
    Reference,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Ancestor => "ancestor",
            Via::Reference => "reference",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "ancestor" => Ok(Via::Ancestor),
            "reference" => Ok(Via::Reference),
            other => anyhow::bail!("unknown incoming via {other:?}"),
        }
    }
}

/// One queued entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingEntry {
    pub node_id: Uuid,
    pub action_id: i64,
    pub via: Via,
    pub source_node: Uuid,
    pub queued_at: i64,
}

/// A node's pending changes to one item, netted over its entries.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingChange {
    pub entity: Entity,
    pub entity_id: Uuid,
    /// `Added`, `Edited`, or `Deleted`.
    pub op: NetOp,
    /// The item before the first pending action.
    pub before: Option<EntitySnapshot>,
    /// The item after the latest pending action.
    pub after: Option<EntitySnapshot>,
    pub via: Via,
    /// The ancestor or component that changed (the latest entry's).
    pub source_node: Uuid,
    /// The entries this change nets, oldest first.
    pub action_ids: Vec<i64>,
}

/// Queue action `action_id` for every node it could affect. Only
/// constraint-kind obligations on a Spec node fan out (added, reworded,
/// re-kinded to or from constraint, deleted), to each strict Spec
/// descendant in `ready` or later.
pub(crate) fn fan_out(
    conn: &Connection,
    action_id: i64,
    entity: Entity,
    before: Option<&EntitySnapshot>,
    after: Option<&EntitySnapshot>,
    now: i64,
) -> Result<()> {
    if entity != Entity::Obligation {
        return Ok(());
    }
    let parts = |s: Option<&EntitySnapshot>| match s {
        Some(EntitySnapshot::Obligation {
            node_id,
            kind,
            body,
            ..
        }) => Some((*node_id, kind.clone(), body.clone())),
        _ => None,
    };
    let (b, a) = (parts(before), parts(after));
    if b == a {
        // Moves within the node, section or phase changes: not inherited.
        return Ok(());
    }
    let mut sources = Vec::new();
    for (node, kind, _) in [&b, &a].into_iter().flatten() {
        if kind == KIND_CONSTRAINT && !sources.contains(node) {
            sources.push(*node);
        }
    }
    for source in sources {
        if !has_spec(conn, source)? {
            continue;
        }
        for target in subtree_node_ids(conn, source)? {
            if target == source || !committed_spec(conn, target)? {
                continue;
            }
            conn.execute(
                "INSERT OR IGNORE INTO incoming_changes
                 (node_id, action_id, via, source_node, queued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    uuid_to_blob(target),
                    action_id,
                    Via::Ancestor.as_str(),
                    uuid_to_blob(source),
                    now
                ],
            )?;
        }
    }
    Ok(())
}

/// Drop every entry for `action_id` (it was reversed).
pub(crate) fn cancel(conn: &Connection, action_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM incoming_changes WHERE action_id = ?1",
        params![action_id],
    )?;
    Ok(())
}

fn has_spec(conn: &Connection, node: Uuid) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM node_capabilities WHERE node_id = ?1 AND capability = ?2")?
        .exists(params![uuid_to_blob(node), Capability::Spec.as_str()])?)
}

fn committed_spec(conn: &Connection, node: Uuid) -> Result<bool> {
    if !has_spec(conn, node)? {
        return Ok(false);
    }
    let state: Option<String> = conn
        .prepare("SELECT state FROM node_lifecycle WHERE node_id = ?1")?
        .query_map(params![uuid_to_blob(node)], |r| r.get(0))?
        .next()
        .transpose()?;
    Ok(state.is_some_and(|s| COMMITTED_STATES.contains(&s.as_str())))
}

/// Nothing of the node's own work is affected: it stays where it is.
pub const AFFECTS_NONE: &str = "none";
/// The obligations still hold, some plan steps don't: back to `planning`.
pub const AFFECTS_PLAN: &str = "plan";
/// An obligation must be added, changed, or removed: back to `design`.
pub const AFFECTS_OBLIGATIONS: &str = "obligations";
/// Every verdict `--affects` accepts.
pub const AFFECTS: [&str; 3] = [AFFECTS_NONE, AFFECTS_PLAN, AFFECTS_OBLIGATIONS];

/// Where a verdict sends the node back to, when it sends it back at all.
pub fn affects_target(affects: &str) -> Option<&'static str> {
    match affects {
        AFFECTS_PLAN => Some("planning"),
        AFFECTS_OBLIGATIONS => Some("design"),
        _ => None,
    }
}

/// Append-only: every verdict an evaluation recorded. A node's latest row is
/// what `tod_core::lifecycle_validity` reads; the rest is history.
pub const CREATE_VERDICTS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS incoming_verdicts (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        affects         TEXT NOT NULL CHECK (affects IN ('none','plan','obligations')),
        note            TEXT NOT NULL,
        action_ids      TEXT NOT NULL,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS incoming_verdicts_node ON incoming_verdicts(node_id, id);
";

/// One evaluation's verdict on a node (`doc/conversation/incoming-changes.md` §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingVerdict {
    pub id: i64,
    pub node_id: Uuid,
    /// `none`, `plan`, or `obligations`.
    pub affects: String,
    pub note: String,
    /// The recorded actions this verdict resolved.
    pub action_ids: Vec<i64>,
    /// The evaluation conversation that recorded it, when one did.
    pub conversation_id: Option<Uuid>,
    pub created_at: i64,
}

impl IncomingVerdict {
    /// The state this verdict sends the node back to, if any.
    pub fn target(&self) -> Option<&'static str> {
        affects_target(&self.affects)
    }
}

/// Read and write access to the queue.
pub struct IncomingRepo<'a> {
    conn: &'a Connection,
}

impl<'a> IncomingRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// A node's pending entries, oldest action first.
    pub fn pending(&self, node_id: Uuid) -> Result<Vec<IncomingEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, action_id, via, source_node, queued_at FROM incoming_changes
             WHERE node_id = ?1 ORDER BY action_id",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], |r| {
                let node: Vec<u8> = r.get(0)?;
                let source: Vec<u8> = r.get(3)?;
                Ok((
                    blob_to_uuid_sql(&node)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    blob_to_uuid_sql(&source)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(node_id, action_id, via, source_node, queued_at)| {
                Ok(IncomingEntry {
                    node_id,
                    action_id,
                    via: Via::parse(&via)?,
                    source_node,
                    queued_at,
                })
            })
            .collect()
    }

    /// Pending entry count for every node that has any.
    pub fn counts(&self) -> Result<HashMap<Uuid, usize>> {
        let mut stmt = self
            .conn
            .prepare("SELECT node_id, COUNT(*) FROM incoming_changes GROUP BY node_id")?;
        let rows = stmt
            .query_map([], |r| {
                let node: Vec<u8> = r.get(0)?;
                Ok((node, r.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(node, n)| Ok((blob_to_uuid(&node)?, n as usize)))
            .collect()
    }

    /// Clear every entry for a node. Returns how many were removed.
    pub fn clear(&self, node_id: Uuid) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM incoming_changes WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
        )?)
    }

    /// Record a verdict on `node_id` and resolve the entries it covers:
    /// `action_ids`, or every pending entry when `None`. The entries are
    /// removed and the node's baseline notes it has been checked against
    /// those actions. Fails when there is nothing to resolve.
    pub fn resolve(
        &self,
        node_id: Uuid,
        affects: &str,
        note: &str,
        action_ids: Option<&[i64]>,
        conversation_id: Option<Uuid>,
    ) -> Result<IncomingVerdict> {
        let affects = AFFECTS
            .iter()
            .find(|a| a.eq_ignore_ascii_case(affects.trim()))
            .copied()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown --affects `{affects}` (expected {})",
                    AFFECTS.join("|")
                )
            })?;
        let note = note.trim();
        anyhow::ensure!(!note.is_empty(), "--note is required: say why");
        let pending: Vec<i64> = self.pending(node_id)?.iter().map(|e| e.action_id).collect();
        let ids: Vec<i64> = match action_ids {
            Some(ids) => ids.iter().copied().filter(|id| pending.contains(id)).collect(),
            None => pending,
        };
        anyhow::ensure!(
            !ids.is_empty(),
            "node {node_id} has no pending incoming changes to resolve"
        );
        let now = crate::outline::uuid_blob::now_ms();
        self.conn.execute(
            "INSERT INTO incoming_verdicts
             (node_id, affects, note, action_ids, conversation_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid_to_blob(node_id),
                affects,
                note,
                serde_json::to_string(&ids)?,
                conversation_id.map(uuid_to_blob),
                now
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.remove(node_id, &ids)?;
        Ok(IncomingVerdict {
            id,
            node_id,
            affects: affects.to_string(),
            note: note.to_string(),
            action_ids: ids,
            conversation_id,
            created_at: now,
        })
    }

    /// Clear the node's entries without a verdict: for a node whose pending
    /// changes net to nothing. The baseline still records the actions as
    /// checked. Returns how many entries were removed.
    pub fn clear_checked(&self, node_id: Uuid) -> Result<usize> {
        let ids: Vec<i64> = self.pending(node_id)?.iter().map(|e| e.action_id).collect();
        self.remove(node_id, &ids)?;
        Ok(ids.len())
    }

    fn remove(&self, node_id: Uuid, ids: &[i64]) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "DELETE FROM incoming_changes WHERE node_id = ?1 AND action_id = ?2",
                params![uuid_to_blob(node_id), id],
            )?;
        }
        crate::lifecycle_baseline::BaselineRepo::new(self.conn).record_checked(node_id, ids)
    }

    /// Every verdict recorded on the node, oldest first.
    pub fn verdicts(&self, node_id: Uuid) -> Result<Vec<IncomingVerdict>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, affects, note, action_ids, conversation_id, created_at
             FROM incoming_verdicts WHERE node_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], |r| {
                let node: Vec<u8> = r.get(1)?;
                let conversation: Option<Vec<u8>> = r.get(5)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    blob_to_uuid_sql(&node)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    conversation.map(|c| blob_to_uuid_sql(&c)).transpose()?,
                    r.get::<_, i64>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(id, node_id, affects, note, ids, conversation_id, created_at)| {
                    Ok(IncomingVerdict {
                        id,
                        node_id,
                        affects,
                        note,
                        action_ids: serde_json::from_str(&ids)?,
                        conversation_id,
                        created_at,
                    })
                },
            )
            .collect()
    }

    /// The node's latest verdict.
    pub fn latest_verdict(&self, node_id: Uuid) -> Result<Option<IncomingVerdict>> {
        Ok(self.verdicts(node_id)?.pop())
    }

    /// The verdict a conversation recorded, if it recorded one.
    pub fn verdict_for_conversation(
        &self,
        conversation_id: Uuid,
    ) -> Result<Option<IncomingVerdict>> {
        let node: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT node_id FROM incoming_verdicts WHERE conversation_id = ?1
                 ORDER BY id DESC LIMIT 1",
                params![uuid_to_blob(conversation_id)],
                |r| r.get(0),
            )
            .optional()?;
        let Some(node) = node else { return Ok(None) };
        Ok(self
            .verdicts(blob_to_uuid(&node)?)?
            .into_iter()
            .rev()
            .find(|v| v.conversation_id == Some(conversation_id)))
    }

    /// The node's pending entries netted per item, in the order each item
    /// was first touched: created then deleted is nothing, several rewords
    /// are one (first before, latest after), and an item whose net after
    /// matches its before (in node, kind, and text) is nothing. Empty means
    /// the entries can be cleared without an agent.
    pub fn net_pending(&self, node_id: Uuid) -> Result<Vec<PendingChange>> {
        let entries = self.pending(node_id)?;
        let actions = ConversationRepo::new(self.conn);
        let mut order: Vec<(Entity, Uuid)> = Vec::new();
        let mut groups: HashMap<(Entity, Uuid), PendingChange> = HashMap::new();
        for entry in entries {
            let Some(action) = actions.action(entry.action_id)? else {
                continue;
            };
            let key = (action.entity, action.entity_id);
            match groups.get_mut(&key) {
                Some(change) => {
                    change.after = action.after;
                    change.via = entry.via;
                    change.source_node = entry.source_node;
                    change.action_ids.push(entry.action_id);
                }
                None => {
                    order.push(key);
                    groups.insert(
                        key,
                        PendingChange {
                            entity: action.entity,
                            entity_id: action.entity_id,
                            op: NetOp::Edited,
                            before: action.before,
                            after: action.after,
                            via: entry.via,
                            source_node: entry.source_node,
                            action_ids: vec![entry.action_id],
                        },
                    );
                }
            }
        }
        let mut out = Vec::new();
        for key in order {
            let mut change = groups.remove(&key).expect("grouped");
            change.op = match (&change.before, &change.after) {
                (None, None) => continue,
                (None, Some(_)) => NetOp::Added,
                (Some(_), None) => NetOp::Deleted,
                (Some(b), Some(a)) if same_inherited(b, a) => continue,
                (Some(_), Some(_)) => NetOp::Edited,
            };
            out.push(change);
        }
        Ok(out)
    }
}

/// Whether two snapshots agree on everything a descendant inherits.
fn same_inherited(a: &EntitySnapshot, b: &EntitySnapshot) -> bool {
    match (a, b) {
        (
            EntitySnapshot::Obligation {
                node_id: n1,
                kind: k1,
                body: b1,
                ..
            },
            EntitySnapshot::Obligation {
                node_id: n2,
                kind: k2,
                body: b2,
                ..
            },
        ) => n1 == n2 && k1 == k2 && b1 == b2,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests;
