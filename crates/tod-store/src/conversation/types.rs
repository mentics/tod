//! Conversation log value types.

use crate::outline::OutlineMutation;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a conversation is about. A starting point only: the agent may act
/// anywhere in the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Focus {
    Project,
    Node(Uuid),
    Obligation { node: Uuid, id: Uuid },
    PlanStep { node: Uuid, id: Uuid },
}

impl Focus {
    pub fn kind_str(self) -> &'static str {
        match self {
            Focus::Project => "project",
            Focus::Node(_) => "node",
            Focus::Obligation { .. } => "obligation",
            Focus::PlanStep { .. } => "plan_step",
        }
    }

    /// The focused item's id; `None` for the project.
    pub fn focus_id(self) -> Option<Uuid> {
        match self {
            Focus::Project => None,
            Focus::Node(id) => Some(id),
            Focus::Obligation { id, .. } | Focus::PlanStep { id, .. } => Some(id),
        }
    }

    /// The node the focused item lives on (the node itself for a node focus).
    pub fn node_id(self) -> Option<Uuid> {
        match self {
            Focus::Project => None,
            Focus::Node(id) => Some(id),
            Focus::Obligation { node, .. } | Focus::PlanStep { node, .. } => Some(node),
        }
    }

    /// Rebuild a focus from its stored columns.
    pub fn from_columns(kind: &str, id: Option<Uuid>, node: Option<Uuid>) -> Result<Self> {
        let need = |v: Option<Uuid>, what: &str| {
            v.ok_or_else(|| anyhow::anyhow!("{kind} focus is missing its {what}"))
        };
        Ok(match kind {
            "project" => Focus::Project,
            "node" => Focus::Node(need(id, "id")?),
            "obligation" => Focus::Obligation {
                node: need(node, "node")?,
                id: need(id, "id")?,
            },
            "plan_step" => Focus::PlanStep {
                node: need(node, "node")?,
                id: need(id, "id")?,
            },
            other => bail!("unknown focus kind `{other}`"),
        })
    }
}

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(s: &str) -> Result<Self> {
                match s {
                    $($s => Ok($name::$variant),)+
                    other => bail!(concat!("unknown ", stringify!($name), " `{}`"), other),
                }
            }
        }
    };
}

str_enum!(
    /// What a recorded action did. `Reverse` applied the inverse of an earlier action.
    ActionKind { Create => "create", Edit => "edit", Move => "move", Delete => "delete", Reverse => "reverse" }
);

str_enum!(
    /// The kind of outline item an action touched.
    Entity { Node => "node", Obligation => "obligation", PlanStep => "plan_step" }
);

str_enum!(
    /// Who made an action: the conversation's agent, or the user (edits and reversals).
    ActionActor { Agent => "agent", User => "user" }
);

str_enum!(
    /// A transcript entry. `Rotation` marks where a fresh agent session started.
    TurnRole { User => "user", Agent => "agent", Error => "error", Rotation => "rotation" }
);

/// The recorded state of one item, enough to compare, diff, and restore it.
///
/// A node's `ordinal` is its 0-based position among its siblings (not the raw
/// `outline_entries.ordinal`, which can have gaps); obligation and plan-step
/// ordinals are their 1-based stored ordinals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "entity", rename_all = "snake_case")]
pub enum EntitySnapshot {
    Node {
        title: String,
        list_id: Uuid,
        parent_id: Option<Uuid>,
        ordinal: i32,
    },
    Obligation {
        node_id: Uuid,
        kind: String,
        section: Option<String>,
        body: String,
        phase: String,
        ordinal: i32,
        visual_design_path: Option<String>,
    },
    PlanStep {
        node_id: Uuid,
        ordinal: i32,
        body: String,
        status: String,
        /// Sorted.
        depends_on: Vec<Uuid>,
        /// Obligations this step is linked to. Sorted.
        satisfies: Vec<Uuid>,
    },
}

impl EntitySnapshot {
    pub fn entity(&self) -> Entity {
        match self {
            EntitySnapshot::Node { .. } => Entity::Node,
            EntitySnapshot::Obligation { .. } => Entity::Obligation,
            EntitySnapshot::PlanStep { .. } => Entity::PlanStep,
        }
    }

