use gpui::{Context, Window};
use tod_store::{CredentialStore, resolve_linear_api_key};
use uuid::Uuid;

use tod_store::outline::types::Capability;
use tod_store::outline::{CreatePosition, OutlineMutation};

use crate::views::linear_import::{apply_linear_fields_to_node, parse_ticket_reference};

use super::TaskListView;
use super::credential_prompt::PendingCredentialRequest;

/// Outcome of starting a ticket import — may complete synchronously or fetch from Linear async.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketImportResult {
    Pending,
    Completed(bool),
}

pub(super) struct PendingTicketImport {
    pub generation: u64,
    pub ticket: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub error: Option<String>,
    pub draft_node_id: Option<String>,
    pub auth_failure: bool,
}

impl TaskListView {
    /// Link `ticket` to a new or draft node, or jump to an existing node with the same ticket.
    ///
    /// When `draft_node_id` is set (inline create), the empty draft row is updated or removed.
    /// When `None` (compose), a new sibling node is created after Linear fetch succeeds.
    pub(super) fn import_from_ticket(
        &mut self,
        ticket: &str,
        draft_node_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TicketImportResult {
        let Some(ticket) = parse_ticket_reference(ticket) else {
            return TicketImportResult::Completed(false);
        };
        let ticket = ticket.as_str();

        if let Some(existing) = self.find_by_ticket_id(ticket) {
            let id = existing.id.clone();
            if let Some(draft_id) = draft_node_id {
                if draft_id != id {
                    self.remove_outline_node(draft_id, window, cx);
                }
            }
            self.finish_ticket_import(window, cx);
            self.select_created_task(&id, window, cx);
            self.status_line = format!("Selected existing task for {ticket}");
            return TicketImportResult::Completed(true);
        }

        let store = CredentialStore::from_data_root(&self.config_dir);
        let Some(api_key) = resolve_linear_api_key(&store) else {
            self.open_linear_credential_prompt(
                PendingCredentialRequest {
                    ticket: ticket.to_string(),
                    draft_node_id: draft_node_id.map(str::to_string),
                },
                window,
                cx,
            );
            return TicketImportResult::Pending;
        };

        self.start_linear_ticket_fetch(&api_key, ticket, draft_node_id, window, cx);
        TicketImportResult::Pending
    }

    pub(super) fn create_task_with_title(
        &mut self,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.create_tree_node(CreatePosition::Below, window, cx) else {
            return;
        };
        let Ok(node_id) = Uuid::parse_str(&id) else {
            return;
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::UpdateNodeTitle {
                node_id,
                title: title.to_string(),
            })
        {
            self.show_error(format!("Failed to create task: {err}"), window, cx);
            return;
        }
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![Capability::Spec, Capability::Lifecycle],
            })
        {
            self.show_error(format!("Failed to create task: {err}"), window, cx);
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.show_error(format!("Failed to create task: {err}"), window, cx);
            return;
        }
        self.live_refresh(window, cx);
        self.select_created_task(&id, window, cx);
        self.status_line = format!("Created task: {title}");
    }

    pub(super) fn apply_pending_ticket_import(
        &mut self,
        pending: PendingTicketImport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ticket_import_generation != pending.generation {
            return;
        }
        if pending.auth_failure {
            let store = CredentialStore::from_data_root(&self.config_dir);
            let _ = store.delete(tod_store::CredentialKind::LinearApiKey);
            self.open_linear_credential_prompt(
                PendingCredentialRequest {
                    ticket: pending.ticket.clone(),
                    draft_node_id: pending.draft_node_id.clone(),
                },
                window,
                cx,
            );
            return;
        }
        if let Some(message) = pending.error {
            self.fail_ticket_import(&pending.ticket, message, window, cx);
            return;
        }
        let Some(title) = pending.title else {
            self.fail_ticket_import(
                &pending.ticket,
                "Linear returned no issue title".into(),
                window,
                cx,
            );
            return;
        };
        self.complete_ticket_import(
            &pending.ticket,
            &title,
            pending.description.as_deref(),
            pending.draft_node_id.as_deref(),
            window,
            cx,
        );
    }

    fn complete_ticket_import(
        &mut self,
        ticket: &str,
        title: &str,
        description: Option<&str>,
        draft_node_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let node_id = match draft_node_id {
            Some(id) => match Uuid::parse_str(id) {
                Ok(uuid) => uuid,
                Err(_) => {
                    self.fail_ticket_import(ticket, "Invalid draft node id".into(), window, cx);
                    return;
                }
            },
            None => match self.create_tree_node(CreatePosition::Below, window, cx) {
                Some(id) => match Uuid::parse_str(&id) {
                    Ok(uuid) => uuid,
                    Err(_) => {
                        self.fail_ticket_import(ticket, "Failed to create node".into(), window, cx);
                        return;
                    }
                },
                None => {
                    self.fail_ticket_import(ticket, "Failed to create node".into(), window, cx);
                    return;
                }
            },
        };

        if let Err(err) = self.apply_ticket_to_node(node_id, ticket, title, description) {
            self.fail_ticket_import(ticket, err, window, cx);
            return;
        }

        self.finish_ticket_import(window, cx);
        if self.compose_open {
            self.compose_open = false;
            self.selection_before_compose = None;
            self.compose_title_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        let id = node_id.to_string();
        self.live_refresh(window, cx);
        self.select_created_task(&id, window, cx);
        self.status_line = format!("Created task from {ticket}: {title}");
        cx.notify();
    }

    fn fail_ticket_import(
        &mut self,
        ticket: &str,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_error(format!("Failed to import {ticket}: {message}"), window, cx);
    }

    fn apply_ticket_to_node(
        &self,
        node_id: Uuid,
        ticket: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<(), String> {
        apply_linear_fields_to_node(
            &self.fleet,
            node_id,
            ticket,
            Some(title),
            description,
            Some(vec!["linear".into()]),
            true,
        )
    }

    fn finish_ticket_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_node_id = None;
        self.edit_open_for = None;
        self.edit_original_title = None;
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
        self.focus_handle.focus(window);
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
