/// A managed work task (placeholder fields for later Cursor / worktree integration).
#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,
    pub path: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Active,
    Paused,
    Done,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Done => "done",
        }
    }
}

impl Task {
    pub fn new(name: impl Into<String>, path: impl Into<String>, status: TaskStatus) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            status,
        }
    }
}
