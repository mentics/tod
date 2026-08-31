use gpui::{Context, Window};

use super::TaskListView;

/// Returns true when `text` looks like an issue-tracker ticket id (e.g. TOD-142).
pub fn is_ticket_id(text: &str) -> bool {
    text.len() >= 3 && text.contains('-') && text.chars().all(|c| c != ' ')
}

impl TaskListView {
    pub(super) fn import_from_ticket(
        &mut self,
        ticket: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(existing) = self.find_by_ticket_id(ticket) {
            let id = existing.id.clone();
            self.select_created_task(&id, window, cx);
            self.status_line = format!("Selected existing task for {ticket}");
            return true;
        }

        if ticket.eq_ignore_ascii_case("ERR-500") {
            crate::ui::toast::error_toast(window, cx, "Issue tracker fetch failed (stub)");
            return false;
        }

        let title = ticket.to_string();
        let id = format!("linear-{}", ticket.to_lowercase().replace('-', "_"));
        let task = super::TaskItem {
            id: id.clone(),
            ticket_id: Some(ticket.to_string()),
            title,
            lifecycle: "proposed".into(),
            entity_path: std::path::PathBuf::from(format!("linear/{ticket}")),
            tags: vec!["linear".into()],
            agents: vec![],
            shells: vec![],
            interaction_timestamp: chrono::Utc::now(),
            tree_ordinal: 0,
            depth: 0,
            collapsed: false,
            is_work_node: true,
            has_spec: false,
            requirement_count: 0,
            constraint_count: 0,
            has_children: false,
        };
        self.all_tasks.push(task);
        self.select_created_task(&id, window, cx);
        self.status_line = format!("Created task from {ticket}");
        true
    }

    pub(super) fn create_task_with_title(
        &mut self,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slug = title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let id = if slug.is_empty() {
            format!("task-{}", chrono::Utc::now().timestamp_millis())
        } else {
            format!("task-{slug}")
        };
        let task = super::TaskItem {
            id: id.clone(),
            ticket_id: None,
            title: title.to_string(),
            lifecycle: "proposed".into(),
            entity_path: std::path::PathBuf::from(format!("tasks/{id}")),
            tags: vec![],
            agents: vec![],
            shells: vec![],
            interaction_timestamp: chrono::Utc::now(),
            tree_ordinal: 0,
            depth: 0,
            collapsed: false,
            is_work_node: true,
            has_spec: false,
            requirement_count: 0,
            constraint_count: 0,
            has_children: false,
        };
        self.all_tasks.push(task);
        self.select_created_task(&id, window, cx);
        self.status_line = format!("Created task: {title}");
    }

    fn find_by_ticket_id(&self, ticket: &str) -> Option<&super::TaskItem> {
        self.all_tasks.iter().find(|t| {
            t.ticket_id
                .as_ref()
                .is_some_and(|id| id.eq_ignore_ascii_case(ticket))
        })
    }

    pub(super) fn select_created_task(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        if visible.iter().any(|t| t.id == task_id) {
            self.select_task_by_id(task_id, window, cx);
        }
        self.rebuild_visible_list(window, cx);
    }
}
