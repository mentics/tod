//! The app-wide registry of agent runs.
//!
//! Every conversation driver (implement, verify, review, fix, gate check,
//! on-entry, freeform) is hosted here instead of inside [`ConversationView`]
//! (`crate::conversation::ConversationView`), so any view can ask which
//! agents are running on a given node, under which protocol, and — for a
//! lifecycle-flavored run — whether it is entering or leaving a state. One
//! [`AgentRuns`] is created in `app::window::open` and shared as an
//! `Entity<AgentRuns>`, cloned into each view that needs it, the same way
//! `views::lifecycle_control::LifecycleController` is shared.
//!
//! Starting a turn and collecting a finished one run git, Docker, and
//! `tod-cli`, which can take seconds — see
//! `crate::conversation::driver_slot::DriverSlot`. Callers take a slot's
//! driver to the background executor and put it back; [`AgentRuns`] only
//! ever holds the slots, never blocks the UI thread itself.

use crate::conversation::driver_slot::DriverSlot;
use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use anyhow::Context as _;
use gpui::Context;
use std::sync::Arc;
use tod_core::conversation::implement::{HandoffAnswer, handoff_answer_message};
use tod_core::conversation::{ConversationConfig, ConversationDriver, ConversationStatus, SharedAgentAccess};
use tod_store::conversation::{Conversation, ConversationRepo, Focus, ProtocolKind};
use tod_store::decisions::{Decision, DecisionRepo};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::PlanStepRepo;
use tod_store::outline::repos::plan_steps::STATUS_IN_PROGRESS;
use tod_store::review::ReviewRepo;
use uuid::Uuid;

/// The answer a decision gets when the user settled it outside the app.
pub const RESOLVED_ELSEWHERE: &str = "Resolved outside the app, in the agent's own session";

/// The conversation that most recently handed a plan step back on `focus`:
/// whichever of its implementation/verification conversations was updated
/// last (there is at most one of each per focus — `Protocol::cwd` — so "most
/// recent" picks the one whose agent produced the handoff).
pub fn latest_handoff_conversation(
    conn: &rusqlite::Connection,
    focus: Focus,
) -> anyhow::Result<Option<Conversation>> {
    let repo = ConversationRepo::new(conn);
    let implement = repo.latest_for_focus_with_protocol(focus, ProtocolKind::Implementation)?;
    let verify = repo.latest_for_focus_with_protocol(focus, ProtocolKind::Verification)?;
    Ok(match (implement, verify) {
        (Some(a), Some(b)) => Some(if a.updated_at >= b.updated_at { a } else { b }),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    })
}

/// One node's runs, as a status label (`state` / `state →` / `→ state`, W11)
/// reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRun {
    pub conversation_id: Option<Uuid>,
    pub protocol: ProtocolKind,
    pub running: bool,
    /// Set once the run's conversation exists, for a gate check or on-entry
    /// run only: the state it started in and the one it is about (equal for
    /// on-entry — `to_state` is the state entered).
    pub from_state: Option<String>,
    pub to_state: Option<String>,
}

impl NodeRun {
    /// Running an on-entry turn: entering `to_state`.
    pub fn entering(&self) -> bool {
        self.running && self.protocol == ProtocolKind::OnEntry
    }

    /// Running the state's own work or its evaluation (or, from before
    /// gates were app checks, a gate check): working toward leaving
    /// `from_state`.
    pub fn leaving(&self) -> bool {
        self.running
            && matches!(
                self.protocol,
                ProtocolKind::Phase | ProtocolKind::Evaluate | ProtocolKind::GateCheck
            )
    }
}

/// Hosts every conversation driver in the app.
pub struct AgentRuns {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    /// Drivers with work in flight, plus whichever idle ones a view last
    /// looked at. Idle drivers for a conversation nobody is showing are
    /// dropped by [`AgentRuns::retain`]: everything they know is already in
    /// the database.
    slots: Vec<DriverSlot>,
    /// The next [`DriverSlot::id`].
    next_slot: u64,
}

/// Drop the provider's live agent sessions `keys`. The conversations keep
/// their recorded agent session ids: the next message starts the agent again
/// and resumes by id, and when the agent cannot (a recreated sandbox has no
/// session log) the driver starts a fresh session seeded with a snapshot
/// (`ConversationDriver`'s cold-resume rotation). Takes the agent lock:
/// call it off the UI thread.
pub fn close_sessions(agent: &SharedAgent, keys: &[String]) {
    let mut access = SharedAgentAccess(agent);
    for key in keys {
        tod_core::conversation::AgentAccess::with(&mut access, |a| a.close_session(key));
    }
}

impl AgentRuns {
    pub fn new(fleet: Arc<FleetStore>, agent: SharedAgent) -> Self {
        Self {
            fleet,
            agent,
            slots: Vec::new(),
            next_slot: 0,
        }
    }

    pub fn fleet(&self) -> &Arc<FleetStore> {
        &self.fleet
    }

    pub fn agent(&self) -> &SharedAgent {
        &self.agent
    }

    fn is_match(d: &DriverSlot, focus: Focus, protocol: ProtocolKind, conversation_id: Option<Uuid>) -> bool {
        match conversation_id {
            Some(id) => d.conversation_id == Some(id),
            None => d.conversation_id.is_none() && d.focus == focus && d.protocol == protocol,
        }
    }

    /// The slot showing `focus`/`protocol`/`conversation_id`, when there is
    /// one here already.
    pub fn find_index(
        &self,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Option<Uuid>,
    ) -> Option<usize> {
        self.slots
            .iter()
            .position(|d| Self::is_match(d, focus, protocol, conversation_id))
    }

    pub fn slot_by_index(&self, ix: usize) -> Option<&DriverSlot> {
        self.slots.get(ix)
    }

