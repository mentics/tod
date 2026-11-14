//! The unified view's *view state* and the facts on screen, for the dev
//! agent socket's driven/user mode (`agent_socket::snapshot`): what the view
//! looks like (so it can be put back after the other party used the app) and
//! what data it shows (so a change under the agent can be named).
//!
//! Only view state is saved and restored. Nothing here writes data.

use std::collections::BTreeSet;

use gpui::{App, Context, Window};
use tod_core::next_actor::NextActor;
use uuid::Uuid;

use super::{Column, PanelKind, UnifiedView, WorkbenchPlace};

/// Where the view is: the tree's selection, each column's panel and pin, the
/// focused column, and whether the chat drawer is open.
#[derive(Debug, Clone)]
pub struct UnifiedViewState {
    pub place: WorkbenchPlace,
    pub focused_column: Option<usize>,
    pub chat_open: bool,
}

impl UnifiedViewState {
    pub fn node(&self) -> Option<Uuid> {
        self.place.node
    }

    pub fn panels(&self) -> Vec<PanelKind> {
        self.place.columns.iter().map(|c: &Column| c.panel).collect()
    }
}

/// What the store says about a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRead {
    Missing,
    Lifecycle(String),
    /// The read failed; never taken for "unchanged".
    Unreadable(String),
}

/// The data on screen for one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFacts {
    pub id: Uuid,
    /// What a person calls it: its ticket, else its slug.
    pub name: String,
    pub read: NodeRead,
    /// Who is next (`working`, `you`, `others`, `done`), `none` when the
    /// node has no standing.
    pub run_state: String,
    /// The ids of the requests waiting on the user.
    pub requests: BTreeSet<Uuid>,
}

/// Whether one open panel still applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelFacts {
    /// `FindingsPanel (ENG-2925)`.
    pub label: String,
    pub applies: bool,
}

fn kind_name(panel: PanelKind) -> &'static str {
    match panel {
        PanelKind::Details(_) => "DetailsPanel",
        PanelKind::Task(_) => "TaskPanel",
        PanelKind::Obligations(_) => "ObligationsPanel",
        PanelKind::Plan(_) => "PlanPanel",
        PanelKind::Findings(_) => "FindingsPanel",
        PanelKind::Settings(_) => "SettingsPanel",
        PanelKind::Changes(_) => "ChangesPanel",
        PanelKind::Transcript(_) => "TranscriptPanel",
        PanelKind::Watch(_) => "ActivityPanel",
        PanelKind::OnOthers => "OnOthersPanel",
        PanelKind::Journey(_) => "JourneyPanel",
        PanelKind::ProjectJourney => "ProjectJourneyPanel",
    }
}

fn panel_node(panel: PanelKind) -> Option<Uuid> {
    match panel {
        PanelKind::Details(id)
        | PanelKind::Task(id)
        | PanelKind::Obligations(id)
        | PanelKind::Plan(id)
        | PanelKind::Findings(id)
        | PanelKind::Settings(id)
        | PanelKind::Changes(id)
        | PanelKind::Journey(id)
        | PanelKind::Watch(id) => Some(id),
        // A transcript's id is its conversation's; the other two are about
        // no one node.
        PanelKind::Transcript(_) | PanelKind::OnOthers | PanelKind::ProjectJourney => None,
    }
}

fn actor_word(actor: &NextActor) -> &'static str {
    match actor {
        NextActor::Working { .. } => "working",
        NextActor::You { .. } => "you",
        NextActor::Others { .. } => "others",
        NextActor::Done => "done",
    }
}

fn short(id: Uuid) -> String {
    id.simple().to_string()[..4].to_string()
}

impl UnifiedView {
    pub fn view_state(&self, cx: &App) -> UnifiedViewState {
        UnifiedViewState {
            place: self.place(cx),
            focused_column: self.columns.focused_index(),
            chat_open: self.chat_drawer.read(cx).expanded(),
        }
    }

    /// Put the view back as `state` says. Menus open in the tree are closed
    /// (a popover cannot be restored); keyboard focus goes to the column that
    /// was focused.
    pub fn restore_view_state(
        &mut self,
        state: &UnifiedViewState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_app_nav(cx);
        self.restore_place(&state.place, window, cx);
        if let Some(ix) = state.focused_column {
            self.columns.focus(ix);
        }
        let open = self.chat_drawer.read(cx).expanded();
        if state.chat_open != open {
            self.chat_drawer
                .update(cx, |drawer, cx| drawer.toggle(window, cx));
        }
        self.restore_focus(window, cx);
        cx.notify();
    }

    pub fn node_facts(&self, id: Uuid) -> NodeFacts {
        let (name, read) = match self.fleet.get_node(&id.to_string()) {
            Ok(Some(task)) => (
                task.ticket.clone().unwrap_or_else(|| task.slug.clone()),
                NodeRead::Lifecycle(task.lifecycle),
            ),
            Ok(None) => (short(id), NodeRead::Missing),
            Err(err) => (short(id), NodeRead::Unreadable(format!("{err:#}"))),
        };
        NodeFacts {
            id,
            name,
            read,
            run_state: self
                .standings
                .get(&id)
                .map_or("none", |standing| actor_word(&standing.actor))
                .to_string(),
            requests: self
                .attention
                .get(&id)
                .map(|attention| attention.items.iter().map(|item| item.id).collect())
                .unwrap_or_default(),
        }
    }

    pub fn panel_facts(&self, panel: PanelKind) -> PanelFacts {
        let facts = panel_node(panel).map(|id| self.node_facts(id));
        PanelFacts {
            label: match &facts {
                Some(facts) => format!("{} ({})", kind_name(panel), facts.name),
                None => kind_name(panel).to_string(),
            },
            applies: match &facts {
                Some(facts) => facts.read != NodeRead::Missing,
                None => true,
            },
        }
    }
}
