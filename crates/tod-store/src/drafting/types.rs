//! Drafting row types and the vocabulary stored in their text columns.

use crate::outline::NodeObligation;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROVENANCE_AGENT: &str = "agent";
pub const PROVENANCE_USER: &str = "user";

pub const ATTENTION_LOW: &str = "low";
pub const ATTENTION_MEDIUM: &str = "medium";
pub const ATTENTION_HIGH: &str = "high";
pub const ATTENTION_LEVELS: [&str; 3] = [ATTENTION_LOW, ATTENTION_MEDIUM, ATTENTION_HIGH];

/// The attention reason every obligation written before drafting v3 carries
/// until it is rewritten or the user touches it.
pub const PRE_V3_ATTENTION_WHY: &str = "Written before drafting v3";

/// The single `design` → `planning` gate criterion under drafting v3.
pub const BUILDABLE_CRITERION_SLUG: &str = "design-planning.buildable";

pub const CHOICE_OPEN: &str = "open";
pub const CHOICE_ANSWERED: &str = "answered";
pub const CHOICE_DELEGATED: &str = "delegated";
pub const CHOICE_WITHDRAWN: &str = "withdrawn";

/// Open choices allowed per node. Hitting it means pick defaults instead.
pub const CHOICE_CAP: usize = 3;

/// Sort rank for review: highest attention first.
pub fn attention_rank(attention: Option<&str>) -> u8 {
    match attention {
        Some(ATTENTION_HIGH) => 0,
        Some(ATTENTION_MEDIUM) => 1,
        Some(ATTENTION_LOW) => 2,
        _ => 3,
    }
}

/// An obligation a choice option would write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceObligation {
    /// `requirement` | `constraint`.
    pub kind: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub label: String,
    #[serde(default)]
    pub obligations: Vec<ChoiceObligation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftingChoice {
    pub id: Uuid,
    pub node_id: Uuid,
    pub seq: i64,
    pub phase: String,
    pub context: Option<String>,
    pub question: String,
    pub options: Vec<ChoiceOption>,
    pub status: String,
    /// 1-based option picked (`answered` only).
    pub answer: Option<i64>,
    pub created_at: i64,
    pub answered_at: Option<i64>,
    pub processed_at: Option<i64>,
}

impl DraftingChoice {
    pub fn label(&self) -> String {
        format!("c-{}", self.seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftingDump {
    pub id: Uuid,
    pub seq: i64,
    pub target_node_id: Option<Uuid>,
    pub body: String,
    pub routing: Option<String>,
    pub created_at: i64,
    pub routed_at: Option<i64>,
}

impl DraftingDump {
    pub fn label(&self) -> String {
        format!("d-{}", self.seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftingSummary {
    pub node_id: Uuid,
    pub seq: i64,
    pub body: String,
    pub created_at: i64,
}

/// Provenance and attention for one obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationMark {
    pub provenance: String,
    pub attention: Option<String>,
    pub attention_why: Option<String>,
}

impl ObligationMark {
    pub fn is_agent(&self) -> bool {
        self.provenance == PROVENANCE_AGENT
    }

    pub fn is_pre_v3(&self) -> bool {
        self.is_agent() && self.attention_why.as_deref() == Some(PRE_V3_ATTENTION_WHY)
    }
}

/// An obligation with its marks, as shown during review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedObligation {
    pub obligation: NodeObligation,
    pub mark: ObligationMark,
}

/// A node as the fuzzy node picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePick {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
}

/// A `[[slug]]` in an obligation that names no node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenReference {
    pub obligation_id: Uuid,
    pub node_id: Uuid,
    pub slug: String,
}

/// Every `[[slug]]` written inline in `text`, in order.
pub fn referenced_slugs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let slug = after[..end].trim();
        if !slug.is_empty() && !slug.contains('[') {
            out.push(slug.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_inline_references() {
        assert_eq!(
            referenced_slugs("Settings render as a [[dynamic-form]] and [[ account-picker ]]."),
            vec!["dynamic-form".to_string(), "account-picker".to_string()]
        );
        assert!(referenced_slugs("no refs [[ ]] or [[unclosed").is_empty());
    }
}
