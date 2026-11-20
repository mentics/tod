//! What each agent turn of a conversation spent, per turn, so the cost of a
//! node can be read by phase and the prompt cache's hit rate checked rather
//! than assumed (`doc/lifecycle/find-it-first.md` section 13, "Measure it";
//! `doc/lifecycle/cache-measurement.md`).
//!
//! One row per finished turn (a failed turn too: it spent tokens). The counts
//! are what the agent reported for that turn, never an estimate
//! (`tod_agent::TokenCounts`): `input` is the input tokens that were neither
//! read from nor written to the prompt cache, so a turn's prompt is
//! `input + cache_read + cache_write` and its cache-read share is
//! `cache_read / prompt`. The conversation's protocol and node come from its
//! own row (`conversations.protocol`, `focus_node_id`); `phase` is the
//! lifecycle phase the turn ran in (`None` for a chat) and `model` the model
//! that served it as far as the agent said.
//!
//! Not in `journey_changes`: a `conversation_*` table (see that module), and
//! bookkeeping about how a conversation ran, not a change to the node.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tod_agent::TokenCounts;
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS conversation_usage (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        at_ms           INTEGER NOT NULL,
        phase           TEXT,
        model           TEXT,
        requests        INTEGER NOT NULL DEFAULT 0,
        input           INTEGER NOT NULL DEFAULT 0,
        cache_read      INTEGER NOT NULL DEFAULT 0,
        cache_write     INTEGER NOT NULL DEFAULT 0,
        output          INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS conversation_usage_conversation
        ON conversation_usage(conversation_id, id);
";

/// One finished turn's spending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub conversation_id: Uuid,
    pub at_ms: i64,
    pub phase: Option<String>,
    pub model: Option<String>,
    pub counts: TokenCounts,
}

/// Token counts added up over some turns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub turns: u64,
    pub counts: TokenCounts,
}

impl UsageTotals {
    /// The share of the prompt tokens that were read from the cache, `None`
    /// when no prompt was recorded.
    pub fn cache_read_share(&self) -> Option<f64> {
        let prompt = self.counts.prompt();
        (prompt > 0).then(|| self.counts.cache_read as f64 / prompt as f64)
    }
}

/// A group of turns: a phase (`None`: turns outside any phase, a chat's) and
/// what they spent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseUsage {
    pub phase: Option<String>,
    pub totals: UsageTotals,
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Records one turn. A turn the agent reported nothing for is not recorded
/// (there is nothing to add up).
pub fn record(conn: &Connection, turn: &TurnUsage) -> Result<()> {
    let c = &turn.counts;
    if c.input + c.cache_read + c.cache_write + c.output == 0 {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO conversation_usage
             (conversation_id, at_ms, phase, model, requests, input, cache_read, cache_write, output)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            uuid_to_blob(turn.conversation_id),
            turn.at_ms,
            turn.phase,
            turn.model,
            to_i64(c.requests),
            to_i64(c.input),
            to_i64(c.cache_read),
            to_i64(c.cache_write),
            to_i64(c.output),
        ],
    )?;
    Ok(())
}