    pub fn slot_by_id(&self, id: u64) -> Option<&DriverSlot> {
        self.slots.iter().find(|s| s.id == id)
    }

    pub fn status_at(&self, ix: usize) -> Option<ConversationStatus> {
        self.slots.get(ix).map(|s| s.status.clone())
    }

    /// Whether a conversation running `protocol` on `focus` is working now,
    /// whichever conversation a caller has open.
    pub fn protocol_running(&self, focus: Focus, protocol: ProtocolKind) -> bool {
        self.slots
            .iter()
            .any(|d| d.focus == focus && d.protocol == protocol && d.status.running)
    }

    /// The conversation id of the (first) slot running `protocol` on
    /// `focus`, when one is working now; `Some(None)` for an unsaved one.
    pub fn running_conversation(&self, focus: Focus, protocol: ProtocolKind) -> Option<Option<Uuid>> {
        self.slots
            .iter()
            .find(|d| d.focus == focus && d.protocol == protocol && d.status.running)
            .map(|d| d.conversation_id)
    }

    /// The slot for `focus`/`protocol`/`conversation_id`, starting a fresh
    /// [`DriverSlot`] from `make` when there is none yet. Returns its index.
    pub fn ensure(
        &mut self,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Option<Uuid>,
        make: impl FnOnce() -> Result<ConversationDriver, String>,
    ) -> Result<usize, String> {
        if let Some(ix) = self.find_index(focus, protocol, conversation_id) {
            return Ok(ix);
        }
        let driver = make()?;
        self.next_slot += 1;
        self.slots.push(DriverSlot::new(self.next_slot, driver));
        Ok(self.slots.len() - 1)
    }

    /// Take the driver at `ix` to send a message: `None` when it is working
    /// already (or away).
    pub fn take_to_send(&mut self, ix: usize) -> Option<(u64, ConversationDriver)> {
        let slot = self.slots.get_mut(ix)?;
        let driver = slot.take_to_send()?;
        Some((slot.id, driver))
    }

    /// Every slot with a turn in flight, taken to be ticked off the main
    /// thread; [`AgentRuns::put_back`] returns each one.
    pub fn take_running(&mut self) -> Vec<(u64, ConversationDriver)> {
        self.slots
            .iter_mut()
            .filter_map(|slot| slot.take_to_tick().map(|driver| (slot.id, driver)))
            .collect()
    }

