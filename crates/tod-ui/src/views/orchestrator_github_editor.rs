//! Settings → "Orchestrator GitHub access": the GitHub tokens the cloud
//! orchestrator reads with, one per organization, each for the repositories
//! the user lists (`tod_store::orchestrator_github`).
//!
//! Token values are write-only: typed into a masked input, stored in the
//! credential store, never read back for display (leaving the input empty
//! keeps the stored one). Saving writes the list and the token only; nothing
//! reaches the orchestrator until Apply (`tod_core::orchestrator_github::apply`,
//! which blocks on the network and so runs on the background executor).
//! Everything that touches the credential store or the network runs off the
//! UI thread.
//!
//! Focus: Enter on the section's stop in Settings focuses this editor, where
//! Enter adds a token and the mouse reaches the rest. A token's form is the
//! edit mode (its inputs exist only while it is open, and every one is
//! single-line): Enter saves, Escape closes it. Escape with no form open
//! hands focus back to Settings. The bindings sit one context deeper than
//! Settings' own Enter/Escape ones and are registered after them, so they win
//! (`register_keyboard_bindings`).

use crate::ui::journey::{self, Source};
use crate::ui::selectable_text::selectable_text;
use gpui::{
    actions, App, AppContext, Context, KeyBinding, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::path::PathBuf;
use tod_store::orchestrator_github::{self as store, Config, Entry};

/// Asks Settings to take keyboard focus back (Escape with no form open).
pub struct Leave;

const CONTEXT: &str = "OrchestratorGithub";

actions!(orchestrator_github, [OrchGhAdd, OrchGhSave, OrchGhEscape]);

/// Enter and Escape for this editor. Call after Settings' own bindings are
/// registered: ties in context depth go to the later binding.
pub fn register_keyboard_bindings(cx: &mut App) {
    let outside_input: &'static str = Box::leak(format!("{CONTEXT} && !Input").into_boxed_str());
    let in_input: &'static str = Box::leak(format!("{CONTEXT} > Input").into_boxed_str());
    cx.bind_keys([
        KeyBinding::new("enter", OrchGhAdd, Some(outside_input)),
        KeyBinding::new("escape", OrchGhEscape, Some(outside_input)),
        KeyBinding::new("enter", OrchGhSave, Some(in_input)),
        KeyBinding::new("escape", OrchGhEscape, Some(in_input)),
    ]);
}

/// The repositories text (one per line, or comma or space separated) as
/// patterns: trimmed, empty pieces dropped, duplicates dropped, each checked.
pub fn parse_repos(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for piece in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        store::check_pattern(piece)?;
        if !out.iter().any(|p| p == piece) {
            out.push(piece.to_string());
        }
    }
    if out.is_empty() {
        return Err("List at least one repository (owner/repo, owner/*, or *).".to_string());
    }
    Ok(out)
}

/// What a form describes, ready to save, or what is wrong with it.
#[derive(Debug, PartialEq, Eq)]
pub struct Checked {
    pub config: Config,
    /// The id of the entry the form saved.
    pub id: String,
}

/// The config after the form's entry replaces `original` (or is added),
/// checked as a whole. A new entry needs its token: without one it could
/// never be applied.
pub fn check_form(
    current: &Config,
    original: Option<&str>,
    label: &str,
    repos_text: &str,
    typed_token: bool,
    stored_token: bool,
) -> Result<Checked, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("Give the token a label, e.g. the organization it is for.".to_string());
    }
    let repos = parse_repos(repos_text)?;
    if !typed_token && !(original.is_some() && stored_token) {
        return Err("Paste the token: none is stored for this entry yet.".to_string());
    }
    let mut config = current.clone();
    let id = match original {
        Some(id) => id.to_string(),
        None => store::new_id(label, &config.tokens),
    };
    let entry = Entry { id: id.clone(), label: label.to_string(), repos };
    match config.tokens.iter_mut().find(|e| e.id == id) {
        Some(slot) => *slot = entry,
        None => config.tokens.push(entry),
    }
    config.check().map_err(|e| format!("{e:#}"))?;
    Ok(Checked { config, id })
}