    /// The node an item with this state lives on; a node lives on itself.
    pub fn node_id(&self, id: Uuid) -> Uuid {
        match self {
            EntitySnapshot::Node { .. } => id,
            EntitySnapshot::Obligation { node_id, .. }
            | EntitySnapshot::PlanStep { node_id, .. } => *node_id,
        }
    }

    pub fn ordinal(&self) -> i32 {
        match self {
            EntitySnapshot::Node { ordinal, .. }
            | EntitySnapshot::Obligation { ordinal, .. }
            | EntitySnapshot::PlanStep { ordinal, .. } => *ordinal,
        }
    }

    /// The item's text: a node's title, an obligation's or step's body.
    pub fn text(&self) -> &str {
        match self {
            EntitySnapshot::Node { title, .. } => title,
            EntitySnapshot::Obligation { body, .. } | EntitySnapshot::PlanStep { body, .. } => body,
        }
    }
}

/// A conversation row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub focus: Focus,
    /// The current provider session; `None` until the first reply.
    pub agent_session_id: Option<String>,
    pub session_name: Option<String>,
    pub platform: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One picker entry: a conversation, its net change count, and the opening
/// words of its first user turn (conversations have no names).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub conversation: Conversation,
    pub change_count: usize,
    pub opening: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub seq: i64,
    pub role: TurnRole,
    /// An empty agent body means "done, no notes".
    pub body: String,
    pub created_at: i64,
}

/// One `conversation_actions` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRow {
    pub id: i64,
    pub conversation_id: Uuid,
    pub turn_seq: i64,
    pub actor: ActionActor,
    pub kind: ActionKind,
    pub entity: Entity,
    pub entity_id: Uuid,
    /// The node the entity lives on after the action (itself for a node).
    pub node_id: Option<Uuid>,
    /// The mutation that was applied, normalized so it can be replayed.
    pub mutation: OutlineMutation,
    pub before: Option<EntitySnapshot>,
    pub after: Option<EntitySnapshot>,
    /// The `DeleteNode` archive, for restore.
    pub archive_id: Option<Uuid>,
    pub reverses: Option<i64>,
    pub reversed_by: Option<i64>,
    pub at: i64,
}

/// The net effect a conversation had on one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetOp {
    Added,
    Edited,
    Moved,
    Deleted,
    Reversed,
}

/// What a [`ContextRef`] points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextTarget {
    Item {
        entity: Entity,
        id: Uuid,
        label: String,
    },
    Node {
        id: Uuid,
        label: String,
    },
}

/// Short context shown beside a change, e.g. "from *Web client*" or
/// "replaced by *P-12*": `phrase` followed by the target's label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRef {
    pub phrase: String,
    pub target: ContextTarget,
}

/// One row of the change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetChange {
    pub entity: Entity,
    pub id: Uuid,
    /// The node the item lives on now (or last lived on, if deleted).
    pub node_id: Option<Uuid>,
    pub op: NetOp,
    /// The item before the conversation first touched it.
    pub before: Option<EntitySnapshot>,
    /// The item as it is now; `None` when it no longer exists.
    pub current: Option<EntitySnapshot>,
    pub context: Vec<ContextRef>,
    /// The agent's reason, when it flagged this item as unsure.
    pub flag: Option<String>,
    /// The actions to pass to `ReverseConversationActions` to reverse this
    /// change (or, when `op` is `Reversed`, to re-apply it): the latest row
    /// of each reversal chain that is currently in effect, oldest first.
    pub action_ids: Vec<i64>,
}

/// The result of [`crate::interview::InterviewCommand::ReverseConversationActions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReverseOutcome {
    Applied {
        new_action_ids: Vec<i64>,
    },
    /// Nothing was applied: some items changed since the conversation last
    /// touched them (`conflicts`), or reversing needs unselected dependent
    /// actions too (`dependents`).
    NeedsConfirmation {
        conflicts: Vec<NetChange>,
        dependents: Vec<NetChange>,
    },
}
