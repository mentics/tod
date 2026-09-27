use uuid::Uuid;

/// Identifies which journey a set of files belongs to (spec §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JourneyKey {
    Node(Uuid),
    Project,
}

impl JourneyKey {
    /// The file stem shared by a journey's `.zst` / `.tail` / `.idx` files,
    /// e.g. `<uuid>` for a node or `project` for the project journey.
    pub fn stem(&self) -> String {
        match self {
            JourneyKey::Node(id) => id.to_string(),
            JourneyKey::Project => "project".to_string(),
        }
    }
}
