use gpui::{Context, ParentElement, Styled, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme, StyledExt, v_flex};
use tod_store::CredentialKind;

use super::TaskListView;
use super::from_ticket::PendingTicketImport;

#[derive(Clone, Debug)]
pub(super) struct PendingCredentialRequest {
    pub ticket: String,
    pub draft_node_id: Option<String>,
}

impl TaskListView {
    pub(super) fn open_linear_credential_prompt(
        &mut self,
        request: PendingCredentialRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.pending_credential_request = None;
        self.credential_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.status_line.clear();
        self.focus_handle.focus(window);
        cx.notify();
    }

    pub(super) fn submit_credential_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

        let store = tod_store::CredentialStore::from_data_root(&self.config_dir);
        match store.set(CredentialKind::LinearApiKey, &secret) {
            Ok(backend) => {
                self.credential_prompt_open = false;
                self.pending_credential_request = None;
                self.credential_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.status_line = match backend {
                    tod_store::CredentialBackend::Keyring => {
                        "Saved Linear API key to OS keyring".into()
                    }
                    tod_store::CredentialBackend::EncryptedFile => {
                        "Saved Linear API key to encrypted credentials file".into()
                    }
                    tod_store::CredentialBackend::Environment => "Saved Linear API key".into(),
                };
                self.start_linear_ticket_fetch(
                    &secret,
                    &request.ticket,
                    request.draft_node_id.as_deref(),
                    window,
                    cx,
                );
            }
            Err(err) => {
                crate::ui::toast::error_toast(
                    window,
                    cx,
                    format!("Failed to store Linear API key: {err}"),
                );
                self.credential_input.update(cx, |input, cx| {
                    input.focus(window, cx);
                });
            }
        }
    }

    pub(super) fn render_credential_prompt_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let theme = cx.theme();
        let kind = CredentialKind::LinearApiKey;
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
                                        "Used to fetch issue titles from Linear. Stored in your OS keyring when available, otherwise an encrypted local credentials file.",
                                    ),
                            )
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
            let fetch = std::thread::spawn(move || {
                tod_store::linear::fetch_issue(&api_key, &ticket_for_fetch)
            })
            .join();
            let pending = match fetch {
                Ok(Ok(issue)) => PendingTicketImport {
                    generation,
                    ticket: issue.identifier,
                    title: Some(issue.title),
                    error: None,
                    draft_node_id: draft_node_id.clone(),
                    auth_failure: false,
                },
                Ok(Err(err)) => PendingTicketImport {
                    generation,
                    ticket: ticket_owned.clone(),
                    title: None,
                    error: Some(err.to_string()),
                    draft_node_id: draft_node_id.clone(),
                    auth_failure: matches!(
                        err,
                        tod_store::linear::LinearError::Api(ref msg)
                            if msg.contains("Invalid Linear API key")
                    ),
                },
                Err(_) => PendingTicketImport {
                    generation,
                    ticket: ticket_owned.clone(),
                    title: None,
                    error: Some("Linear fetch thread panicked".into()),
                    draft_node_id: draft_node_id.clone(),
                    auth_failure: false,
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
