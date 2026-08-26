use super::provider::{AgentRunState, RunId};
use crate::interview::settings::AnswerProcessorSettings;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// Live pool counts for the workspace status footer (req 11a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnswerProcessorPoolStats {
    pub active: u32,
    pub in_pool: u32,
    pub max: u32,
}

#[derive(Debug, Clone)]
struct QueuedJob {
    run_id: RunId,
    prompt: String,
}

#[derive(Debug)]
struct PoolSlot {
    slot_id: u32,
    /// Responses received and processed on this ACP session.
    responses_received: u32,
    busy: bool,
}

#[derive(Debug)]
struct InterviewPool {
    settings: AnswerProcessorSettings,
    slots: Vec<PoolSlot>,
    queue: VecDeque<QueuedJob>,
    runs: HashMap<RunId, AgentRunState>,
    next_slot_id: u32,
}

/// Result of assigning an answer-processor job to the pool.
#[derive(Debug, Clone)]
pub enum AnswerSubmitAssignment {
    /// Run immediately on the given slot.
    Dispatch { slot_id: u32, prompt: String },
    /// All pool sessions busy at capacity — queued until a slot frees.
    Queued { prompt: String },
}

/// Outcome after an answer-processor response is processed.
#[derive(Debug, Clone)]
pub struct CompleteRunOutcome {
    /// Newly dispatched queued jobs.
    pub dispatched: Vec<(u32, RunId, String)>,
    /// Slot closed after its Nth response (recycle on response, not submit).
    pub recycled_slot_id: Option<u32>,
}

impl InterviewPool {
    fn stats(&self) -> AnswerProcessorPoolStats {
        let active = self.slots.iter().filter(|s| s.busy).count() as u32;
        AnswerProcessorPoolStats {
            active,
            in_pool: self.slots.len() as u32,
            max: self.settings.session_pool_size,
        }
    }

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

    /// Register a new answer-processor run.
    fn submit(&mut self, run_id: RunId, prompt: String) -> Result<AnswerSubmitAssignment, String> {
        if let Some(idx) = self.find_idle_slot() {
            self.runs.insert(run_id, AgentRunState::InFlight);
            self.slots[idx].busy = true;
            let slot_id = self.slots[idx].slot_id;
            return Ok(AnswerSubmitAssignment::Dispatch { slot_id, prompt });
        }

        if self.slots.len() < self.settings.session_pool_size as usize {
            let slot_id = self.create_slot();
            let idx = self.slots.len() - 1;
            self.runs.insert(run_id, AgentRunState::InFlight);
            self.slots[idx].busy = true;
            return Ok(AnswerSubmitAssignment::Dispatch { slot_id, prompt });
        }

        self.runs.insert(run_id, AgentRunState::InFlight);
        self.queue.push_back(QueuedJob {
            run_id,
            prompt: prompt.clone(),
        });
        Ok(AnswerSubmitAssignment::Queued { prompt })
    }

