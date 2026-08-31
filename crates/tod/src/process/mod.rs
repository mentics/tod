#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessTask {
    pub title: String,
    /// Lifecycle state from state.md, e.g. "proposed"
    pub lifecycle: String,
    pub entity_path: PathBuf,
    pub project_slug: String,
    pub task_slug: String,
}

#[cfg(test)]
pub fn scan_process_tasks(repo_root: &Path) -> Vec<ProcessTask> {
    let mut tasks = Vec::new();

    let projects_root = repo_root.join("doc").join("process").join("projects");
    if projects_root.is_dir() {
        if let Ok(projects) = std::fs::read_dir(&projects_root) {
            for project in projects.flatten() {
                let project_path = project.path();
                if !project_path.is_dir() {
                    continue;
                }
                let project_slug = match project_path.file_name().and_then(|n| n.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let tasks_dir = project_path.join("tasks");
                collect_tasks_from_dir(&tasks_dir, &project_slug, &mut tasks);
            }
        }
    }

    let standalone = repo_root.join("doc").join("process").join("tasks");
    if standalone.is_dir() {
        collect_tasks_from_dir(&standalone, "", &mut tasks);
    }

    tasks.sort_by(|a, b| {
        a.project_slug
            .cmp(&b.project_slug)
            .then_with(|| a.task_slug.cmp(&b.task_slug))
    });
    tasks
}

/// Map lifecycle → interview phase for kickoff, or None if no interview jump.
/// proposed → task-requirements-interview
/// design → design-interview
/// planning → planning-interview
pub fn interview_phase_for_lifecycle(lifecycle: &str) -> Option<&'static str> {
    match lifecycle {
        "proposed" => Some("task-requirements-interview"),
        "design" => Some("design-interview"),
        "planning" => Some("planning-interview"),
        _ => None,
    }
}

/// Map interview phase → lifecycle state when the outline DB has no row yet.
/// Unknown phases default to `proposed`.
pub fn lifecycle_for_interview_phase(phase: &str) -> &'static str {
    let base = phase.split('(').next().unwrap_or(phase).trim();
    match base {
        "design-interview" => "design",
        "planning-interview" => "planning",
        _ => "proposed",
    }
}

/// Human label for the phase (for display_name).
pub fn interview_phase_label(phase: &str) -> &'static str {
    match phase {
        "task-requirements-interview" => "Task requirements",
        "design-interview" => "Design phase",
        "planning-interview" => "Planning phase",
        "project-defining" => "Initial / defining",
        _ => "Interview",
    }
}

#[cfg(test)]
fn collect_tasks_from_dir(tasks_dir: &Path, project_slug: &str, out: &mut Vec<ProcessTask>) {
    if !tasks_dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(tasks_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let task_path = entry.path();
        if !task_path.is_dir() {
            continue;
        }
        let Some(task_slug) = task_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
        else {
            continue;
        };

        let state_path = task_path.join("state.md");
        if !state_path.is_file() {
            continue;
        }
        let Ok(state_body) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let lifecycle = parse_lifecycle(&state_body).unwrap_or_else(|| "unknown".to_string());

        let title = read_title(&task_path.join("user.md")).unwrap_or_else(|| task_slug.clone());
        let entity_path = task_path
            .canonicalize()
            .unwrap_or_else(|_| task_path.clone());

        out.push(ProcessTask {
            title,
            lifecycle,
            entity_path,
            project_slug: project_slug.to_string(),
            task_slug,
        });
    }
}

#[cfg(test)]
fn parse_lifecycle(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("- State:") else {
            continue;
        };
        let value = rest.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
        return Some(String::new());
    }
    None
}

#[cfg(test)]
fn read_title(user_md: &Path) -> Option<String> {
    let body = std::fs::read_to_string(user_md).ok()?;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_repo(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("tod-process-{}-{}", label, uuid::Uuid::new_v4()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn scan_finds_project_task_with_title_and_lifecycle() {
        let root = temp_repo("scan");
        let task_dir = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("demo")
            .join("tasks")
            .join("widget");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(
            task_dir.join("state.md"),
            "# State\n\n- State: proposed\n- Mode: interactive\n",
        )
        .unwrap();
        fs::write(
            task_dir.join("user.md"),
            "# Widget Persistence\n\nGoal text.\n",
        )
        .unwrap();

        // Dir without state.md must be skipped.
        let skip_dir = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("demo")
            .join("tasks")
            .join("orphan");
        fs::create_dir_all(&skip_dir).unwrap();
        fs::write(skip_dir.join("user.md"), "# Orphan\n").unwrap();

        let found = scan_process_tasks(&root);
        assert_eq!(found.len(), 1);
        let task = &found[0];
        assert_eq!(task.title, "Widget Persistence");
        assert_eq!(task.lifecycle, "proposed");
        assert_eq!(task.project_slug, "demo");
        assert_eq!(task.task_slug, "widget");
        assert!(
            task.entity_path.ends_with(
                Path::new("doc")
                    .join("process")
                    .join("projects")
                    .join("demo")
                    .join("tasks")
                    .join("widget")
            ) || task.entity_path == task_dir.canonicalize().unwrap_or(task_dir.clone())
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn interview_phase_for_lifecycle_mapping() {
        assert_eq!(
            interview_phase_for_lifecycle("proposed"),
            Some("task-requirements-interview")
        );
        assert_eq!(
            interview_phase_for_lifecycle("design"),
            Some("design-interview")
        );
        assert_eq!(
            interview_phase_for_lifecycle("planning"),
            Some("planning-interview")
        );
        assert_eq!(interview_phase_for_lifecycle("active"), None);
        assert_eq!(interview_phase_for_lifecycle("unknown"), None);

        assert_eq!(
            lifecycle_for_interview_phase("task-requirements-interview"),
            "proposed"
        );
        assert_eq!(lifecycle_for_interview_phase("design-interview"), "design");
        assert_eq!(
            lifecycle_for_interview_phase("planning-interview"),
            "planning"
        );
        assert_eq!(
            lifecycle_for_interview_phase("project-defining"),
            "proposed"
        );
        assert_eq!(lifecycle_for_interview_phase("other"), "proposed");

        assert_eq!(
            interview_phase_label("task-requirements-interview"),
            "Task requirements"
        );
        assert_eq!(interview_phase_label("design-interview"), "Design phase");
        assert_eq!(
            interview_phase_label("planning-interview"),
            "Planning phase"
        );
    }

    #[test]
    fn missing_state_line_uses_unknown() {
        let root = temp_repo("unknown-state");
        let task_dir = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("p")
            .join("tasks")
            .join("t");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(
            task_dir.join("state.md"),
            "# State\n\n- Mode: interactive\n",
        )
        .unwrap();
        // No user.md → title falls back to task_slug
        let found = scan_process_tasks(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lifecycle, "unknown");
        assert_eq!(found[0].title, "t");
        let _ = fs::remove_dir_all(&root);
    }
}
