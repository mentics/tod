//! The GitHub token's health, kept in view.
//!
//! Every GitHub call (`tod-cli pr`, the pull request views, the gates, the PR
//! babysitter) uses the one token tod holds. When it is missing or GitHub
//! rejects it all of them fail, and an agent can only report that. So the
//! shell checks it at startup and every few minutes ([`GithubTokenMonitor`]),
//! shows a red banner at the top of the window while it needs the user
//! ([`render_banner`]), and the banner's button opens a dialog that tests a
//! new token against GitHub before storing it ([`open_dialog`]).

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::Focus;
use tod_store::github_token::{self, NEW_TOKEN_URL, TokenStatus};
use tod_store::{CredentialKind, CredentialStore};

use crate::ui::journey::{Source, record_action};
use crate::ui::selectable_text::selectable_text;

const JOURNEY_SURFACE: &str = "github_token";
const ACT_OPEN: &str = "fix_github_token";
const ACT_SAVE: &str = "save";
const ACT_CREATE: &str = "create_token";
const ACT_RECHECK: &str = "recheck";
const ACT_CANCEL: &str = "cancel";

/// How often a token that looked fine is checked again (a revoked one is
/// noticed within this).
const RECHECK_EVERY: Duration = Duration::from_secs(5 * 60);

/// Holds the last [`TokenStatus`]; `None` until the first check is back.
pub struct GithubTokenMonitor {
    data_root: PathBuf,
    status: Option<TokenStatus>,
    checking: bool,
}

impl GithubTokenMonitor {
    /// Checks now, then every [`RECHECK_EVERY`].
    pub fn start(data_root: PathBuf, cx: &mut App) -> Entity<Self> {
        let monitor = cx.new(|_| Self { data_root, status: None, checking: false });
        let weak = monitor.downgrade();
        cx.spawn(async move |cx| {
            loop {
                if weak.update(cx, |this, cx| this.recheck(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(RECHECK_EVERY).await;
            }
        })
        .detach();
        monitor
    }

    pub fn status(&self) -> Option<&TokenStatus> {
        self.status.as_ref()
    }

    /// Ask GitHub again, off the UI thread (it is a network call).
    pub fn recheck(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        cx.notify();
        let root = self.data_root.clone();
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async move { github_token::check(&CredentialStore::from_data_root(&root)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if status.needs_fixing() {
                    tracing::warn!("github token: {}", status.headline());
                }
                this.status = Some(status);
                this.checking = false;
                cx.notify();
            });
        })
        .detach();
    }
}

/// The red banner, when the token needs the user; nothing otherwise.
pub fn render_banner(
    monitor: &Entity<GithubTokenMonitor>,
    window: &mut Window,
    cx: &mut App,
) -> Option<gpui::AnyElement> {
    let this = monitor.read(cx);
    let status = this.status.clone().filter(TokenStatus::needs_fixing)?;
    let checking = this.checking;
    let label = if matches!(status, TokenStatus::Missing { .. }) { "Add token" } else { "Replace token" };
    let (open_monitor, recheck_monitor) = (monitor.clone(), monitor.clone());
    let help = status.help();
    let text = gpui::white();
    Some(
        h_flex()
            .w_full()
            .flex_shrink_0()
            .gap_3()
            .px_4()
            .py_2()
            .items_center()
            .bg(gpui::red())
            .text_color(text)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        selectable_text("github-token-banner", status.headline(), window, cx)
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(text)
                            .whitespace_normal(),
                    )
                    .when(!help.is_empty(), |el| {
                        el.child(
                            selectable_text("github-token-banner-help", help, window, cx)
                                .text_xs()
                                .text_color(text)
                                .whitespace_normal(),
                        )
                    }),
            )
            .child(
                Button::new("github-token-recheck")
                    .label(if checking { "Checking…" } else { "Recheck" })
                    .small()
                    .disabled(checking)
                    .on_click(move |_, _, cx| {
                        record(cx, ACT_RECHECK, &Presented::default());
                        recheck_monitor.update(cx, |m, cx| m.recheck(cx));
                    }),
            )
            .child(
                Button::new("github-token-fix")
                    .label(label)
                    .small()
                    .primary()
                    .on_click(move |_, window, cx| {
                        record(cx, ACT_OPEN, &banner_presented(label));
                        open_dialog(open_monitor.clone(), window, cx);
                    }),
            )
            .into_any_element(),
    )
}

