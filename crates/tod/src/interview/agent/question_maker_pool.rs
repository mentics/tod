use super::provider::{AgentRunState, RunId};
use crate::interview::settings::QuestionMakerSettings;
use crate::process_bundle::InterviewAgentPrompt;
use std::collections::{HashMap, VecDeque};

/// Max concurrent question maker ACP sessions per interview workspace.
pub const RESEARCHER_SESSION_POOL_SIZE: u32 = 2;

#[derive(Debug, Clone)]
struct QueuedJob {
    run_id: RunId,
    prompt: InterviewAgentPrompt,
}

#[derive(Debug)]
struct PoolSlot {
    slot_id: u32,
    responses_received: u32,
    busy: bool,
}

#[derive(Debug)]
struct QuestionMakerPool {
    settings: QuestionMakerSettings,
    slots: Vec<PoolSlot>,
    queue: VecDeque<QueuedJob>,
    runs: HashMap<RunId, AgentRunState>,
    next_slot_id: u32,
}

/// Result of assigning a question maker job to the pool.
#[derive(Debug, Clone)]
pub enum QuestionMakerSubmitAssignment {
    Dispatch { slot_id: u32, prompt: String },
    Queued { _prompt: InterviewAgentPrompt },
}

/// Outcome after a question maker response is processed.
#[derive(Debug, Clone)]
pub struct QuestionMakerCompleteRunOutcome {
    pub dispatched: Vec<(u32, RunId, String)>,
    pub recycled_slot_id: Option<u32>,
}

impl QuestionMakerPool {
    fn find_idle_slot(&self) -> Option<usize> {
        self.slots.iter().position(|s| !s.busy)
    }

    fn slot_index(&self, slot_id: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.slot_id == slot_id)
    }

    fn create_slot(&mut self) -> u32 {
        let id = self.next_slot_id;
        self.next_slot_id += 1;
        self.slots.push(PoolSlot {
            slot_id: id,
            responses_received: 0,
            busy: false,
        });
        id
    }

    fn submit(
        &mut self,
        run_id: RunId,
        prompt: InterviewAgentPrompt,
    ) -> Result<QuestionMakerSubmitAssignment, String> {
        if let Some(idx) = self.find_idle_slot() {
            self.runs.insert(run_id, AgentRunState::InFlight);
            self.slots[idx].busy = true;
            let slot_id = self.slots[idx].slot_id;
            let responses_received = self.slots[idx].responses_received;
            return Ok(QuestionMakerSubmitAssignment::Dispatch {
                slot_id,
                prompt: prompt.for_slot(responses_received),
            });
        }

        if self.slots.len() < RESEARCHER_SESSION_POOL_SIZE as usize {
            let slot_id = self.create_slot();
            let idx = self.slots.len() - 1;
            self.runs.insert(run_id, AgentRunState::InFlight);
            self.slots[idx].busy = true;
            return Ok(QuestionMakerSubmitAssignment::Dispatch {
                slot_id,
                prompt: prompt.for_slot(0),
            });
        }

        self.runs.insert(run_id, AgentRunState::InFlight);
        self.queue.push_back(QueuedJob {
            run_id,
            prompt: prompt.clone(),
        });
        Ok(QuestionMakerSubmitAssignment::Queued { _prompt: prompt })
    }

    fn on_slot_response(
        &mut self,
        slot_id: u32,
        run_id: RunId,
        result: Result<String, String>,
    ) -> QuestionMakerCompleteRunOutcome {
        let Some(slot_idx) = self.slot_index(slot_id) else {
            return QuestionMakerCompleteRunOutcome {
                dispatched: Vec::new(),
                recycled_slot_id: None,
            };
        };

        let state = match &result {
            Ok(text) => AgentRunState::Success(Some(text.clone())),
            Err(message) => AgentRunState::Failure(message.clone()),
        };
        self.runs.insert(run_id, state);

        let recycle = {
            let slot = &mut self.slots[slot_idx];
            slot.busy = false;
            slot.responses_received += 1;
            slot.responses_received >= self.settings.runs_per_session
        };

        let recycled_slot_id = if recycle {
            self.slots.remove(slot_idx);
            Some(slot_id)
        } else {
            None
        };

        QuestionMakerCompleteRunOutcome {
            dispatched: self.drain_queue(),
            recycled_slot_id,
        }
    }

    fn drain_queue(&mut self) -> Vec<(u32, RunId, String)> {
        let mut dispatched = Vec::new();
        while self.queue.front().is_some() {
            let slot_idx = if let Some(idx) = self.find_idle_slot() {
                idx
            } else if self.slots.len() < RESEARCHER_SESSION_POOL_SIZE as usize {
                let id = self.create_slot();
                self.slot_index(id).expect("just created")
            } else {
                break;
            };

            let job = self.queue.pop_front().expect("front exists");
            self.slots[slot_idx].busy = true;
            let slot_id = self.slots[slot_idx].slot_id;
            let responses_received = self.slots[slot_idx].responses_received;
            self.runs.insert(job.run_id, AgentRunState::InFlight);
            dispatched.push((slot_id, job.run_id, job.prompt.for_slot(responses_received)));
        }
        dispatched
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.runs.get(&id).cloned()
    }

    fn cancel_run(&mut self, id: RunId) {
        self.runs.remove(&id);
        self.queue.retain(|j| j.run_id != id);
    }

    fn in_flight_count(&self) -> u32 {
        self.runs
            .values()
            .filter(|state| matches!(state, AgentRunState::InFlight))
            .count() as u32
    }
}

