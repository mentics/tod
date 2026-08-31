//! Memory-only queued and in-flight prompt state (not persisted on reload).

use crate::fleet::runtime::PromptDeliveryState;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct MemoryPromptQueue {
    queued: Mutex<HashMap<String, Vec<String>>>,
    in_flight: Mutex<HashMap<String, usize>>,
}

impl MemoryPromptQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&self, agent_id: impl Into<String>, content: impl Into<String>) {
        self.queued
            .lock()
            .expect("prompt queue mutex")
            .entry(agent_id.into())
            .or_default()
            .push(content.into());
    }

    pub fn mark_sent(&self, agent_id: &str) {
        let mut queued = self.queued.lock().expect("prompt queue mutex");
        if let Some(items) = queued.get_mut(agent_id) {
            if !items.is_empty() {
                items.remove(0);
            }
        }
        *self
            .in_flight
            .lock()
            .expect("prompt queue mutex")
            .entry(agent_id.to_string())
            .or_insert(0) += 1;
    }

    pub fn complete_in_flight(&self, agent_id: &str) {
        let mut in_flight = self.in_flight.lock().expect("prompt queue mutex");
        if let Some(count) = in_flight.get_mut(agent_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(agent_id);
            }
        }
    }

    pub fn clear(&self) {
        self.queued.lock().expect("prompt queue mutex").clear();
        self.in_flight.lock().expect("prompt queue mutex").clear();
    }
}

impl PromptDeliveryState for MemoryPromptQueue {
    fn queued_count(&self, agent_id: &str) -> usize {
        self.queued
            .lock()
            .expect("prompt queue mutex")
            .get(agent_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    fn in_flight_count(&self, agent_id: &str) -> usize {
        self.in_flight
            .lock()
            .expect("prompt queue mutex")
            .get(agent_id)
            .copied()
            .unwrap_or(0)
    }

    fn total_queued(&self) -> usize {
        self.queued
            .lock()
            .expect("prompt queue mutex")
            .values()
            .map(|v| v.len())
            .sum()
    }

    fn total_in_flight(&self) -> usize {
        self.in_flight
            .lock()
            .expect("prompt queue mutex")
            .values()
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_prompts_absent_after_clear_simulating_reload() {
        let queue = MemoryPromptQueue::new();
        queue.enqueue("a1", "hello");
        assert_eq!(queue.total_queued(), 1);
        queue.clear();
        assert_eq!(queue.total_queued(), 0);
        assert_eq!(queue.queued_count("a1"), 0);
    }

    #[test]
    fn in_flight_tracks_sent_prompts() {
        let queue = MemoryPromptQueue::new();
        queue.enqueue("a1", "one");
        queue.mark_sent("a1");
        assert_eq!(queue.queued_count("a1"), 0);
        assert_eq!(queue.in_flight_count("a1"), 1);
        queue.complete_in_flight("a1");
        assert_eq!(queue.total_in_flight(), 0);
    }
}
