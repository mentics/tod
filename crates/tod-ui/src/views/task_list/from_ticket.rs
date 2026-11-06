use gpui::{Context, Window};
use tod_store::CredentialStore;
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
    /// The node the new one goes under; its Environment holds the key.
    pub parent: Uuid,
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
            self.finish_ticket_import(window, cx);
            self.select_created_task(&id, window, cx);
            self.status_line = format!("Selected existing task for {ticket}");
            return TicketImportResult::Completed(true);
        }

        // The key is in the Environment of the node the new one goes under (or
        // an ancestor of it), so a top-level node has nowhere to look.
        let Some(parent) = self.ticket_import_parent(draft_node_id) else {
            self.show_error(
                "Add the ticket under a node: the Linear API key is kept in a node's Environment",
                window,
                cx,
            );
            return TicketImportResult::Completed(false);
        };
        let api_key = match tod_store::environment::linear_api_key_for(&self.fleet, parent) {
            Ok(Some(key)) => key,
            Ok(None) => {
                self.open_linear_credential_prompt(
                    PendingCredentialRequest::TicketImport {
                        ticket: ticket.to_string(),
                        draft_node_id: draft_node_id.map(str::to_string),
                        parent,
                    },
                    window,
                    cx,
                );
                return TicketImportResult::Pending;
            }
            Err(err) => {
                self.show_error(format!("Could not read the Linear API key: {err:#}"), window, cx);
                return TicketImportResult::Completed(false);
            }
        };

        self.start_linear_ticket_fetch(&api_key, ticket, draft_node_id, parent, window, cx);
        TicketImportResult::Pending
    }

    /// The node an imported ticket's new node goes under: the draft's parent
    /// (or, for a sibling, its anchor's parent), else the selected node's
    /// parent for a ticket pasted into the compose field. `None` when the new
    /// node would be top level.
    fn ticket_import_parent(&self, draft_node_id: Option<&str>) -> Option<Uuid> {
        let parent_of = |id: Uuid| {
            let id = id.to_string();
            self.all_tasks
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| t.parent_id.as_deref())
                .and_then(|p| Uuid::parse_str(p).ok())
        };
        let draft = draft_node_id
            .filter(|id| self.is_draft_id(id))
            .and_then(|_| self.draft.as_ref());
        match draft {
            Some(draft) => draft.parent_id.or_else(|| draft.anchor_id.and_then(parent_of)),
            None => self
                .working_set
                .selected_id
                .as_deref()
                .filter(|id| !self.is_draft_id(id))
                .and_then(|id| Uuid::parse_str(id).ok())
                .and_then(parent_of),
        }
    }

    pub(super) fn create_task_with_title(
        &mut self,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.create_tree_node(CreatePosition::Below, title, window, cx) else {
            return;
        };
        let Ok(node_id) = Uuid::parse_str(&id) else {
            return;
        };
        let title = title.to_string();
        self.write_outline(
            OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![Capability::Spec, Capability::Lifecycle],
            },
            window,
            cx,
            move |this, result, window, cx| {
                if let Err(err) = result {
                    this.show_error(format!("Failed to create task: {err}"), window, cx);
                    return;
                }
                this.live_refresh(window, cx);
                this.select_created_task(&id, window, cx);
                this.status_line = format!("Created task: {title}");
            },
        );
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
            // Linear refused the key: forget the value (the entry stays, so the
            // prompt replaces it) wherever it was defined.
            let source = self.fleet.read(|conn| {
                Ok(tod_store::environment::resolve(conn, pending.parent)?
                    .into_iter()
                    .find(|r| r.entry.name.eq_ignore_ascii_case(tod_store::environment::LINEAR_ENTRY))
                    .map(|r| r.source_node))
            });
            match source {
                Ok(Some(node)) => tod_store::environment::forget_secret(
                    &CredentialStore::from_data_root(&self.config_dir),
                    node,
                    tod_store::environment::LINEAR_ENTRY,
                ),
                Ok(None) => {}
                Err(err) => {
                    self.show_error(format!("Could not clear the rejected Linear API key: {err:#}"), window, cx);
                    return;
                }
            }
            self.open_linear_credential_prompt(
                PendingCredentialRequest::TicketImport {
                    ticket: pending.ticket.clone(),
                    draft_node_id: pending.draft_node_id.clone(),
                    parent: pending.parent,
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
        // The node is created only now, titled from the ticket, so its slug is too.
        let draft = draft_node_id
            .filter(|id| self.is_draft_id(id))
            .and_then(|_| self.draft.take());
        let created = match draft {
            Some(draft) => {
                let created = self.create_node_at(&draft, title, window, cx);
                if created.is_none() {
                    self.draft = Some(draft);
                }
                created
            }
            None => self.create_tree_node(CreatePosition::Below, title, window, cx),
        };
        let Some(node_id) = created.and_then(|id| Uuid::parse_str(&id).ok()) else {
            self.fail_ticket_import(ticket, "Failed to create node".into(), window, cx);
            return;
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
        self.draft = None;
        self.edit_open_for = None;
        self.edit_original_title = None;
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
        self.reload_all_tasks();
        self.focus_handle.focus(window, cx);
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
        let visible = Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set, &self.actor_view);
        if visible.iter().any(|t| t.id == task_id) {
            self.select_task_by_id(task_id, window, cx);
        }
        self.rebuild_visible_list(window, cx);
    }
}
