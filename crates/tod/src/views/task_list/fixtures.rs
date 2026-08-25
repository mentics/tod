/// Mock task row for the task list slice.
#[derive(Clone, Debug)]
pub struct TaskItem {
    pub title: String,
    pub lifecycle: &'static str,
    pub tags: Vec<&'static str>,
    pub agent_count: usize,
}

/// Hand-authored fixture rows covering all 12 project lifecycle states.
pub fn sample_tasks() -> Vec<TaskItem> {
    vec![
        TaskItem {
            title: "Add fleet persistence layer".into(),
            lifecycle: "proposed",
            tags: vec!["backend", "data"],
            agent_count: 0,
        },
        TaskItem {
            title: "Design notification resolve UX".into(),
            lifecycle: "design",
            tags: vec!["ui"],
            agent_count: 1,
        },
        TaskItem {
            title: "Plan Linear webhook integration".into(),
            lifecycle: "planning",
            tags: vec!["integrations", "linear"],
            agent_count: 2,
        },
        TaskItem {
            title: "Implement agent launch dialog".into(),
            lifecycle: "ready",
            tags: vec!["agents", "ui"],
            agent_count: 0,
        },
        TaskItem {
            title: "Wire GitHub PR associations".into(),
            lifecycle: "active",
            tags: vec!["github"],
            agent_count: 3,
        },
        TaskItem {
            title: "Verify worktree reclaim flow".into(),
            lifecycle: "verifying",
            tags: vec!["git", "qa"],
            agent_count: 1,
        },
        TaskItem {
            title: "External review: shell keyboard nav".into(),
            lifecycle: "review",
            tags: vec!["keyboard"],
            agent_count: 0,
        },
        TaskItem {
            title: "Approved: credential storage MVP".into(),
            lifecycle: "approved",
            tags: vec!["security"],
            agent_count: 0,
        },
        TaskItem {
            title: "Merge agent transcript streaming".into(),
            lifecycle: "merged",
            tags: vec!["agents"],
            agent_count: 2,
        },
        TaskItem {
            title: "Release dev preview build".into(),
            lifecycle: "released",
            tags: vec!["release"],
            agent_count: 0,
        },
        TaskItem {
            title: "Retrospective: ui-scaffolding learn".into(),
            lifecycle: "learn",
            tags: vec!["process"],
            agent_count: 1,
        },
        TaskItem {
            title: "Close out import JSON spike".into(),
            lifecycle: "done",
            tags: vec!["import"],
            agent_count: 0,
        },
        TaskItem {
            title: "Spike micro-VM agent host".into(),
            lifecycle: "design",
            tags: vec!["infra", "agents"],
            agent_count: 4,
        },
        TaskItem {
            title: "Prototype fuzzy task search".into(),
            lifecycle: "planning",
            tags: vec!["search", "ui"],
            agent_count: 0,
        },
    ]
}
