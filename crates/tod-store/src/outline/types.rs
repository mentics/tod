//! Outline domain types.

use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Normal,
    Reference,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Reference => "reference",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Capability {
    Spec,
    Lifecycle,
    Agent,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::Lifecycle => "lifecycle",
            Self::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "spec" => Some(Self::Spec),
            "lifecycle" => Some(Self::Lifecycle),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    pub const ALL: [Self; 3] = [Self::Spec, Self::Lifecycle, Self::Agent];

    pub fn label(self) -> &'static str {
        match self {
            Self::Spec => "Spec",
            Self::Lifecycle => "Lifecycle",
            Self::Agent => "Agent",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Spec => "Requirements, constraints, and interview artifacts",
            Self::Lifecycle => "Process state and lifecycle transitions",
            Self::Agent => "Repository, tags, links, and agent workspace",
        }
    }

    pub fn disable_warning(self) -> &'static str {
        match self {
            Self::Spec => {
                "Disabling Spec will permanently remove this node's obligations, extra content, and interview data."
            }
            Self::Lifecycle => "Disabling Lifecycle will remove this node's lifecycle state.",
            Self::Agent => {
                "Disabling Agent will remove repository settings, tags, links, and notes stored on this node."
            }
        }
    }
}

/// `node_extra_content.content_type` value for the node's purpose / goal statement.
pub const EXTRA_CONTENT_GOAL: &str = "goal";

pub const EXTRA_CONTENT_TYPES: [&str; 4] = ["goal", "design", "plan", "notes"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineList {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub kind: NodeKind,
    pub ref_target_id: Option<Uuid>,
    pub slug_manual: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    pub node_id: Uuid,
    pub list_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub ordinal: i32,
    pub collapsed: bool,
}

/// Flattened tree row for UI rendering (enriched in task_list layer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatNodeRow {
    pub node: Node,
    pub depth: usize,
    pub parent_id: Option<Uuid>,
    pub capabilities: Vec<Capability>,
    pub lifecycle: Option<String>,
    pub tags: Vec<String>,
    pub ticket_id: Option<String>,
    /// Stable row order from outline flatten.
    pub tree_ordinal: usize,
    pub collapsed: bool,
    pub has_children: bool,
}