    /// After a response completes: mark slot idle, recycle if at limit, drain queue.
    fn on_slot_response(
        &mut self,
        slot_id: u32,
        run_id: RunId,
        result: Result<String, String>,
    ) -> CompleteRunOutcome {
        let Some(slot_idx) = self.slot_index(slot_id) else {
            return CompleteRunOutcome {
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
            slot.responses_received >= self.settings.answers_per_session
        };

        let recycled_slot_id = if recycle {
            self.slots.remove(slot_idx);
            Some(slot_id)
        } else {
            None
        };

        CompleteRunOutcome {
            dispatched: self.drain_queue(),
            recycled_slot_id,
        }
    }

    /// Assign queued jobs to idle slots (or new slots while under cap).
    fn drain_queue(&mut self) -> Vec<(u32, RunId, String)> {
        let mut dispatched = Vec::new();
        while self.queue.front().is_some() {
            let slot_idx = if let Some(idx) = self.find_idle_slot() {
                idx
            } else if self.slots.len() < self.settings.session_pool_size as usize {
                let id = self.create_slot();
                self.slot_index(id).expect("just created")
            } else {
                break;
            };

            let job = self.queue.pop_front().expect("front exists");
            self.slots[slot_idx].busy = true;
            let slot_id = self.slots[slot_idx].slot_id;
            self.runs.insert(job.run_id, AgentRunState::InFlight);
            dispatched.push((slot_id, job.run_id, job.prompt));
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
}

/// Per-interview-entity session pool keyed by canonical cwd.
#[derive(Debug, Default)]
pub struct AnswerProcessorPoolManager {
    pools: HashMap<PathBuf, InterviewPool>,
}

impl AnswerProcessorPoolManager {
    pub fn stats(
        &self,
        cwd: &Path,
        settings: &AnswerProcessorSettings,
    ) -> AnswerProcessorPoolStats {
        let key = normalize_cwd(cwd);
        self.pools
            .get(&key)
            .map(InterviewPool::stats)
            .unwrap_or(AnswerProcessorPoolStats {
                active: 0,
                in_pool: 0,
                max: settings.session_pool_size,
            })
    }

    pub fn poll_run(&mut self, cwd: &Path, id: RunId) -> Option<AgentRunState> {
        let key = normalize_cwd(cwd);
        self.pools.get_mut(&key)?.poll_run(id)
    }

    pub fn cancel_run(&mut self, cwd: &Path, id: RunId) {
        let key = normalize_cwd(cwd);
        if let Some(pool) = self.pools.get_mut(&key) {
            pool.cancel_run(id);
        }
    }

    /// Register a new answer-processor run.
    pub fn submit(
        &mut self,
        cwd: PathBuf,
        settings: AnswerProcessorSettings,
        prompt: String,
    ) -> Result<(AnswerSubmitAssignment, RunId), String> {
        let key = normalize_cwd(&cwd);
        let pool = self.pools.entry(key).or_insert_with(|| InterviewPool {
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
        cwd: &Path,
        slot_id: u32,
        run_id: RunId,
        result: Result<String, String>,
    ) -> CompleteRunOutcome {
        let key = normalize_cwd(cwd);
        let Some(pool) = self.pools.get_mut(&key) else {
            return CompleteRunOutcome {
                dispatched: Vec::new(),
                recycled_slot_id: None,
            };
        };
        pool.on_slot_response(slot_id, run_id, result)
    }
}

fn normalize_cwd(cwd: &Path) -> PathBuf {
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_idle_slot_before_opening_new() {
        let mut mgr = AnswerProcessorPoolManager::default();
        let cwd = PathBuf::from("/tmp/interview-a");
        let settings = AnswerProcessorSettings {
            session_pool_size: 4,
            answers_per_session: 4,
        };

        let (a0, r0) = mgr
            .submit(cwd.clone(), settings.clone(), "p0".into())
            .unwrap();
        let slot0 = match a0 {
            AnswerSubmitAssignment::Dispatch { slot_id, .. } => slot_id,
            _ => panic!("expected dispatch"),
        };

        mgr.complete_run(&cwd, slot0, r0, Ok("ok".into()));

        let (a1, _r1) = mgr
            .submit(cwd.clone(), settings.clone(), "p1".into())
            .unwrap();
        assert!(matches!(
            a1,
            AnswerSubmitAssignment::Dispatch { slot_id, .. } if slot_id == slot0
        ));
        assert_eq!(mgr.stats(&cwd, &settings).in_pool, 1);
    }

    #[test]
    fn opens_second_slot_when_first_busy() {
        let mut mgr = AnswerProcessorPoolManager::default();
        let cwd = PathBuf::from("/tmp/interview-b");
        let settings = AnswerProcessorSettings {
            session_pool_size: 4,
            answers_per_session: 4,
        };

        let (a0, _r0) = mgr
            .submit(cwd.clone(), settings.clone(), "p0".into())
            .unwrap();
        let slot0 = match a0 {
            AnswerSubmitAssignment::Dispatch { slot_id, .. } => slot_id,
            _ => panic!("expected dispatch"),
        };

        let (a1, _r1) = mgr
            .submit(cwd.clone(), settings.clone(), "p1".into())
            .unwrap();
        assert!(matches!(
            a1,
            AnswerSubmitAssignment::Dispatch { slot_id, .. } if slot_id != slot0
        ));
        assert_eq!(mgr.stats(&cwd, &settings).active, 2);
        assert_eq!(mgr.stats(&cwd, &settings).in_pool, 2);
    }

    #[test]
    fn recycles_after_nth_response() {
        let mut mgr = AnswerProcessorPoolManager::default();
        let cwd = PathBuf::from("/tmp/interview-c");
        let settings = AnswerProcessorSettings {
            session_pool_size: 4,
            answers_per_session: 2,
        };

        let (a0, r0) = mgr
            .submit(cwd.clone(), settings.clone(), "p0".into())
            .unwrap();
        let slot0 = match a0 {
            AnswerSubmitAssignment::Dispatch { slot_id, .. } => slot_id,
            _ => panic!("expected dispatch"),
        };
        mgr.complete_run(&cwd, slot0, r0, Ok("ok".into()));

        let (_, r1) = mgr
            .submit(cwd.clone(), settings.clone(), "p1".into())
            .unwrap();
        let outcome = mgr.complete_run(&cwd, slot0, r1, Ok("ok".into()));
        assert_eq!(outcome.recycled_slot_id, Some(slot0));
        assert_eq!(mgr.stats(&cwd, &settings).in_pool, 0);

        let (a2, _) = mgr
            .submit(cwd.clone(), settings.clone(), "p2".into())
            .unwrap();
        assert!(matches!(a2, AnswerSubmitAssignment::Dispatch { .. }));
        assert_eq!(mgr.stats(&cwd, &settings).in_pool, 1);
    }

    #[test]
    fn queues_when_pool_full_and_all_busy() {
        let mut mgr = AnswerProcessorPoolManager::default();
        let cwd = PathBuf::from("/tmp/interview-d");
        let settings = AnswerProcessorSettings {
            session_pool_size: 2,
            answers_per_session: 4,
        };

        let (_, _r0) = mgr
            .submit(cwd.clone(), settings.clone(), "p0".into())
            .unwrap();
        let (_, _r1) = mgr
            .submit(cwd.clone(), settings.clone(), "p1".into())
            .unwrap();
        let (a2, r2) = mgr
            .submit(cwd.clone(), settings.clone(), "p2".into())
            .unwrap();
        assert!(matches!(a2, AnswerSubmitAssignment::Queued { .. }));
        assert!(matches!(
            mgr.poll_run(&cwd, r2),
            Some(AgentRunState::InFlight)
        ));
    }
}
