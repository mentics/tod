use gpui::{AppContext, Context, ParentElement, Styled, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme, StyledExt, v_flex};
use tod_store::CredentialKind;
use uuid::Uuid;

use super::TaskListView;
use super::from_ticket::PendingTicketImport;

/// What the prompt should resume once the key has been stored. The prompt
/// itself only knows how to collect and save a credential; each entry point
/// says here what it was in the middle of doing.
#[derive(Clone, Debug)]
pub(super) enum PendingCredentialRequest {
    /// Import a Linear ticket, from the compose field or an inline edit. The
    /// new node goes under `parent`, whose Environment (or an ancestor's)
    /// holds the key.
    TicketImport {
        ticket: String,
        draft_node_id: Option<String>,
        parent: Uuid,
    },
    /// Refresh a generator node whose data source declared the key.
    GeneratorRefresh { node_id: Uuid },
}

impl PendingCredentialRequest {
    /// The node whose Environment the key is looked up in: the picker offers
    /// it and its ancestors.
    fn context_node(&self) -> Uuid {
        match self {
            Self::TicketImport { parent, .. } => *parent,
            Self::GeneratorRefresh { node_id } => *node_id,
        }
    }
}

impl TaskListView {
    pub(super) fn open_linear_credential_prompt(
        &mut self,
        request: PendingCredentialRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context = request.context_node();
        let chain = match self
            .fleet
            .read(|conn| tod_store::environment::ancestry(conn, context))
        {
            Ok(chain) => chain,
            Err(err) => {
                self.show_error(format!("Could not read the node's ancestry: {err:#}"), window, cx);
                return;
            }
        };
        self.credential_prompt_nodes = chain
            .into_iter()
            .map(|id| {
                let title = self
                    .fleet
                    .get_node(&id.to_string())
                    .ok()
                    .flatten()
                    .map(|node| node.title)
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| "Untitled".to_string());
                (id, title)
            })
            .collect();
        self.credential_prompt_choice = 0;
        self.credential_prompt_saving = false;
        self.credential_prompt_open = true;
        self.pending_credential_request = Some(request);
        self.credential_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.status_line = "Linear API key required".into();
        cx.notify();
    }

    pub(super) fn cancel_credential_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.credential_prompt_open {
            return;
        }
        self.credential_prompt_open = false;
        self.credential_prompt_saving = false;
        self.credential_prompt_nodes.clear();
        self.pending_credential_request = None;
        self.credential_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.status_line.clear();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn submit_credential_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.credential_prompt_saving {
            return;
        }
        let Some(request) = self.pending_credential_request.clone() else {
            return;
        };
        let secret = self
            .credential_input
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string();
        if secret.is_empty() {
            crate::ui::toast::error_toast(window, cx, "Enter a Linear API key");
            self.credential_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return;
        }
        let Some((node, title)) = self.credential_prompt_nodes.get(self.credential_prompt_choice).cloned() else {
            crate::ui::toast::error_toast(window, cx, "Pick the node to keep the Linear API key on");
            return;
        };

        // Storing the value goes to the OS keyring, and the entry (with the
        // Environment capability, if the node lacks it) is written and waited
        // for, so the resumed work finds it.
        self.credential_prompt_saving = true;
        cx.notify();
        let (fleet, root) = (self.fleet.clone(), self.config_dir.clone());
        let stored = secret.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tod_store::environment::store_linear_api_key(&fleet, &root, node, &stored)
                        .map_err(|err| format!("{err:#}"))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.credential_prompt_saving = false;
                match result {
                    Ok(()) => {
                        this.credential_prompt_open = false;
                        this.credential_prompt_nodes.clear();
                        this.pending_credential_request = None;
                        this.credential_input.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                        });
                        this.status_line = format!("Saved Linear API key to {title}'s Environment");
                        match request {
                            PendingCredentialRequest::TicketImport { ticket, draft_node_id, parent } => {
                                this.start_linear_ticket_fetch(
                                    &secret,
                                    &ticket,
                                    draft_node_id.as_deref(),
                                    parent,
                                    window,
                                    cx,
                                );
                            }
                            PendingCredentialRequest::GeneratorRefresh { node_id } => {
                                this.start_generator_refresh(node_id, window, cx);
                            }
                        }
                    }
                    Err(err) => {
                        crate::ui::toast::error_toast(
                            window,
                            cx,
                            format!("Failed to store Linear API key: {err}"),
                        );
                        this.credential_input.update(cx, |input, cx| {
                            input.focus(window, cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_credential_prompt_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let theme = cx.theme();
        let kind = CredentialKind::LinearApiKey;
        let choice = self.credential_prompt_choice;
        let saving = self.credential_prompt_saving;
        let nearest = match &self.pending_credential_request {
            Some(PendingCredentialRequest::GeneratorRefresh { .. }) => "this node",
            _ => "the new node's parent",
        };
        let nodes = div().flex().flex_col().gap_1().children(
            self.credential_prompt_nodes
                .iter()
                .enumerate()
                .map(|(ix, (_, title))| {
                    let label = match ix {
                        0 => format!("{title} ({nearest})"),
                        _ => title.clone(),
                    };
                    let button = Button::new(("credential-node", ix)).label(label).w_full();
                    let button = if ix == choice { button.primary() } else { button.ghost() };
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        this.credential_prompt_choice = ix;
                        cx.notify();
                    }))
                }),
        );
        div()
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.45))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(420.))
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .shadow_lg()
                    .p_4()
                    .child(
                        v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .child(format!("Enter {}", kind.label())),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        "Used to reach Linear on your behalf: importing tickets, refreshing generator nodes, and agents' own Linear calls. It is kept in a node's Environment, and every node below that node uses it.",
                                    ),
                            )
                            .child(div().text_sm().font_semibold().child("Keep it on"))
                            .child(nodes)
                            .child(Input::new(&self.credential_input).w_full())
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("credential-cancel")
                                            .label("Cancel")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.cancel_credential_prompt(window, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("credential-save")
                                            .label("Save")
                                            .primary()
                                            .loading(saving)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.submit_credential_prompt(window, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
    }
}