fn banner_presented(fix_label: &str) -> Presented {
    Presented {
        actions: vec![
            PresentedAction { id: ACT_OPEN.into(), label: fix_label.into(), primary: true, disabled: false },
            PresentedAction { id: ACT_RECHECK.into(), label: "Recheck".into(), primary: false, disabled: false },
        ],
        focused: None,
        notices: vec!["GitHub token needs fixing".into()],
    }
}

fn record(cx: &mut App, action: &str, presented: &Presented) {
    record_action(cx, Focus::Project, action, Source::Click, JOURNEY_SURFACE, presented.clone());
}

/// The dialog that tests a token against GitHub and stores it.
pub fn open_dialog(monitor: Entity<GithubTokenMonitor>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| TokenDialog::new(monitor, window, cx));
    window.open_dialog(cx, move |d, _, _| {
        d.title("GitHub token")
            .w(px(520.))
            .overlay(true)
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .child(view.clone())
    });
}

struct TokenDialog {
    monitor: Entity<GithubTokenMonitor>,
    input: Entity<InputState>,
    working: bool,
    /// The last failure, or what was saved.
    status: Option<(bool, String)>,
}

impl TokenDialog {
    fn new(monitor: Entity<GithubTokenMonitor>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder("ghp_… or github_pat_… (write-only)")
        });
        Self { monitor, input, working: false, status: None }
    }

    /// Test the typed token against GitHub; store it only if GitHub accepts it.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        record(cx, ACT_SAVE, &Presented::default());
        let token = self.input.read(cx).value().trim().to_string();
        if token.is_empty() {
            self.status = Some((false, "Paste the token first.".into()));
            cx.notify();
            return;
        }
        self.working = true;
        self.status = None;
        cx.notify();
        let root = self.monitor.read(cx).data_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let login = github_token::verify_token(&token).map_err(|err| err.to_string())?;
                    let backend = CredentialStore::from_data_root(&root)
                        .set(CredentialKind::GithubToken, &token)
                        .map_err(|err| format!("GitHub accepted the token but it could not be stored: {err}"))?;
                    Ok::<_, String>((login, backend))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.working = false;
                match outcome {
                    Ok((login, backend)) => {
                        tracing::info!("github token: stored for {login} ({backend:?})");
                        this.monitor.update(cx, |m, cx| m.recheck(cx));
                        window.close_dialog(cx);
                    }
                    Err(message) => this.status = Some((false, format!("Not saved: {message}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for TokenDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let busy = self.working;
        let current = self.monitor.read(cx).status().cloned();
        v_flex()
            .gap_2()
            .when_some(current.filter(TokenStatus::needs_fixing), |el, status| {
                el.child(
                    selectable_text("github-token-dialog-state", status.headline(), window, cx)
                        .text_sm()
                        .text_color(danger)
                        .whitespace_normal(),
                )
            })
            .child(
                div().text_sm().text_color(muted).child(
                    "Agents use this token for pull requests: reading them, opening them, \
                     answering review threads. A classic token needs the `repo` scope; a \
                     fine-grained one needs Pull requests and Contents read/write on the \
                     repositories. It is checked against GitHub before it is stored, and never \
                     shown again.",
                ),
            )
            .child(Input::new(&self.input).small().disabled(busy))
            .when_some(self.status.clone(), |el, (ok, text)| {
                let color = if ok { cx.theme().success } else { cx.theme().danger };
                el.child(
                    selectable_text(SharedString::from("github-token-dialog-status"), text, window, cx)
                        .text_sm()
                        .text_color(color)
                        .whitespace_normal(),
                )
            })
            .when(busy, |el| el.child(div().text_sm().text_color(muted).child("Checking with GitHub…")))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("github-token-create")
                            .label("Create a token on GitHub")
                            .on_click(|_, _, cx| {
                                record(cx, ACT_CREATE, &Presented::default());
                                cx.open_url(NEW_TOKEN_URL);
                            }),
                    )
                    .child(
                        Button::new("github-token-cancel")
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(cx.listener(|_, _, window, cx| {
                                record(cx, ACT_CANCEL, &Presented::default());
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("github-token-save")
                            .label("Test and save")
                            .primary()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