/// "N tokens, M routes", with the right plurals.
pub fn applied_summary(tokens: usize, routes: usize) -> String {
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    format!("{}, {}", plural(tokens, "token", "tokens"), plural(routes, "route", "routes"))
}

struct Form {
    /// The id of the entry being edited; `None` for a new one.
    original: Option<String>,
    label: Entity<InputState>,
    repos: Entity<InputState>,
    token: Entity<InputState>,
    saving: bool,
}

pub struct OrchestratorGithubEditor {
    data_root: PathBuf,
    config: Config,
    /// Whether each entry's token is stored; filled off the UI thread (the
    /// keyring can prompt). `None` until read.
    has_token: Option<HashMap<String, bool>>,
    loaded: bool,
    form: Option<Form>,
    error: Option<String>,
    applying: bool,
    apply_status: Option<Result<String, String>>,
    focus_handle: FocusHandle,
}

impl EventEmitter<Leave> for OrchestratorGithubEditor {}

impl OrchestratorGithubEditor {
    pub fn new(data_root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            data_root,
            config: Config::default(),
            has_token: None,
            loaded: false,
            form: None,
            error: None,
            applying: false,
            apply_status: None,
            focus_handle: cx.focus_handle(),
        };
        this.reload(cx);
        this
    }

    /// Re-read the list and which tokens are stored, off the UI thread.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let root = self.data_root.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let config = Config::load(&root).map_err(|e| format!("{e:#}"))?;
                    let has = config
                        .tokens
                        .iter()
                        .map(|e| (e.id.clone(), store::has_token(&root, &e.id)))
                        .collect::<HashMap<_, _>>();
                    Ok::<_, String>((config, has))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = true;
                match loaded {
                    Ok((config, has)) => {
                        this.config = config;
                        this.has_token = Some(has);
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.form {
            Some(form) => form.label.update(cx, |s, cx| s.focus(window, cx)),
            None => self.focus_handle.focus(window, cx),
        }
    }

    fn stored(&self, id: &str) -> bool {
        self.has_token.as_ref().and_then(|m| m.get(id).copied()).unwrap_or(false)
    }

    /// A `UserAction` with what was on screen: the buttons of the list or of
    /// the open form, Apply primary while the list is shown.
    fn record(&self, cx: &mut gpui::App, action: &str, source: Source) {
        let button = |id: &str, label: &str, primary: bool, disabled: bool| tod_journey::PresentedAction {
            id: id.to_string(),
            label: label.to_string(),
            primary,
            disabled,
        };
        let actions = if self.form.is_some() {
            vec![
                button("orchestrator_github_save", "Save", true, self.form.as_ref().is_some_and(|f| f.saving)),
                button("orchestrator_github_cancel", "Cancel", false, false),
            ]
        } else {
            let mut v = vec![button("orchestrator_github_add", "Add a token", self.config.tokens.is_empty(), false)];
            for e in &self.config.tokens {
                v.push(button(&format!("orchestrator_github_edit:{}", e.id), &format!("Edit {}", e.label), false, false));
                v.push(button(&format!("orchestrator_github_remove:{}", e.id), &format!("Remove {}", e.label), false, false));
            }
            v.push(button(
                "orchestrator_github_apply",
                "Apply to the orchestrator",
                !self.config.tokens.is_empty(),
                self.applying || !self.loaded,
            ));
            v
        };
        let presented = tod_journey::Presented {
            actions,
            focused: Some(action.to_string()),
            notices: Vec::new(),
        };
        journey::record_action(
            cx,
            tod_store::conversation::Focus::Project,
            action,
            source,
            "settings_orchestrator_github",
            presented,
        );
    }

    fn open_form(&mut self, original: Option<Entry>, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let action = if original.is_some() { "orchestrator_github_edit" } else { "orchestrator_github_add" };
        self.record(cx, action, source);
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("Label, e.g. the organization (Acme)"));
        let repos = cx.new(|cx| InputState::new(window, cx).placeholder("acme/api, acme/*, or *"));
        let stored = original.as_ref().is_some_and(|e| self.stored(&e.id));
        let token = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(if stored {
                "A token is stored. Leave empty to keep it, or paste a new one"
            } else {
                "Paste the GitHub token (write-only)"
            })
        });
        if let Some(e) = &original {
            label.update(cx, |s, cx| s.set_value(e.label.clone(), window, cx));
            repos.update(cx, |s, cx| s.set_value(e.repos.join(", "), window, cx));
        }
        label.update(cx, |s, cx| s.focus(window, cx));
        self.form = Some(Form { original: original.map(|e| e.id), label, repos, token, saving: false });
        self.error = None;
        cx.notify();
    }

    fn close_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form = None;
        self.error = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn save(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else { return };
        if form.saving {
            return;
        }
        let label = form.label.read(cx).text().to_string();
        let repos = form.repos.read(cx).text().to_string();
        let typed: String = form.token.read(cx).text().to_string();
        let typed: String = typed.trim().chars().filter(|c| !c.is_control()).collect();
        let original = form.original.clone();
        let stored = original.as_deref().is_some_and(|id| self.stored(id));
        let checked = match check_form(&self.config, original.as_deref(), &label, &repos, !typed.is_empty(), stored) {
            Ok(c) => c,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.record(cx, "orchestrator_github_save", source);
        if let Some(form) = self.form.as_mut() {
            form.saving = true;
        }
        self.error = None;
        let root = self.data_root.clone();
        let Checked { config, id } = checked;
        let saved_id = id.clone();
        let saved_config = config.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if !typed.is_empty() {
                        store::set_token(&root, &id, &typed).map_err(|e| format!("Storing the token failed: {e}"))?;
                    }
                    config.save(&root).map_err(|e| format!("Saving the list failed: {e:#}"))?;
                    Ok::<_, String>(store::has_token(&root, &id))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(has) => {
                    this.config = saved_config;
                    this.has_token.get_or_insert_with(HashMap::new).insert(saved_id, has);
                    this.apply_status = Some(Ok(
                        "Saved. Nothing reaches the orchestrator until you press Apply.".to_string()
                    ));
                    this.close_form(window, cx);
                }
                Err(e) => {
                    if let Some(form) = this.form.as_mut() {
                        form.saving = false;
                    }
                    this.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        self.record(cx, "orchestrator_github_remove", Source::Click);
        let mut config = self.config.clone();
        config.tokens.retain(|e| e.id != id);
        let root = self.data_root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let (config, id, root) = (config.clone(), id.clone(), root);
                    async move {
                        config.save(&root).map_err(|e| format!("Saving the list failed: {e:#}"))?;
                        store::delete_token(&root, &id);
                        Ok::<_, String>(())
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.config = config;
                        if let Some(m) = this.has_token.as_mut() {
                            m.remove(&id);
                        }
                        this.error = None;
                        this.apply_status = Some(Ok(
                            "Removed. The orchestrator still holds it until you press Apply.".to_string()
                        ));
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.applying || !self.loaded {
            return;
        }
        self.record(cx, "orchestrator_github_apply", source);
        self.applying = true;
        self.apply_status = None;
        cx.notify();
        let root = self.data_root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tod_core::orchestrator_github::apply(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.applying = false;
                this.apply_status = Some(match result {
                    Ok(applied) => Ok(format!(
                        "Applied: {}.",
                        applied_summary(applied.tokens, applied.routes)
                    )),
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn render_row(&self, entry: &Entry, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let stored = match &self.has_token {
            None => "checking the token…",
            Some(_) if self.stored(&entry.id) => "token stored",
            Some(_) => "no token stored",
        };
        let id = entry.id.clone();
        let edit_entry = entry.clone();
        v_flex()
            .gap_0p5()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().font_semibold().child(selectable_text(
                        format!("orch-gh-label-{}", entry.id),
                        entry.label.clone(),
                        window,
                        cx,
                    )))
                    .child(div().text_xs().text_color(muted).child(stored))
                    .child(div().flex_1())
                    .child(
                        Button::new(SharedStringId::edit(&entry.id))
                            .label("Edit")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_form(Some(edit_entry.clone()), Source::Click, window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedStringId::remove(&entry.id))
                            .label("Remove")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(move |this, _, _, cx| this.remove(id.clone(), cx))),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(selectable_text(
                format!("orch-gh-repos-{}", entry.id),
                entry.repos.join(", "),
                window,
                cx,
            )))
            .into_any_element()
    }

    fn render_form(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(form) = &self.form else { return div().into_any_element() };
        let field = |label: &'static str, child: gpui::AnyElement| {
            v_flex().gap_1().w_full().child(div().text_xs().font_semibold().child(label)).child(child)
        };
        let mut col = v_flex()
            .gap_2()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().list_active_border)
            .child(field("Label", Input::new(&form.label).into_any_element()))
            .child(field(
                "Repositories this token is for (comma separated: owner/repo, owner/*, or *)",
                Input::new(&form.repos).into_any_element(),
            ))
            .child(field(
                if form.original.is_some() { "Token (leave empty to keep the stored one)" } else { "Token" },
                Input::new(&form.token).into_any_element(),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("orch-gh-save")
                            .label(if form.saving { "Saving…" } else { "Save" })
                            .primary()
                            .compact()
                            .disabled(form.saving)
                            .on_click(cx.listener(|this, _, window, cx| this.save(Source::Click, window, cx))),
                    )
                    .child(
                        Button::new("orch-gh-cancel")
                            .label("Cancel")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.record(cx, "orchestrator_github_cancel", Source::Click);
                                this.close_form(window, cx)
                            })),
                    ),
            );
        if let Some(err) = &self.error {
            col = col.child(div().text_xs().text_color(cx.theme().danger).child(selectable_text(
                "orch-gh-form-error",
                err.clone(),
                window,
                cx,
            )));
        }
        col.into_any_element()
    }
}