impl TaskListView {
    pub(super) fn start_linear_ticket_fetch(
        &mut self,
        api_key: &str,
        ticket: &str,
        draft_node_id: Option<&str>,
        parent: Uuid,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft_node_id = draft_node_id.map(str::to_string);
        let ticket_owned = ticket.to_string();
        let ticket_for_fetch = ticket_owned.clone();
        let api_key = api_key.to_string();

        self.ticket_import_generation = self.ticket_import_generation.wrapping_add(1);
        let generation = self.ticket_import_generation;
        self.status_line = format!("Fetching {ticket} from Linear…");
        cx.notify();

        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            // `background_spawn`, never a thread we join here: this future runs
            // on the foreground executor, so blocking in it would freeze the UI
            // for the whole round trip.
            let fetch = cx
                .background_spawn(async move {
                    tod_store::linear::fetch_issue(&api_key, &ticket_for_fetch)
                })
                .await;
            let pending = match fetch {
                Ok(issue) => PendingTicketImport {
                    generation,
                    ticket: issue.identifier,
                    title: Some(issue.title),
                    description: issue.description,
                    error: None,
                    draft_node_id: draft_node_id.clone(),
                    parent,
                    auth_failure: false,
                },
                Err(err) => PendingTicketImport {
                    generation,
                    ticket: ticket_owned.clone(),
                    title: None,
                    description: None,
                    error: Some(err.to_string()),
                    draft_node_id: draft_node_id.clone(),
                    parent,
                    auth_failure: matches!(
                        err,
                        tod_store::linear::LinearError::Api(ref msg)
                            if msg.contains("Invalid Linear API key")
                    ),
                },
            };
            let _ = entity.update(cx, |this, cx| {
                if this.ticket_import_generation != generation {
                    return;
                }
                this.pending_ticket_import = Some(pending);
                cx.notify();
            });
        })
        .detach();
    }
}