fn counts_from(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<TokenCounts> {
    let n = |i: usize| r.get::<_, i64>(at + i).map(|v| v.max(0) as u64);
    Ok(TokenCounts {
        requests: n(0)?,
        input: n(1)?,
        cache_read: n(2)?,
        cache_write: n(3)?,
        output: n(4)?,
        ..TokenCounts::default()
    })
}

fn totals_from(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<UsageTotals> {
    let turns = r.get::<_, i64>(at)?.max(0) as u64;
    Ok(UsageTotals { turns, counts: counts_from(r, at + 1)? })
}

const SUMS: &str = "COUNT(*), COALESCE(SUM(u.requests),0), COALESCE(SUM(u.input),0),
    COALESCE(SUM(u.cache_read),0), COALESCE(SUM(u.cache_write),0), COALESCE(SUM(u.output),0)";

/// Everything the conversations focused on `node` spent.
pub fn node_totals(conn: &Connection, node: Uuid) -> Result<UsageTotals> {
    let sql = format!(
        "SELECT {SUMS} FROM conversation_usage u
         JOIN conversations c ON c.id = u.conversation_id
         WHERE COALESCE(c.focus_node_id, c.focus_id) = ?1"
    );
    Ok(conn.query_row(&sql, params![uuid_to_blob(node)], |r| totals_from(r, 0))?)
}

/// What `node` spent, by the phase each turn ran in, in the order the phases
/// were first used.
pub fn phase_totals(conn: &Connection, node: Uuid) -> Result<Vec<PhaseUsage>> {
    let sql = format!(
        "SELECT u.phase, {SUMS} FROM conversation_usage u
         JOIN conversations c ON c.id = u.conversation_id
         WHERE COALESCE(c.focus_node_id, c.focus_id) = ?1
         GROUP BY u.phase ORDER BY MIN(u.id)"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], |r| {
        Ok(PhaseUsage { phase: r.get(0)?, totals: totals_from(r, 1)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What one conversation spent.
pub fn conversation_totals(conn: &Connection, conversation: Uuid) -> Result<UsageTotals> {
    let sql = format!("SELECT {SUMS} FROM conversation_usage u WHERE u.conversation_id = ?1");
    Ok(conn.query_row(&sql, params![uuid_to_blob(conversation)], |r| totals_from(r, 0))?)
}

/// The turns of `node`, oldest first, each with its conversation's protocol
/// (for the cache measurement's per-session reading).
pub fn node_turns(conn: &Connection, node: Uuid) -> Result<Vec<(String, TurnUsage)>> {
    let mut stmt = conn.prepare(
        "SELECT c.protocol, u.conversation_id, u.at_ms, u.phase, u.model,
                u.requests, u.input, u.cache_read, u.cache_write, u.output
         FROM conversation_usage u JOIN conversations c ON c.id = u.conversation_id
         WHERE COALESCE(c.focus_node_id, c.focus_id) = ?1 ORDER BY u.id",
    )?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], |r| {
        let conversation: Vec<u8> = r.get(1)?;
        Ok((
            r.get::<_, String>(0)?,
            TurnUsage {
                conversation_id: blob_to_uuid_sql(&conversation)?,
                at_ms: r.get(2)?,
                phase: r.get(3)?,
                model: r.get(4)?,
                counts: counts_from(r, 5)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What turns cost at list prices (`tod_agent::price`): the money, and how
/// many of the turns could not be priced because their model is unknown (so
/// the money is a floor).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spent {
    pub turns: u64,
    pub unpriced_turns: u64,
    pub micros: u64,
}

impl Spent {
    pub fn add_turn(&mut self, micros: Option<u64>) {
        self.turns += 1;
        match micros {
            Some(m) => self.micros += m,
            None => self.unpriced_turns += 1,
        }
    }

    pub fn add(&mut self, other: &Self) {
        self.turns += other.turns;
        self.unpriced_turns += other.unpriced_turns;
        self.micros += other.micros;
    }

    /// Nothing was recorded (no turn reported tokens).
    pub fn is_empty(&self) -> bool {
        self.turns == 0
    }

    /// `$1.23`, `$1.23+` when some turns could not be priced, `unpriced`
    /// when none could; `None` when nothing was recorded.
    pub fn label(&self) -> Option<String> {
        if self.is_empty() {
            None
        } else if self.unpriced_turns == self.turns {
            Some("unpriced".to_string())
        } else {
            let plus = if self.unpriced_turns > 0 { "+" } else { "" };
            Some(format!("{}{plus}", tod_agent::format_usd(self.micros)))
        }
    }
}

/// `SELECT model, sums` grouped by model: pricing is linear in the counts, so
/// a group priced as one is the sum of its turns priced one by one.
fn spent_by_model(conn: &Connection, where_clause: &str, arg: Vec<u8>) -> Result<Spent> {
    let sql = format!(
        "SELECT u.model, {SUMS} FROM conversation_usage u
         JOIN conversations c ON c.id = u.conversation_id
         WHERE {where_clause} GROUP BY u.model"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![arg], |r| Ok((r.get::<_, Option<String>>(0)?, totals_from(r, 1)?)))?;
    let mut spent = Spent::default();
    for row in rows {
        let (model, totals) = row?;
        let micros = tod_agent::price(&totals.counts, model.as_deref()).map(|c| c.micros);
        match micros {
            Some(m) => {
                spent.turns += totals.turns;
                spent.micros += m;
            }
            None => {
                spent.turns += totals.turns;
                spent.unpriced_turns += totals.turns;
            }
        }
    }
    Ok(spent)
}

/// What everything focused on `node` cost, every phase together.
pub fn node_cost(conn: &Connection, node: Uuid) -> Result<Spent> {
    spent_by_model(conn, "COALESCE(c.focus_node_id, c.focus_id) = ?1", uuid_to_blob(node))
}

/// What one conversation cost.
pub fn conversation_cost(conn: &Connection, conversation: Uuid) -> Result<Spent> {
    spent_by_model(conn, "c.id = ?1", uuid_to_blob(conversation))
}

/// Each turn of a conversation that reported tokens: when it was recorded
/// and what it cost (`None`: its model is unknown), oldest first.
pub fn conversation_turn_costs(conn: &Connection, conversation: Uuid) -> Result<Vec<(i64, Option<u64>)>> {
    let mut stmt = conn.prepare(
        "SELECT u.at_ms, u.model, u.requests, u.input, u.cache_read, u.cache_write, u.output
         FROM conversation_usage u WHERE u.conversation_id = ?1 ORDER BY u.id",
    )?;
    let rows = stmt.query_map(params![uuid_to_blob(conversation)], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, counts_from(r, 2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (at, model, counts) = row?;
        out.push((at, tod_agent::price(&counts, model.as_deref()).map(|c| c.micros)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (id BLOB PRIMARY KEY, focus_id BLOB, focus_node_id BLOB, protocol TEXT);",
        )
        .unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        conn
    }

    fn conversation(conn: &Connection, node: Uuid, protocol: &str) -> Uuid {
        let id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO conversations (id, focus_node_id, protocol) VALUES (?1, ?2, ?3)",
            params![uuid_to_blob(id), uuid_to_blob(node), protocol],
        )
        .unwrap();
        id
    }

    fn turn(conversation: Uuid, phase: Option<&str>, input: u64, read: u64, write: u64, output: u64) -> TurnUsage {
        TurnUsage {
            conversation_id: conversation,
            at_ms: 1,
            phase: phase.map(str::to_string),
            model: Some("claude-sonnet-5".into()),
            counts: TokenCounts { requests: 1, input, cache_read: read, cache_write: write, output, ..Default::default() },
        }
    }

    #[test]
    fn totals_add_up_per_node_and_per_phase() {
        let conn = conn();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let (ca, cb) = (conversation(&conn, a, "implementation"), conversation(&conn, b, "review"));
        record(&conn, &turn(ca, Some("implement"), 10, 0, 1000, 50)).unwrap();
        record(&conn, &turn(ca, Some("implement"), 5, 900, 100, 40)).unwrap();
        record(&conn, &turn(ca, Some("review"), 1, 0, 0, 1)).unwrap();
        record(&conn, &turn(cb, Some("review"), 7, 7, 7, 7)).unwrap();

        let total = node_totals(&conn, a).unwrap();
        assert_eq!(total.turns, 3);
        assert_eq!(total.counts.cache_read, 900);
        assert_eq!(total.counts.prompt(), 10 + 1000 + 5 + 900 + 100 + 1);

        let phases = phase_totals(&conn, a).unwrap();
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].phase.as_deref(), Some("implement"));
        assert_eq!(phases[0].totals.turns, 2);
        let share = phases[0].totals.cache_read_share().unwrap();
        assert!((share - 900.0 / 2015.0).abs() < 1e-9, "{share}");
        assert_eq!(node_totals(&conn, b).unwrap().counts.output, 7);
        let turns = node_turns(&conn, a).unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].0, "implementation");
        assert_eq!(turns[0].1.conversation_id, ca);
        assert_eq!(conversation_totals(&conn, ca).unwrap().turns, 3);
    }

    #[test]
    fn a_turn_that_reported_nothing_is_not_recorded_and_an_empty_node_totals_zero() {
        let conn = conn();
        let node = Uuid::new_v4();
        let c = conversation(&conn, node, "chat");
        record(&conn, &turn(c, None, 0, 0, 0, 0)).unwrap();
        let totals = node_totals(&conn, node).unwrap();
        assert_eq!(totals, UsageTotals::default());
        assert_eq!(totals.cache_read_share(), None);
        assert!(phase_totals(&conn, node).unwrap().is_empty());
    }
    #[test]
    fn cost_is_priced_per_model_and_a_node_totals_its_conversations() {
        let conn = conn();
        let node = Uuid::new_v4();
        let (a, b) = (conversation(&conn, node, "implementation"), conversation(&conn, node, "review"));
        let mut t = turn(a, Some("implement"), 1_000_000, 0, 0, 100_000);
        t.model = Some("claude-sonnet-5-5".into()); // $2 in, $10 out
        record(&conn, &t).unwrap();
        let mut t = turn(b, Some("review"), 1_000_000, 0, 0, 0);
        t.model = Some("opus".into()); // $4 in
        record(&conn, &t).unwrap();
        // A model nobody priced.
        let mut t = turn(b, Some("review"), 5, 0, 0, 5);
        t.model = Some("mystery".into());
        record(&conn, &t).unwrap();

        let a_cost = conversation_cost(&conn, a).unwrap();
        assert_eq!((a_cost.micros, a_cost.unpriced_turns), (3_000_000, 0));
        assert_eq!(a_cost.label().as_deref(), Some("$3.00"));
        let all = node_cost(&conn, node).unwrap();
        assert_eq!((all.turns, all.micros, all.unpriced_turns), (3, 7_000_000, 1));
        assert_eq!(all.label().as_deref(), Some("$7.00+"));
        let turns = conversation_turn_costs(&conn, b).unwrap();
        assert_eq!(turns.iter().map(|t| t.1).collect::<Vec<_>>(), vec![Some(4_000_000), None]);
        assert_eq!(node_cost(&conn, Uuid::new_v4()).unwrap().label(), None);
    }
}