/// Button ids that stay distinct per entry.
struct SharedStringId;
impl SharedStringId {
    fn edit(id: &str) -> gpui::SharedString {
        format!("orch-gh-edit-{id}").into()
    }
    fn remove(id: &str) -> gpui::SharedString {
        format!("orch-gh-remove-{id}").into()
    }
}

impl Render for OrchestratorGithubEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let tokens = self.config.tokens.clone();
        let mut col = v_flex()
            .id("orchestrator-github-editor")
            .track_focus(&self.focus_handle)
            .gap_2()
            .w_full()
            .max_w(px(720.))
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &OrchGhAdd, window, cx| {
                if this.form.is_none() && this.loaded {
                    this.open_form(None, Source::Keyboard, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OrchGhSave, window, cx| {
                this.save(Source::Keyboard, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OrchGhEscape, window, cx| {
                if this.form.is_some() {
                    this.record(cx, "orchestrator_github_cancel", Source::Keyboard);
                    this.close_form(window, cx);
                } else {
                    cx.emit(Leave);
                }
            }))
            // Settings focuses itself on any click; the inputs here must keep theirs.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_sm().text_color(muted).child(
                "While a node waits, the orchestrator reads GitHub for it (merges, releases) so its sandbox need not wake. \
                 A GitHub token covers one organization, so add one token per organization and list the repositories each is for: \
                 owner/repo, owner/*, or * (the most specific match wins). \
                 Saving keeps the list and the tokens here; nothing reaches the orchestrator until you press Apply.",
            ));
        if !self.loaded {
            col = col.child(div().text_xs().text_color(muted).child("Loading…"));
        } else if tokens.is_empty() && self.form.is_none() {
            col = col.child(div().text_xs().text_color(muted).child("No tokens yet."));
        }
        for entry in &tokens {
            // The entry being edited is shown as its form instead.
            if self.form.as_ref().and_then(|f| f.original.as_deref()) == Some(entry.id.as_str()) {
                col = col.child(self.render_form(window, cx));
            } else {
                col = col.child(self.render_row(entry, window, cx));
            }
        }
        if self.form.as_ref().is_some_and(|f| f.original.is_none()) {
            col = col.child(self.render_form(window, cx));
        }
        if self.form.is_none() {
            col = col.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("orch-gh-add")
                            .label("Add a token")
                            .compact()
                            .disabled(!self.loaded)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_form(None, Source::Click, window, cx)
                            })),
                    )
                    .child(
                        Button::new("orch-gh-apply")
                            .label(if self.applying { "Applying…" } else { "Apply to the orchestrator" })
                            .primary()
                            .compact()
                            .disabled(self.applying || !self.loaded)
                            .on_click(cx.listener(|this, _, _, cx| this.apply(Source::Click, cx))),
                    ),
            );
            if let Some(err) = &self.error {
                col = col.child(div().text_xs().text_color(cx.theme().danger).child(selectable_text(
                    "orch-gh-error",
                    err.clone(),
                    window,
                    cx,
                )));
            }
        }
        if let Some(status) = self.apply_status.clone() {
            let (color, text) = match status {
                Ok(t) => (muted, t),
                Err(t) => (cx.theme().danger, t),
            };
            col = col.child(div().text_xs().text_color(color).child(selectable_text(
                "orch-gh-status",
                text,
                window,
                cx,
            )));
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config { tokens: vec![Entry { id: "acme".into(), label: "Acme".into(), repos: vec!["acme/*".into()] }] }
    }

    #[test]
    fn repos_split_on_lines_commas_and_spaces() {
        assert_eq!(
            parse_repos("acme/api, acme/web\n\n  other/*  ,*, acme/api").unwrap(),
            vec!["acme/api", "acme/web", "other/*", "*"]
        );
    }

    #[test]
    fn repos_reject_a_bad_pattern_and_an_empty_list() {
        assert!(parse_repos("acme").unwrap_err().contains("acme"));
        assert!(parse_repos(" \n, ").is_err());
    }

    #[test]
    fn a_new_entry_needs_its_token() {
        assert!(check_form(&Config::default(), None, "Acme", "acme/*", false, false).is_err());
        let ok = check_form(&Config::default(), None, "Acme", "acme/*", true, false).unwrap();
        assert_eq!(ok.id, "acme");
        assert_eq!(ok.config.tokens.len(), 1);
    }

    #[test]
    fn a_new_entry_gets_an_unused_id() {
        let ok = check_form(&cfg(), None, "Acme", "x/*", true, false).unwrap();
        assert_eq!(ok.id, "acme-2");
        assert_eq!(ok.config.tokens.len(), 2);
    }

    #[test]
    fn editing_keeps_the_stored_token_and_replaces_in_place() {
        let ok = check_form(&cfg(), Some("acme"), "Acme Inc", "acme/api\nacme/web", false, true).unwrap();
        assert_eq!(ok.config.tokens.len(), 1);
        assert_eq!(ok.config.tokens[0].label, "Acme Inc");
        assert_eq!(ok.config.tokens[0].repos, vec!["acme/api", "acme/web"]);
        // Editing an entry whose token is gone needs one pasted.
        assert!(check_form(&cfg(), Some("acme"), "Acme", "acme/*", false, false).is_err());
    }

    #[test]
    fn a_blank_label_is_refused() {
        assert!(check_form(&cfg(), None, "  ", "a/b", true, false).is_err());
    }

    #[test]
    fn summary_has_plurals() {
        assert_eq!(applied_summary(1, 1), "1 token, 1 route");
        assert_eq!(applied_summary(2, 3), "2 tokens, 3 routes");
    }
}