/// Per-interview-entity question maker session pool keyed by canonical cwd.
#[derive(Debug, Default)]
pub struct QuestionMakerPoolManager {
    pools: HashMap<String, QuestionMakerPool>,
}

impl QuestionMakerPoolManager {
    pub fn poll_run(&mut self, agent_config_id: &str, id: RunId) -> Option<AgentRunState> {
        self.pools.get_mut(agent_config_id)?.poll_run(id)
    }

    pub fn cancel_run(&mut self, agent_config_id: &str, id: RunId) {
        if let Some(pool) = self.pools.get_mut(agent_config_id) {
            pool.cancel_run(id);
        }
    }

    pub fn submit(
        &mut self,
        agent_config_id: String,
        settings: QuestionMakerSettings,
        prompt: InterviewAgentPrompt,
    ) -> Result<(QuestionMakerSubmitAssignment, RunId), String> {
        let pool = self.pools.entry(agent_config_id).or_insert_with(|| QuestionMakerPool {
            settings: settings.clone(),
            slots: Vec::new(),
            queue: VecDeque::new(),
            runs: HashMap::new(),
            next_slot_id: 0,
        });
        pool.settings = settings;
        let run_id = RunId::new();
        let assignment = pool.submit(run_id, prompt)?;
        Ok((assignment, run_id))
    }

    pub fn complete_run(
        &mut self,
        agent_config_id: &str,
        slot_id: u32,
        run_id: RunId,
        result: Result<String, String>,
    ) -> QuestionMakerCompleteRunOutcome {
        let Some(pool) = self.pools.get_mut(agent_config_id) else {
            return QuestionMakerCompleteRunOutcome {
                dispatched: Vec::new(),
                recycled_slot_id: None,
            };
        };
        pool.on_slot_response(slot_id, run_id, result)
    }

    pub fn in_flight_count(&self) -> u32 {
        self.pools
            .values()
            .map(QuestionMakerPool::in_flight_count)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_prompt(text: &str) -> InterviewAgentPrompt {
        InterviewAgentPrompt {
            session_prefix: String::new(),
            turn: text.into(),
        }
    }

    #[test]
    fn recycles_after_nth_response() {
        let mut mgr = QuestionMakerPoolManager::default();
        let agent = "qm-agent-a".to_string();
        let settings = QuestionMakerSettings {
            runs_per_session: 2,
            ..QuestionMakerSettings::default()
        };

        let (a0, r0) = mgr
            .submit(
                agent.clone(),
                settings.clone(),
                InterviewAgentPrompt {
                    session_prefix: "PREFIX".into(),
                    turn: "p0".into(),
                },
            )
            .unwrap();
        let slot0 = match a0 {
            QuestionMakerSubmitAssignment::Dispatch { slot_id, .. } => slot_id,
            _ => panic!("expected dispatch"),
        };
        mgr.complete_run(&agent, slot0, r0, Ok("ok".into()));

        let (_, r1) = mgr
            .submit(agent.clone(), settings.clone(), turn_prompt("p1"))
            .unwrap();
        let outcome = mgr.complete_run(&agent, slot0, r1, Ok("ok".into()));
        assert_eq!(outcome.recycled_slot_id, Some(slot0));
    }

    #[test]
    fn followup_omits_session_prefix() {
        let mut mgr = QuestionMakerPoolManager::default();
        let agent = "qm-agent-b".to_string();
        let settings = QuestionMakerSettings::default();
        let (a0, r0) = mgr
            .submit(
                agent.clone(),
                settings.clone(),
                InterviewAgentPrompt {
                    session_prefix: "PREFIX".into(),
                    turn: "TURN".into(),
                },
            )
            .unwrap();
        let slot0 = match a0 {
            QuestionMakerSubmitAssignment::Dispatch { slot_id, prompt } => {
                assert_eq!(prompt, "PREFIX\n\nTURN");
                slot_id
            }
            _ => panic!("expected dispatch"),
        };
        mgr.complete_run(&agent, slot0, r0, Ok("ok".into()));

        let (a1, _) = mgr
            .submit(
                agent,
                settings,
                InterviewAgentPrompt {
                    session_prefix: "PREFIX2".into(),
                    turn: "TURN2".into(),
                },
            )
            .unwrap();
        match a1 {
            QuestionMakerSubmitAssignment::Dispatch { prompt, .. } => {
                assert_eq!(prompt, "TURN2");
            }
            _ => panic!("expected dispatch"),
        }
    }
}
