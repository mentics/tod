//! Outline domain types.

use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Capability {
    Spec,
    Lifecycle,
    Agent,
    Generator,
    Tags,
    Files,
    Ticket,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::Lifecycle => "lifecycle",
            Self::Agent => "agent",
            Self::Generator => "generator",
            Self::Tags => "tags",
            Self::Files => "files",
            Self::Ticket => "ticket",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "spec" => Some(Self::Spec),
            "lifecycle" => Some(Self::Lifecycle),
            "agent" => Some(Self::Agent),
            "generator" => Some(Self::Generator),
            "tags" => Some(Self::Tags),
            "files" => Some(Self::Files),
            "ticket" => Some(Self::Ticket),
            _ => None,
        }
    }

    pub const ALL: [Self; 7] = [
        Self::Spec,
        Self::Lifecycle,
        Self::Agent,
        Self::Files,
        Self::Ticket,
        Self::Generator,
        Self::Tags,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Spec => "Spec",
            Self::Lifecycle => "Lifecycle",
            Self::Agent => "Agent",
            Self::Generator => "Generator",
            Self::Tags => "Tags",
            Self::Files => "Files",
            Self::Ticket => "Ticket",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Spec => "Requirements, constraints, and interview artifacts",
            Self::Lifecycle => "Process state and lifecycle transitions",
            Self::Agent => "Agent platform, model, and effort for chats and coding agents",
            Self::Files => "Workspace directory, branch, and optional worktree",
            Self::Ticket => "Ticket ID and pull request links",
            Self::Generator => {
                "Automatically produce and manage descendant nodes from an external data source"
            }
            Self::Tags => "Freeform labels for organizing and filtering nodes",
        }
    }

    pub fn disable_warning(self) -> &'static str {
        match self {
            Self::Spec => {
                "Disabling Spec will permanently remove this node's obligations, extra content, and interview data."
            }
            Self::Lifecycle => "Disabling Lifecycle will remove this node's lifecycle state.",
            Self::Agent => {
                "Disabling Agent will remove the platform, model, and effort stored on this node."
            }
            Self::Files => {
                "Disabling Files will remove the workspace directory, branch, and worktree settings stored on this node."
            }
            Self::Ticket => {
                "Disabling Ticket will remove the ticket ID and pull request links stored on this node."
            }
            Self::Generator => {
                "Disabling Generator will permanently delete all managed child nodes under this node."
            }
            Self::Tags => "Disabling Tags will remove this node's tags.",
        }
    }

    /// Capabilities that are mutually exclusive with this one.
    pub fn mutually_exclusive(self) -> &'static [Self] {
        match self {
            Self::Generator => &[Self::Lifecycle],
            Self::Lifecycle => &[Self::Generator],
            _ => &[],
        }
    }
}

/// `node_extra_content.content_type` value for the node's purpose / goal statement.
pub const EXTRA_CONTENT_GOAL: &str = "goal";

/// `node_extra_content.content_type` value for the node's imported/freeform details.
pub const EXTRA_CONTENT_DETAILS: &str = "details";

/// `node_extra_content.content_type` value for the node's generated summary.
/// Ancestor context shows this plus constraints in place of the ancestor's
/// requirements. The state docs regenerate it on entering `design` and
/// `planning`.
pub const EXTRA_CONTENT_SUMMARY: &str = "summary";

pub const EXTRA_CONTENT_TYPES: [&str; 6] =
    ["goal", "design", "plan", "notes", "details", "summary"];

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
    /// True when this node was produced/is owned by a generator ancestor.
    pub managed: bool,
    /// The data-source external id, for managed nodes.
    pub external_id: Option<String>,
    /// The data-source type (e.g. "linear"), for managed nodes.
    pub source_type: Option<String>,
    /// Direct managed child count, for nodes with the Generator capability.
    pub managed_count: Option<usize>,
    /// `last_refresh_status` ("in_progress" | "success" | "error"), for generator nodes.
    pub generator_status: Option<String>,
    /// `last_refresh_error`, for generator nodes whose last refresh failed.
    pub generator_error: Option<String>,
}
