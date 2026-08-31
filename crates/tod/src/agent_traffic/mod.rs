//! In-memory log of raw agent requests and responses for troubleshooting.

use chrono::Utc;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

const DEFAULT_MAX_ENTRIES: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentCategory {
    Fleet,
    QuestionMaker,
    AnswerProcessor,
    DeepDive,
}

impl AgentCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fleet => "Fleet",
            Self::QuestionMaker => "Question maker",
            Self::AnswerProcessor => "Answer",
            Self::DeepDive => "Deep dive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficDirection {
    Request,
    Response,
}

impl TrafficDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrafficEntry {
    pub sequence: u64,
    pub timestamp_ms: i64,
    pub category: AgentCategory,
    pub agent_id: String,
    pub agent_label: String,
    pub direction: TrafficDirection,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSummary {
    pub id: String,
    pub label: String,
    pub category: AgentCategory,
    pub entry_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterviewAgentCounts {
    pub question_maker_in_flight: u32,
    pub answer_active: u32,
    pub answer_pool: u32,
    pub answer_max: u32,
    pub deep_dive_in_flight: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetAgentCounts {
    pub total: u32,
    pub processing: u32,
    pub blocked: u32,
}

#[derive(Debug, Clone, Default)]
pub struct AgentStatusGroups {
    pub interview: InterviewAgentCounts,
    pub fleet: FleetAgentCounts,
    pub traffic_entries: usize,
}

pub struct AgentTrafficLog {
    entries: Vec<TrafficEntry>,
    next_sequence: u64,
    max_entries: usize,
}

impl AgentTrafficLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_sequence: 1,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }

    pub fn record(
        &mut self,
        category: AgentCategory,
        agent_id: impl Into<String>,
        agent_label: impl Into<String>,
        direction: TrafficDirection,
        content: impl Into<String>,
    ) {
        let entry = TrafficEntry {
            sequence: self.next_sequence,
            timestamp_ms: Utc::now().timestamp_millis(),
            category,
            agent_id: agent_id.into(),
            agent_label: agent_label.into(),
            direction,
            content: content.into(),
        };
        self.next_sequence += 1;
        self.entries.push(entry);
        if self.entries.len() > self.max_entries {
            let drop = self.entries.len() - self.max_entries;
            self.entries.drain(0..drop);
        }
    }

    pub fn record_fleet_request(&mut self, agent_id: &str, content: &str) {
        self.record(
            AgentCategory::Fleet,
            agent_id,
            agent_id,
            TrafficDirection::Request,
            content,
        );
    }

    pub fn record_fleet_response(&mut self, agent_id: &str, content: &str) {
        self.record(
            AgentCategory::Fleet,
            agent_id,
            agent_id,
            TrafficDirection::Response,
            content,
        );
    }

    pub fn entries(&self) -> &[TrafficEntry] {
        &self.entries
    }

    pub fn entries_for_agent(&self, agent_id: &str) -> Vec<TrafficEntry> {
        self.entries
            .iter()
            .filter(|e| e.agent_id == agent_id)
            .cloned()
            .collect()
    }

    pub fn agent_summaries(&self) -> Vec<AgentSummary> {
        let mut counts: HashMap<(AgentCategory, String), (String, usize)> = HashMap::new();
        for entry in &self.entries {
            let key = (entry.category, entry.agent_id.clone());
            counts
                .entry(key)
                .and_modify(|(_, n)| *n += 1)
                .or_insert((entry.agent_label.clone(), 1));
        }
        let mut summaries: Vec<_> = counts
            .into_iter()
            .map(|((category, id), (label, entry_count))| AgentSummary {
                id,
                label,
                category,
                entry_count,
            })
            .collect();
        summaries.sort_by(|a, b| {
            a.category
                .cmp(&b.category)
                .then_with(|| a.label.cmp(&b.label))
        });
        summaries
    }

    pub fn agents_grouped(&self) -> BTreeMap<AgentCategory, Vec<AgentSummary>> {
        let mut grouped: BTreeMap<AgentCategory, Vec<AgentSummary>> = BTreeMap::new();
        for summary in self.agent_summaries() {
            grouped.entry(summary.category).or_default().push(summary);
        }
        grouped
    }
}

impl Default for AgentTrafficLog {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedAgentTrafficLog = Arc<Mutex<AgentTrafficLog>>;

pub fn shared_log() -> SharedAgentTrafficLog {
    Arc::new(Mutex::new(AgentTrafficLog::new()))
}

pub fn format_status_bar(groups: &AgentStatusGroups) -> String {
    let mut parts = Vec::new();

    if groups.interview.question_maker_in_flight > 0 {
        parts.push(format!(
            "Question maker {}",
            groups.interview.question_maker_in_flight
        ));
    }
    if groups.interview.answer_active > 0
        || groups.interview.answer_pool > 0
        || groups.interview.answer_max > 0
    {
        parts.push(format!(
            "Answer {}/{}",
            groups.interview.answer_active, groups.interview.answer_max
        ));
    }
    if groups.interview.deep_dive_in_flight > 0 {
        parts.push(format!(
            "Deep dive {}",
            groups.interview.deep_dive_in_flight
        ));
    }
    if groups.fleet.total > 0 {
        let mut fleet = format!("Fleet {}", groups.fleet.total);
        if groups.fleet.processing > 0 {
            fleet.push_str(&format!(" ({} proc)", groups.fleet.processing));
        } else if groups.fleet.blocked > 0 {
            fleet.push_str(&format!(" ({} blocked)", groups.fleet.blocked));
        }
        parts.push(fleet);
    }

    if parts.is_empty() {
        if groups.traffic_entries > 0 {
            format!("Agents · {} logged turns", groups.traffic_entries)
        } else {
            "Agents · idle".to_string()
        }
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_agents_by_category() {
        let mut log = AgentTrafficLog::new();
        log.record(
            AgentCategory::QuestionMaker,
            "run-1",
            "question-maker",
            TrafficDirection::Request,
            "hello",
        );
        log.record(
            AgentCategory::Fleet,
            "alpha-1",
            "alpha-1",
            TrafficDirection::Request,
            "prompt",
        );
        let grouped = log.agents_grouped();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[&AgentCategory::QuestionMaker].len(), 1);
        assert_eq!(grouped[&AgentCategory::Fleet].len(), 1);
    }

    #[test]
    fn status_bar_shows_grouped_counts() {
        let text = format_status_bar(&AgentStatusGroups {
            interview: InterviewAgentCounts {
                question_maker_in_flight: 1,
                answer_active: 2,
                answer_pool: 2,
                answer_max: 5,
                deep_dive_in_flight: 0,
            },
            fleet: FleetAgentCounts {
                total: 3,
                processing: 1,
                blocked: 0,
            },
            traffic_entries: 10,
        });
        assert!(text.contains("Question maker 1"));
        assert!(text.contains("Answer 2/5"));
        assert!(text.contains("Fleet 3"));
    }
}
