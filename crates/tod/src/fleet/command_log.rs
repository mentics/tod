//! Linear in-memory command log for undo / history.

use crate::fleet::writer::FleetMutation;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

const MAX_ENTRIES: usize = 50;

#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub id: Uuid,
    pub label: String,
    pub created_at: i64,
    pub inverses: Vec<FleetMutation>,
}

#[derive(Debug)]
pub struct CommandLog {
    entries: Vec<CommandEntry>,
    recording_suppressed: bool,
    tx: broadcast::Sender<()>,
}

impl CommandLog {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            entries: Vec::new(),
            recording_suppressed: false,
            tx,
        }
    }

    pub fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new()))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }

    pub fn is_suppressed(&self) -> bool {
        self.recording_suppressed
    }

    pub fn set_suppressed(&mut self, suppressed: bool) {
        self.recording_suppressed = suppressed;
    }

    pub fn entries(&self) -> &[CommandEntry] {
        &self.entries
    }

    pub fn push(&mut self, label: String, inverses: Vec<FleetMutation>) {
        if inverses.is_empty() {
            return;
        }
        let entry = CommandEntry {
            id: Uuid::new_v4(),
            label,
            created_at: crate::outline::uuid_blob::now_ms(),
            inverses,
        };
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..excess);
        }
        let _ = self.tx.send(());
    }

    /// Pop and return the most recent entry.
    pub fn pop_last(&mut self) -> Option<CommandEntry> {
        let entry = self.entries.pop();
        if entry.is_some() {
            let _ = self.tx.send(());
        }
        entry
    }

    /// Pop entries from the tail through `entry_id` (inclusive), returning them newest-first.
    pub fn pop_through(&mut self, entry_id: Uuid) -> Vec<CommandEntry> {
        let Some(pos) = self.entries.iter().position(|e| e.id == entry_id) else {
            return Vec::new();
        };
        let drained: Vec<CommandEntry> = self.entries.drain(pos..).collect();
        if !drained.is_empty() {
            let _ = self.tx.send(());
        }
        drained.into_iter().rev().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::{CreatePosition, OutlineMutation, ReorderDirection};

    fn outline(m: OutlineMutation) -> FleetMutation {
        FleetMutation::Outline(m)
    }

    #[test]
    fn undo_through_pops_tail_inclusive() {
        let mut log = CommandLog::new();
        log.push("a".into(), vec![outline(OutlineMutation::UpdateNodeTitle {
            node_id: Uuid::new_v4(),
            title: "a".into(),
        })]);
        log.push("b".into(), vec![outline(OutlineMutation::UpdateNodeTitle {
            node_id: Uuid::new_v4(),
            title: "b".into(),
        })]);
        let middle_id = log.entries()[1].id;
        log.push("c".into(), vec![outline(OutlineMutation::UpdateNodeTitle {
            node_id: Uuid::new_v4(),
            title: "c".into(),
        })]);
        let popped = log.pop_through(middle_id);
        assert_eq!(popped.len(), 2);
        assert_eq!(popped[0].label, "c");
        assert_eq!(popped[1].label, "b");
        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.entries()[0].label, "a");
    }

    #[test]
    fn pop_last_returns_newest() {
        let mut log = CommandLog::new();
        log.push("first".into(), vec![outline(OutlineMutation::ReorderSibling {
            node_id: Uuid::new_v4(),
            direction: ReorderDirection::Up,
        })]);
        log.push("second".into(), vec![outline(OutlineMutation::ReorderSibling {
            node_id: Uuid::new_v4(),
            direction: ReorderDirection::Down,
        })]);
        let entry = log.pop_last().unwrap();
        assert_eq!(entry.label, "second");
        assert_eq!(log.entries().len(), 1);
    }
}