    /// Mark the slot `id` to stop as soon as its driver is back.
    pub fn set_cancel(&mut self, id: u64) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.cancel = true;
        }
    }

    /// Take, and clear, the slot `id`'s cancel flag.
    pub fn take_cancel(&mut self, id: u64) -> bool {
        self.slots
            .iter_mut()
            .find(|s| s.id == id)
            .is_some_and(|s| std::mem::take(&mut s.cancel))
    }

    /// The driver is back: put it back in its slot.
    pub fn put_back(&mut self, id: u64, driver: ConversationDriver) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.put_back(driver);
        }
    }

    /// The slot `id`'s driver, when it is here (not away on the background
    /// executor).
    pub fn driver_mut(&mut self, id: u64) -> Option<&mut ConversationDriver> {
        self.slots.iter_mut().find(|s| s.id == id).and_then(|s| s.driver_mut())
    }

    pub fn set_status(&mut self, id: u64, status: ConversationStatus) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.status = status;
        }
    }

    /// Show `conversation_id`, which another owner drives (a node's runner),
    /// as running here: every view that asks what runs on its node sees it,
    /// and nothing here sends to it or ticks it until
    /// [`Self::release_elsewhere`]. An idle slot this registry held for the
    /// conversation is dropped (the database has everything it knew); a
    /// working one is left alone and `None` returned.
    pub fn host_elsewhere(
        &mut self,
        focus: Focus,
        protocol: ProtocolKind,
        conversation_id: Uuid,
        status: ConversationStatus,
    ) -> Option<u64> {
        if let Some(ix) = self.slots.iter().position(|s| s.conversation_id == Some(conversation_id)) {
            if self.slots[ix].status.running {
                return None;
            }
            self.slots.remove(ix);
        }
        self.next_slot += 1;
        self.slots.push(DriverSlot::hosted_elsewhere(
            self.next_slot,
            focus,
            protocol,
            conversation_id,
            status,
        ));
        Some(self.next_slot)
    }

    /// The status a slot [`Self::host_elsewhere`] made shows; `false` when it
    /// is gone (dropped by a view's [`Self::retain`]), so the owner hosts it
    /// again.
    pub fn update_elsewhere(&mut self, id: u64, status: ConversationStatus) -> bool {
        match self.slots.iter_mut().find(|s| s.id == id && s.is_hosted_elsewhere()) {
            Some(slot) => {
                slot.status = status;
                true
            }
            None => false,
        }
    }

    /// Drop a slot [`Self::host_elsewhere`] made.
    pub fn release_elsewhere(&mut self, id: u64) {
        self.slots.retain(|s| s.id != id);
    }

    /// Whether a lifecycle conversation this registry drives is working on
    /// `node` (not counting ones hosted for another owner). Chat-panel
    /// conversations are separate from the lifecycle processor and never
    /// count.
    pub fn driving_on_node(&self, node: Uuid) -> bool {
        self.slots.iter().any(|s| {
            s.focus == Focus::Node(node)
                && s.status.running
                && !s.is_hosted_elsewhere()
                && !matches!(s.protocol, ProtocolKind::Outline | ProtocolKind::Chat)
        })
    }

    /// Drop every slot `keep` says no to; used to drop idle slots for a
    /// conversation nobody is showing.
    pub fn retain(&mut self, keep: impl FnMut(&DriverSlot) -> bool) {
        self.slots.retain(keep);
    }

    /// What a running slot is doing, in words the user can place: the kind of
    /// conversation and the node (or project) it is about, never its id.
    fn describe_slot(&self, slot: &DriverSlot) -> String {
        let kind = crate::conversation::kind_label(slot.protocol);
        let node = match slot.focus {
            Focus::Project => None,
            Focus::Node(node) | Focus::Obligation { node, .. } | Focus::PlanStep { node, .. } => Some(node),
        };
        // `get_node`, not `get_task`: the latter only finds nodes with the
        // Agent capability, so a plain node would read as deleted.
        let found = node.map(|node| self.fleet.get_node(&node.to_string()).ok().flatten());
        let title = found
            .clone()
            .flatten()
            .map(|t| t.title)
            .filter(|t| !t.trim().is_empty());
        match (title, found) {
            (Some(title), _) => format!("Agent running ({kind}): {title}"),
            (None, Some(Some(_))) => format!("Agent running ({kind}): an untitled node"),
            (None, Some(None)) => format!("Agent running ({kind}): a node that is no longer in the outline"),
            (None, None) => format!("Agent running ({kind}): the whole project"),
        }
    }

    /// Work in flight, for the close-window warning.
    pub fn running_work(&self) -> Vec<String> {
        let (mut affected, continuing) = self.running_work_split();
        affected.extend(continuing);
        affected
    }

    /// Work in flight, split for the close-window warning: what closing the
    /// app would cut off, and what the resident daemon keeps running (a
    /// conversation a node's runner drives there).
    pub fn running_work_split(&self) -> (Vec<String>, Vec<String>) {
        let daemon = self.fleet.is_client();
        let (mut affected, mut continuing) = (Vec::new(), Vec::new());
        for d in self.slots.iter().filter(|d| d.status.running) {
            let what = self.describe_slot(d);
            if daemon && d.is_hosted_elsewhere() {
                continuing.push(what);
            } else {
                affected.push(what);
            }
        }
        (affected, continuing)
    }

    /// Every run on `node`: what the status label (`state` / `state →` /
    /// `→ state`, W11) reads. Reads each matching slot's conversation row for
    /// the transition a gate check or on-entry turn is about.
    pub fn runs_for_node(&self, node: Uuid) -> Vec<NodeRun> {
        self.slots
            .iter()
            .filter(|s| s.focus == Focus::Node(node))
            .map(|s| {
                let (from_state, to_state) = s
                    .conversation_id
                    .and_then(|id| {
                        self.fleet
                            .read(|conn| ConversationRepo::new(conn).get(id))
                            .ok()
                            .flatten()
                    })
                    .map(|c| (c.from_state, c.to_state))
                    .unwrap_or((None, None));
                NodeRun {
                    conversation_id: s.conversation_id,
                    protocol: s.protocol,
                    running: s.status.running,
                    from_state,
                    to_state,
                }
            })
            .collect()
    }

    /// The agent session keys of every conversation this registry holds on
    /// `node`, for [`close_sessions`]: their live agent processes ran in a
    /// sandbox that was deleted and made again.
    pub fn node_session_keys(&self, node: Uuid) -> Vec<String> {
        self.slots
            .iter()
            .filter(|s| s.focus == Focus::Node(node))
            .filter_map(|s| s.conversation_id)
            .map(ConversationDriver::session_key)
            .collect()
    }

    /// Every run currently in flight, across every focus (for a future
    /// "agents running" indicator).
    pub fn running(&self) -> impl Iterator<Item = &DriverSlot> {
        self.slots.iter().filter(|s| s.status.running)
    }

    /// Nodes with a lifecycle-processor conversation (anything but a chat or
    /// the outline) running right now.
    pub fn running_lifecycle_nodes(&self) -> std::collections::HashSet<Uuid> {
        use tod_store::conversation::ProtocolKind;
        self.slots
            .iter()
            .filter(|s| {
                s.status.running
                    && !matches!(
                        s.protocol,
                        ProtocolKind::Chat | ProtocolKind::Outline | ProtocolKind::VisualDesign
                    )
            })
            .filter_map(|s| match s.focus {
                Focus::Node(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// Nodes a chat (the chat drawer, the freeform conversation) is working
    /// on right now: the complement of [`Self::running_lifecycle_nodes`].
    pub fn running_chat_nodes(&self) -> std::collections::HashSet<Uuid> {
        self.slots
            .iter()
            .filter(|s| {
                s.status.running && matches!(s.protocol, ProtocolKind::Chat | ProtocolKind::Outline)
            })
            .filter_map(|s| match s.focus {
                Focus::Node(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// The status label (W11, `unified::status_label`) for every node with a
    /// gate check or on-entry run in flight, keyed by node id. Nodes with
    /// nothing running are left out — the tree row falls back to its own
    /// plain lifecycle text. Computed once per call (the host calls it only
    /// when this registry notifies, never per row per frame) since it reads
    /// each running slot's conversation row.
    pub fn running_status_labels(&self, lifecycle_of: impl Fn(Uuid) -> Option<String>) -> std::collections::HashMap<Uuid, String> {
        let mut nodes: Vec<Uuid> = self
            .slots
            .iter()
            .filter(|s| s.status.running && s.protocol.has_transition())
            .filter_map(|s| match s.focus {
                Focus::Node(id) => Some(id),
                _ => None,
            })
            .collect();
        nodes.sort();
        nodes.dedup();
        nodes
            .into_iter()
            .filter_map(|node| {
                let lifecycle = lifecycle_of(node)?;
                let runs = self.runs_for_node(node);
                Some((node, crate::unified::status_label::text(&lifecycle, &runs).to_string()))
            })
            .collect()
    }

    /// A hook for W10 (answering decisions from outside the conversation
    /// view): behaves like the user typing `text` into the conversation
    /// `conversation_id` and sending it — finds the matching slot and sends
    /// on the background executor, exactly as `ConversationView::send` does.
    ///
    /// Best-effort: it only delivers when that conversation already has a
    /// slot here and it is idle (not away starting or ticking a turn, not
    /// already sending); `false` means the caller should fall back to
    /// opening the conversation view instead (a conversation without a slot
    /// yet — nobody has opened it since the app started — has no in-flight
    /// state to answer into). It never blocks the UI thread: sending itself
    /// runs off it, the same as every other turn.
    pub fn send_to_conversation(&mut self, conversation_id: Uuid, text: &str, cx: &mut Context<Self>) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        let Some(ix) = self
            .slots
            .iter()
            .position(|s| s.conversation_id == Some(conversation_id))
        else {
            return false;
        };
        let Some((id, mut driver)) = self.take_to_send(ix) else {
            return false;
        };
        cx.notify();
        let fleet = self.fleet.clone();
        let agent = self.agent.clone();
        cx.spawn(async move |this, cx| {
            let (driver, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = driver
                        .send(&fleet, &mut SharedAgentAccess(&agent), &text)
                        .map_err(|e| format!("{e:#}"));
                    (driver, result)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.put_back(id, driver);
                cx.notify();
            });
            if let Err(err) = result {
                tracing::warn!("send_to_conversation delivery failed: {err}");
            }
        })
        .detach();
        true
    }

    /// Tool-agnostic config for a new driver, built on demand the same way
    /// `ConversationView::driver_config` does.
    fn driver_config(&self) -> Result<ConversationConfig, String> {
        let paths = TodPaths::discover().map_err(|e| format!("{e:#}"))?;
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let media =
            tod_core::media::MediaPaths::discover().map_err(|e| format!("Media bundle: {e}"))?;
        Ok(ConversationConfig {
            data_root: self.fleet.paths().root().to_path_buf(),
            media,
            launch: settings.launch_options_for(tod_store::AgentRole::Default),
            settings_path: Some(paths.settings_path()),
            context: settings.interview_context.clone(),
        })
    }

    /// The slot for `conversation_id`, resuming it from its stored row when
    /// there is none here yet — so [`Self::answer_decision`] can deliver even
    /// when nobody has opened this conversation's view since the app
    /// started.
    fn ensure_for_conversation(&mut self, conversation_id: Uuid) -> anyhow::Result<usize> {
        let conversation = self
            .fleet
            .read(|conn| ConversationRepo::new(conn).get(conversation_id))?
            .with_context(|| format!("conversation {conversation_id} not found"))?;
        let config = self.driver_config();
        let fleet = self.fleet.clone();
        self.ensure(
            conversation.focus,
            conversation.protocol,
            Some(conversation_id),
            move || {
                let config = config?;
                ConversationDriver::open(config, &fleet, conversation_id).map_err(|e| format!("{e:#}"))
            },
        )
        .map_err(|e| anyhow::anyhow!(e))
    }

    /// "option 2 (\"per invoice\")", "\"do it Tuesday\"", or "option 1" when
    /// there is no free text and the option's own label is empty.
    fn describe_answer(decision: &Decision, option: Option<i64>, text: Option<&str>) -> String {
        let picked = option.and_then(|o| {
            usize::try_from(o)
                .ok()
                .and_then(|ix| decision.options.get(ix.checked_sub(1)?))
        });
        match (picked, text) {
            (Some(label), Some(text)) if !text.is_empty() => format!("\"{label}\" ({text})"),
            (Some(label), _) => format!("\"{label}\""),
            (None, Some(text)) if !text.is_empty() => format!("\"{text}\""),
            _ => "(no answer recorded)".to_string(),
        }
    }

    /// Records the user's answer to `decision_id` and says what to tell the
    /// asking conversation: its id and the message, or `None` when nothing
    /// is to be delivered (a permission, or a decision with no
    /// conversation). Blocks on the store: run it off the UI thread.
    pub fn record_decision_answer(
        fleet: &FleetStore,
        decision_id: Uuid,
        option: Option<usize>,
        text: Option<String>,
    ) -> anyhow::Result<Option<(Uuid, String)>> {
        let before = fleet
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))?
            .with_context(|| format!("decision {decision_id} not found"))?;
        let option_index = option.map(|o| o as i64);

        fleet.interview(
            ACTOR_USER,
            InterviewCommand::AnswerDecision {
                decision_id,
                option: option_index,
                text: text.clone(),
            },
        )?;

        let after = fleet
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))?
            .with_context(|| format!("decision {decision_id} vanished after answering"))?;

        // A permission is a standing grant, not a message: the runner that
        // stopped for it reopens the step once nothing is waiting, and the
        // agent's next ask is answered from the saved answer
        // (`tod_core::permission`).
        if after.decision.protocol.as_deref() == Some(tod_core::permission::PROTOCOL) {
            return Ok(None);
        }
        let Some(conversation_id) = after.decision.conversation_id else {
            return Ok(None);
        };

        let chosen = Self::describe_answer(&after.decision, option_index, text.as_deref());
        // A credential request: the agent is told whether it is now set,
        // never anything of its value (`tod_core::environment_request`).
        let credential = tod_core::environment_request::is_request(&after.decision)
            .then(|| tod_core::environment_request::parse_question(&after.decision.question))
            .flatten()
            .map(|(name, _)| {
                tod_core::environment_request::answer_message(
                    &name,
                    tod_core::environment_request::was_provided(&after.decision, option_index),
                )
            });
        let message = match credential {
            Some(message) => message,
            None => match before.answers.last() {
            Some(prev) => {
                let previous =
                    Self::describe_answer(&after.decision, prev.option, prev.text.as_deref());
                format!(
                    "Decision answered: {}\n\nThe user answered {chosen}.\n\nThe user changed \
                     their answer from {previous} to {chosen}: review what you did based on \
                     {previous} and adjust.",
                    after.decision.question
                )
            }
            None => format!(
                "Decision answered: {}\n\nThe user answered {chosen}.",
                after.decision.question
            ),
            },
        };

        Ok(Some((conversation_id, message)))
    }

    /// Records the user's answer to `decision_id` (append-only: a change of
    /// mind is a new `decision_answers` row, never an update — see
    /// `tod_store::decisions`), then delivers a turn to the asking
    /// conversation with the question, the chosen option/text, and — if this
    /// is a change of mind — the previous answer, so the agent knows to
    /// review what it did based on it.
    ///
    /// Delivery resumes the conversation's slot when it is not hosted yet
    /// (`Self::ensure_for_conversation`), so this always reaches the agent
    /// once the decision has a conversation, regardless of what a view has
    /// open. It never blocks the UI thread: recording the answer is a single
    /// SQLite write (as every other conversation action is), and sending the
    /// turn itself runs off it via `Self::send_to_conversation`.
    pub fn answer_decision(
        &mut self,
        decision_id: Uuid,
        option: Option<usize>,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if let Some((conversation_id, message)) =
            Self::record_decision_answer(&self.fleet, decision_id, option, text)?
        {
            self.deliver_answer(conversation_id, &message, cx)?;
        }
        Ok(())
    }

    /// [`Self::answer_decision`] without blocking the UI thread: the answer
    /// is recorded on the background executor, the turn is delivered here,
    /// and `done` gets the result.
    pub fn answer_decision_then(
        &mut self,
        decision_id: Uuid,
        option: Option<usize>,
        text: Option<String>,
        cx: &mut Context<Self>,
        done: impl FnOnce(anyhow::Result<()>, &mut gpui::App) + 'static,
    ) {
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let recorded = cx
                .background_executor()
                .spawn(async move { Self::record_decision_answer(&fleet, decision_id, option, text) })
                .await;
            let result = match recorded {
                Ok(Some((conversation_id, message))) => this
                    .update(cx, |this, cx| this.deliver_answer(conversation_id, &message, cx))
                    .unwrap_or(Ok(())),
                Ok(None) => Ok(()),
                Err(err) => Err(err),
            };
            cx.update(|cx| done(result, cx));
        })
        .detach();
    }

    fn deliver_answer(&mut self, conversation_id: Uuid, message: &str, cx: &mut Context<Self>) -> anyhow::Result<()> {
        self.ensure_for_conversation(conversation_id)?;
        if !self.send_to_conversation(conversation_id, message, cx) {
            tracing::warn!(
                "answer_decision: could not deliver the answer to conversation {conversation_id}"
            );
        }
        Ok(())
    }

    fn latest_handoff_conversation(&self, focus: Focus) -> anyhow::Result<Option<Uuid>> {
        Self::latest_handoff_conversation_in(&self.fleet, focus)
    }

    fn latest_handoff_conversation_in(fleet: &FleetStore, focus: Focus) -> anyhow::Result<Option<Uuid>> {
        Ok(fleet.read(|conn| latest_handoff_conversation(conn, focus))?.map(|c| c.id))
    }

    /// Whether `conversation_id`'s turn is in flight here, driven by the app
    /// or by a runner hosted elsewhere.
    pub fn conversation_running(&self, conversation_id: Uuid) -> bool {
        self.slots
            .iter()
            .any(|d| d.conversation_id == Some(conversation_id) && d.status.running)
    }

    /// The config a driver started from the app gets.
    pub fn conversation_config(&self) -> Result<ConversationConfig, String> {
        self.driver_config()
    }

    /// Answer a plan step the agent handed back to the user (`HandoffReason`),
    /// from outside the conversation view — the unified task panel's
    /// `PlanStep` items (`crate::unified::panels::decisions`). Sends the same
    /// message `conversation::side_pane::answer_handoff` sends, to whichever
    /// implementation/verification conversation on the step's node most
    /// recently handed work back, then sets the step back to `in_progress`
    /// as a `ConversationEdit` under that conversation — the same mutation
    /// `ConversationView::choose_status` makes after a handoff answer, so it
    /// is reversible and the agent hears of it.
    pub fn answer_plan_step_handoff(
        &mut self,
        step_id: Uuid,
        answer: HandoffAnswer,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let step = self
            .fleet
            .read(|conn| PlanStepRepo::new(conn).get(step_id))?
            .with_context(|| format!("plan step {step_id} not found"))?;
        let focus = Focus::Node(step.node_id);
        let conversation_id = self
            .latest_handoff_conversation(focus)?
            .context(
                "No implement or verify conversation has worked on this node, so there is no \
                agent to send the answer to. Start one, or change the step yourself in the \
                Plan panel.",
            )?;

        let message = handoff_answer_message(&step, &answer);
        self.ensure_for_conversation(conversation_id)?;
        // As in the conversation view, the step goes back to `in_progress`
        // only once the answer has gone out: otherwise it would read as
        // answered while the agent never heard it.
        if !self.send_to_conversation(conversation_id, &message, cx) {
            anyhow::bail!("the agent is busy; the answer was not sent");
        }

        self.fleet.interview(
            ACTOR_USER,
            InterviewCommand::ConversationEdit {
                conversation_id,
                mutation: OutlineMutation::UpdatePlanStepStatus {
                    step_id,
                    status: STATUS_IN_PROGRESS.to_string(),
                    note: None,
                    reason: None,
                },
            },
        )?;
        Ok(())
    }

    /// [`Self::answer_plan_step_handoff`] without blocking the UI thread:
    /// reads and writes run on the background executor, the turn is
    /// delivered here, and `done` gets the result.
    pub fn answer_plan_step_handoff_then(
        &mut self,
        step_id: Uuid,
        answer: HandoffAnswer,
        cx: &mut Context<Self>,
        done: impl FnOnce(anyhow::Result<()>, &mut gpui::App) + 'static,
    ) {
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let prepare_fleet = fleet.clone();
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    let step = prepare_fleet
                        .read(|conn| PlanStepRepo::new(conn).get(step_id))?
                        .with_context(|| format!("plan step {step_id} not found"))?;
                    let conversation_id = Self::latest_handoff_conversation_in(&prepare_fleet, Focus::Node(step.node_id))?
                        .context(
                            "No implement or verify conversation has worked on this node, so there is no \
                            agent to send the answer to. Start one, or change the step yourself in the \
                            Plan panel.",
                        )?;
                    anyhow::Ok((conversation_id, handoff_answer_message(&step, &answer)))
                })
                .await;
            let result = match prepared {
                Err(err) => Err(err),
                Ok((conversation_id, message)) => {
                    // The step goes back to `in_progress` only once the answer
                    // has gone out, as in the synchronous path.
                    let sent = this
                        .update(cx, |this, cx| {
                            this.ensure_for_conversation(conversation_id)?;
                            if this.send_to_conversation(conversation_id, &message, cx) {
                                Ok(())
                            } else {
                                Err(anyhow::anyhow!("the agent is busy; the answer was not sent"))
                            }
                        })
                        .unwrap_or(Ok(()));
                    match sent {
                        Err(err) => Err(err),
                        Ok(()) => {
                            cx.background_executor()
                                .spawn(async move {
                                    fleet.interview(
                                        ACTOR_USER,
                                        InterviewCommand::ConversationEdit {
                                            conversation_id,
                                            mutation: OutlineMutation::UpdatePlanStepStatus {
                                                step_id,
                                                status: STATUS_IN_PROGRESS.to_string(),
                                                note: None,
                                                reason: None,
                                            },
                                        },
                                    )?;
                                    anyhow::Ok(())
                                })
                                .await
                        }
                    }
                }
            };
            cx.update(|cx| done(result, cx));
        })
        .detach();
    }

    /// Settle a pending decision the user answered outside the app, in the
    /// asking agent's own session: an answer saying so is logged, and no turn
    /// is sent, since that session already has the answer. The next turn
    /// resumes the same session.
    pub fn resolve_decision_on(fleet: &FleetStore, decision_id: Uuid) -> anyhow::Result<()> {
        fleet.interview(
            ACTOR_USER,
            InterviewCommand::AnswerDecision {
                decision_id,
                option: None,
                text: Some(RESOLVED_ELSEWHERE.to_string()),
            },
        )?;
        Ok(())
    }

    pub fn resolve_decision_elsewhere(&mut self, decision_id: Uuid) -> anyhow::Result<()> {
        Self::resolve_decision_on(&self.fleet, decision_id)
    }

    /// Put a plan step the agent handed back to `in_progress` without
    /// sending an answer: the user settled it outside the app, in the agent's
    /// own session. Recorded under the conversation that handed it back,
    /// when there is one, as [`Self::answer_plan_step_handoff`] does.
    pub fn resolve_plan_step_on(fleet: &FleetStore, step_id: Uuid) -> anyhow::Result<()> {
        let step = fleet
            .read(|conn| PlanStepRepo::new(conn).get(step_id))?
            .with_context(|| format!("plan step {step_id} not found"))?;
        let mutation = OutlineMutation::UpdatePlanStepStatus {
            step_id,
            status: STATUS_IN_PROGRESS.to_string(),
            note: None,
            reason: None,
        };
        match Self::latest_handoff_conversation_in(fleet, Focus::Node(step.node_id))? {
            Some(conversation_id) => {
                fleet.interview(
                    ACTOR_USER,
                    InterviewCommand::ConversationEdit { conversation_id, mutation },
                )?;
            }
            None => {
                fleet.enqueue_outline(mutation)?;
                fleet.writer().flush()?;
            }
        }
        Ok(())
    }

    pub fn resolve_plan_step_elsewhere(&mut self, step_id: Uuid) -> anyhow::Result<()> {
        Self::resolve_plan_step_on(&self.fleet, step_id)
    }

    /// Answer an open review finding, from outside the conversation view —
    /// the unified task panel's `Finding` items. Mirrors
    /// `conversation::side_pane::respond_to_finding`: a pure status write,
    /// nothing to deliver to an agent (a fix conversation answers findings on
    /// its own initiative instead).
    pub fn respond_finding_on(fleet: &FleetStore, finding_id: Uuid, status: &str) -> anyhow::Result<()> {
        let current = fleet
            .read(|conn| ReviewRepo::new(conn).get(finding_id))?
            .with_context(|| format!("finding {finding_id} not found"))?;
        if current.status != status {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::RespondReviewFinding {
                    finding_id,
                    status: status.to_string(),
                    response: current.response.clone(),
                },
            )?;
        }
        Ok(())
    }

    pub fn respond_review_finding(&mut self, finding_id: Uuid, status: &str) -> anyhow::Result<()> {
        Self::respond_finding_on(&self.fleet, finding_id, status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{AppContext, TestAppContext};
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;
    use tod_core::conversation::ConversationConfig;

    fn config(fixture: &Fixture) -> ConversationConfig {
        ConversationConfig {
            data_root: fixture.store.paths().root().to_path_buf(),
            media: tod_core::media::MediaPaths::discover().expect("media paths"),
            launch: tod_agent::AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
            settings_path: None,
            context: Default::default(),
        }
    }

    fn mock_agent() -> SharedAgent {
        Arc::new(Mutex::new(Box::new(MockAgentProvider::new())))
    }

    /// A conversation a node's runner drives shows as running here, is never
    /// taken to send, and is gone once released.
    #[gpui::test]
    fn a_conversation_hosted_elsewhere_is_shown_but_never_sent_to(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), mock_agent()));
        let node = fixture.node_id;
        let focus = Focus::Node(node);
        let conversation = Uuid::new_v4();
        registry.update(cx, |registry, _| {
            let running = ConversationStatus {
                running: true,
                ..Default::default()
            };
            let id = registry
                .host_elsewhere(focus, ProtocolKind::Implementation, conversation, running.clone())
                .unwrap();
            assert!(registry.protocol_running(focus, ProtocolKind::Implementation));
            assert!(!registry.driving_on_node(node), "not one of the registry's own");
            let ix = registry
                .ensure(focus, ProtocolKind::Implementation, Some(conversation), || {
                    Err("the hosted slot is found, none is made".into())
                })
                .unwrap();
            assert!(registry.take_to_send(ix).is_none());
            assert!(registry.take_running().is_empty());
            assert!(registry.update_elsewhere(id, running));
            registry.release_elsewhere(id);
            assert!(registry.find_index(focus, ProtocolKind::Implementation, Some(conversation)).is_none());
            assert!(!registry.update_elsewhere(id, ConversationStatus::default()));
        });
    }

    #[gpui::test]
    fn runs_for_node_reports_kind_and_running_state(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let agent = mock_agent();
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let node = fixture.node_id;
        let focus = Focus::Node(node);
        registry.update(cx, |registry, _| {
            let driver = ConversationDriver::new(config(&fixture), focus, ProtocolKind::Fix);
            let ix = registry
                .ensure(focus, ProtocolKind::Fix, None, || Ok(driver))
                .unwrap();
            // Take and put back to exercise the same path a real send does.
            let (id, driver) = registry.take_to_send(ix).unwrap();
            registry.put_back(id, driver);
        });

        let runs = registry.read_with(cx, |registry, _| registry.runs_for_node(node));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].protocol, ProtocolKind::Fix);
        assert_eq!(runs[0].conversation_id, None);

        // A different node has no runs.
        let other = registry.read_with(cx, |registry, _| registry.runs_for_node(Uuid::new_v4()));
        assert!(other.is_empty());
    }

    #[gpui::test]
    fn a_running_flag_change_notifies_observers(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let agent = mock_agent();
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let node = fixture.node_id;
        let focus = Focus::Node(node);

        let notified = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notified_write = notified.clone();
        let _sub = cx.update(|cx| {
            cx.observe(&registry, move |_, _| {
                notified_write.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        });

        registry.update(cx, |registry, cx| {
            let driver = ConversationDriver::new(config(&fixture), focus, ProtocolKind::Fix);
            let ix = registry
                .ensure(focus, ProtocolKind::Fix, None, || Ok(driver))
                .unwrap();
            let (id, driver) = registry.take_to_send(ix).unwrap();
            registry.put_back(id, driver);
            cx.notify();
        });
        cx.run_until_parked();

        assert!(
            notified.load(std::sync::atomic::Ordering::SeqCst),
            "the observer should see the running-flag change"
        );
    }

    /// A minimal [`AgentProvider`] that always succeeds with an empty reply
    /// and records every message it was sent, for [`answer_decision`] tests
    /// that need a real turn to go out without the full mock-interview
    /// machinery `--agent mock` uses in the running app.
    struct RecordingAgent {
        sessions: std::collections::HashMap<String, String>,
        runs: std::collections::HashMap<tod_agent::RunId, tod_agent::AgentRunState>,
        sent: Vec<String>,
    }

    impl RecordingAgent {
        fn new() -> Self {
            Self {
                sessions: Default::default(),
                runs: Default::default(),
                sent: Vec::new(),
            }
        }
    }

    impl tod_agent::AgentProvider for RecordingAgent {
        fn start_fleet_agent(
            &mut self,
            _owner_id: &str,
            _cwd: std::path::PathBuf,
            _prompt: String,
            _options: tod_agent::AgentLaunchOptions,
            _session_title: String,
            _environment: tod_agent::AgentEnvironment,
        ) -> anyhow::Result<tod_agent::AgentRunHandle> {
            anyhow::bail!("not used")
        }

        fn send_session_turn(
            &mut self,
            turn: tod_agent::SessionTurn,
        ) -> anyhow::Result<tod_agent::AgentRunHandle> {
            self.sent.push(turn.message.clone());
            let session = turn
                .resume_session_id
                .clone()
                .unwrap_or_else(|| format!("agent-side-{}", Uuid::new_v4()));
            self.sessions.insert(turn.key.clone(), session);
            let id = tod_agent::RunId::new();
            self.runs
                .insert(id, tod_agent::AgentRunState::Success(Some(String::new())));
            Ok(tod_agent::AgentRunHandle { id })
        }

        fn session_id(&self, key: &str) -> Option<String> {
            self.sessions.get(key).cloned()
        }

        fn fleet_run_session_id(&self, _id: tod_agent::RunId) -> Option<String> {
            None
        }

        fn session_context_chars(&self, _key: &str) -> Option<u64> {
            None
        }

        fn close_session(&mut self, key: &str) {
            self.sessions.remove(key);
        }

        fn poll_run(&mut self, id: tod_agent::RunId) -> Option<tod_agent::AgentRunState> {
            self.runs.get(&id).cloned()
        }

        fn respond_to_permission(&mut self, _id: tod_agent::RunId, _option_id: &str) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }

        fn cancel_run(&mut self, _id: tod_agent::RunId) -> anyhow::Result<()> {
            Ok(())
        }

        fn interview_status_counts(&self) -> tod_agent::agent_traffic::InterviewAgentCounts {
            Default::default()
        }
    }

    /// Answering a decision records an append-only answer (a change of mind
    /// is a second row, not an update to the first) and delivers a turn to
    /// the conversation that asked, naming the previous answer when this is
    /// a change of mind — even though nobody has opened that conversation's
    /// view, so [`AgentRuns`] has no slot for it yet.
    #[gpui::test]
    fn answer_decision_records_append_only_and_delivers_a_turn(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        // `ensure_for_conversation` resolves a driver config through
        // `TodPaths::discover`, which reads this process-wide override
        // rather than a real install; point it at the fixture's own root.
        tod_store::paths::set_data_root(fixture.store.paths().root().to_path_buf());
        let node = fixture.node_id;
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(RecordingAgent::new())));
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));

        let conversation_id = Uuid::new_v4();
        fixture
            .store
            .interview(
                tod_store::interview::ACTOR_USER,
                tod_store::interview::InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: Focus::Node(node),
                    protocol: ProtocolKind::Outline,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        let decision_id = fixture
            .store
            .interview(
                tod_store::interview::ACTOR_USER,
                tod_store::interview::InterviewCommand::AskDecision {
                    node_id: node,
                    conversation_id: Some(conversation_id),
                    protocol: Some("outline".to_string()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Round per line or per invoice?".to_string(),
                        options: vec!["per line".to_string(), "per invoice".to_string()],
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();

        // Drives every slot's driver to collect the turn the background
        // executor's send finished: nothing else in this test polls it (in
        // the running app, `ConversationView`'s own timer does), so a send
        // that isn't ticked leaves the driver's `run` set and the next
        // `send` on it bails with "still working on the previous message".
        fn complete_running(registry: &gpui::Entity<AgentRuns>, cx: &mut TestAppContext) {
            registry.update(cx, |registry, cx| {
                let agent = registry.agent().clone();
                let fleet = registry.fleet().clone();
                for (id, mut driver) in registry.take_running() {
                    driver.tick(&fleet, &mut SharedAgentAccess(&agent));
                    registry.put_back(id, driver);
                }
                cx.notify();
            });
            cx.run_until_parked();
        }

        registry
            .update(cx, |registry, cx| {
                registry.answer_decision(decision_id, Some(1), None, cx)
            })
            .unwrap();
        cx.run_until_parked();
        complete_running(&registry, cx);

        let with_answers = fixture
            .store
            .read(|conn| {
                tod_store::decisions::DecisionRepo::new(conn).get_with_answers(decision_id)
            })
            .unwrap()
            .unwrap();
        assert_eq!(with_answers.answers.len(), 1);
        assert_eq!(with_answers.answers[0].option, Some(1));

        let turns = fixture
            .store
            .read(|conn| ConversationRepo::new(conn).turns(conversation_id))
            .unwrap();
        assert!(
            turns.iter().any(|t| t.body.contains("per line")),
            "expected a delivered turn naming the chosen option: {turns:?}"
        );
        assert!(
            !turns.iter().any(|t| t.body.contains("changed their answer")),
            "the first answer is not a change of mind"
        );

        // A change of mind: a second, append-only answer row, and a
        // delivered turn naming the previous answer.
        registry
            .update(cx, |registry, cx| {
                registry.answer_decision(decision_id, Some(2), Some("changed my mind".to_string()), cx)
            })
            .unwrap();
        cx.run_until_parked();
        complete_running(&registry, cx);

        let with_answers = fixture
            .store
            .read(|conn| {
                tod_store::decisions::DecisionRepo::new(conn).get_with_answers(decision_id)
            })
            .unwrap()
            .unwrap();
        assert_eq!(with_answers.answers.len(), 2, "the first answer is never overwritten");
        assert_eq!(with_answers.answers[0].option, Some(1));
        assert_eq!(with_answers.answers[1].option, Some(2));

        let turns = fixture
            .store
            .read(|conn| ConversationRepo::new(conn).turns(conversation_id))
            .unwrap();
        let change_turn = turns
            .iter()
            .find(|t| t.body.contains("changed their answer"));
        assert!(change_turn.is_some(), "expected a turn about the change of mind: {turns:?}");
        let body = &change_turn.unwrap().body;
        assert!(body.contains("per line"), "{body}");
        assert!(body.contains("per invoice"), "{body}");
    }

    /// Declining a credential request tells the asking conversation it was
    /// declined (and only that), as providing tells it the credential is set.
    #[gpui::test]
    fn a_declined_credential_request_reaches_the_conversation(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        tod_store::paths::set_data_root(fixture.store.paths().root().to_path_buf());
        let node = fixture.node_id;
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(RecordingAgent::new())));
        let registry = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let conversation_id = Uuid::new_v4();
        fixture
            .store
            .interview(
                tod_store::interview::ACTOR_USER,
                tod_store::interview::InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: Focus::Node(node),
                    protocol: ProtocolKind::Outline,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        let decision_id = fixture
            .store
            .interview(
                tod_store::interview::ACTOR_USER,
                tod_store::interview::InterviewCommand::AskDecision {
                    node_id: node,
                    conversation_id: Some(conversation_id),
                    protocol: Some(tod_core::environment_request::PROTOCOL.to_string()),
                    decision: tod_store::decisions::NewDecision {
                        question: tod_core::environment_request::question("widget_key", "to call the widget API"),
                        options: tod_core::environment_request::options(),
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();
        // The decline is the second option.
        registry
            .update(cx, |registry, cx| registry.answer_decision(decision_id, Some(2), None, cx))
            .unwrap();
        cx.run_until_parked();
        registry.update(cx, |registry, cx| {
            let agent = registry.agent().clone();
            let fleet = registry.fleet().clone();
            for (id, mut driver) in registry.take_running() {
                driver.tick(&fleet, &mut SharedAgentAccess(&agent));
                registry.put_back(id, driver);
            }
            cx.notify();
        });
        cx.run_until_parked();
        let turns = fixture
            .store
            .read(|conn| ConversationRepo::new(conn).turns(conversation_id))
            .unwrap();
        assert!(
            turns.iter().any(|t| t.body.contains("cannot provide the credential `widget_key`")),
            "expected the decline message: {turns:?}"
        );
        assert!(!turns.iter().any(|t| t.body.contains("It is now set")), "{turns:?}");
    }
}
