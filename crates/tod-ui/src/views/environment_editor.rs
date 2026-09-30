//! The Environment capability's editor (a section of the task editor) and the
//! presets file editor (a Settings section).
//!
//! Secret values are write-only here: typed into a masked input, stored in the
//! credential store, never read back for display. Everything that touches the
//! credential store, the network, or the disk runs off the UI thread.
//!
//! Focus: the list is navigated with the mouse; an entry's form is the edit
//! mode (its inputs exist only while it is open), Escape closes it, and Save is
//! explicit.

use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tod_store::environment::{self, Auth, Entry, EntryKind, Resolved};
use tod_store::environment_presets::{self, Preset};
use tod_store::fleet::FleetStore;
use tod_store::outline::OutlineMutation;
use tod_store::CredentialStore;
use uuid::Uuid;

const AUTH_STYLES: [&str; 4] = ["Bearer token", "API key in a header", "Basic auth", "Custom"];
const NO_HOST_INVALID: &str = "Invalid: a credential needs the host it is used with (edit it and add one).";

pub struct EnvironmentEditor {
    fleet: Arc<FleetStore>,
    data_root: PathBuf,
    node: Uuid,
    enabled: bool,
    resolved: Vec<Resolved>,
    titles: HashMap<Uuid, String>,
    /// Whether each secret (by account) has a value stored.
    is_set: HashMap<String, bool>,
    presets: Vec<Preset>,
    form: Option<Form>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

struct Form {
    /// The name of the entry being edited; `None` for a new one.
    original: Option<String>,
    preset: Option<String>,
    secret: bool,
    auth_style: usize,
    name: Entity<InputState>,
    value: Entity<InputState>,
    secret_value: Entity<InputState>,
    host: Entity<InputState>,
    env_var: Entity<InputState>,
    description: Entity<InputState>,
    test_url: Entity<InputState>,
    /// Header name, Basic username, or a custom header name.
    auth_extra: Entity<InputState>,
    auth_template: Entity<InputState>,
    preset_select: Entity<SelectState<Vec<String>>>,
    auth_select: Entity<SelectState<Vec<String>>>,
    testing: bool,
    saving: bool,
    test_result: Option<(bool, String)>,
    _subs: Vec<Subscription>,
}

fn text(input: &Entity<InputState>, cx: &gpui::App) -> String {
    input.read(cx).text().to_string()
}

impl EnvironmentEditor {
    pub fn new(fleet: Arc<FleetStore>, data_root: PathBuf, node: Uuid, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            fleet,
            data_root,
            node,
            enabled: false,
            resolved: Vec::new(),
            titles: HashMap::new(),
            is_set: HashMap::new(),
            presets: Vec::new(),
            form: None,
            error: None,
            _subscriptions: Vec::new(),
        };
        this.reload(cx);
        this
    }

    /// Point at `node` and say whether it has the capability itself.
    pub fn sync(&mut self, node: Uuid, enabled: bool, cx: &mut Context<Self>) {
        if self.node != node {
            self.node = node;
            self.form = None;
            self.error = None;
            self.enabled = enabled;
            self.reload(cx);
        } else if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled {
                self.form = None;
            }
            cx.notify();
        }
    }

    /// Re-read the node's environment (and which secrets are set) off the UI thread.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let fleet = self.fleet.clone();
        let node = self.node;
        let root = self.data_root.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let resolved = fleet.read(|conn| environment::resolve(conn, node)).unwrap_or_default();
                    let store = CredentialStore::from_data_root(&root);
                    let mut is_set = HashMap::new();
                    let mut titles = HashMap::new();
                    for r in &resolved {
                        if r.entry.kind == EntryKind::Secret {
                            is_set.insert(r.account(), r.is_set(&store));
                        }
                        if r.inherited && !titles.contains_key(&r.source_node) {
                            let title = fleet
                                .read(|conn| {
                                    Ok(tod_store::outline::repos::NodeRepo::new(conn)
                                        .get(r.source_node)?
                                        .map(|n| n.title))
                                })
                                .ok()
                                .flatten();
                            titles.insert(r.source_node, title.unwrap_or_else(|| "an ancestor".into()));
                        }
                    }
                    let presets = environment_presets::load(&root).unwrap_or_else(|_| environment_presets::bundled());
                    (resolved, is_set, titles, presets)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                (this.resolved, this.is_set, this.titles, this.presets) = loaded;
                cx.notify();
            });
        })
        .detach();
    }

    fn own_entries(&self) -> Vec<Entry> {
        self.resolved.iter().filter(|r| !r.inherited).map(|r| r.entry.clone()).collect()
    }

    fn open_form(&mut self, original: Option<Entry>, window: &mut Window, cx: &mut Context<Self>) {
        let mk = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let name = mk("name, e.g. growthbook", window, cx);
        let value = mk("value", window, cx);
        let secret_value = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("secret value (write-only)")
        });
        let host = mk("api.example.com (comma-separated)", window, cx);
        let env_var = mk("ENV_VAR (default: upper-cased name)", window, cx);
        let description = mk("what it is for (optional)", window, cx);
        let test_url = mk("https://{host}/v1/me", window, cx);
        let auth_extra = mk("header name", window, cx);
        let auth_template = mk("template, e.g. Token {value}", window, cx);
        let labels: Vec<String> = self.presets.iter().map(|p| p.label.clone()).collect();
        let preset_select = cx.new(|cx| SelectState::new(labels, None, window, cx));
        let auth_select = cx.new(|cx| {
            SelectState::new(AUTH_STYLES.iter().map(|s| s.to_string()).collect::<Vec<_>>(), None, window, cx)
        });
        auth_select.update(cx, |s, cx| s.set_selected_value(&AUTH_STYLES[0].to_string(), window, cx));
        let subs = vec![
            cx.subscribe_in(&preset_select, window, |this, _, ev: &SelectEvent<Vec<String>>, window, cx| {
                if let SelectEvent::Confirm(Some(label)) = ev {
                    if let Some(p) = this.presets.iter().find(|p| &p.label == label).cloned() {
                        this.apply_preset(&p, window, cx);
                    }
                }
            }),
            cx.subscribe_in(&auth_select, window, |this, _, ev: &SelectEvent<Vec<String>>, _, cx| {
                if let SelectEvent::Confirm(Some(label)) = ev {
                    if let (Some(form), Some(i)) = (this.form.as_mut(), AUTH_STYLES.iter().position(|s| s == label)) {
                        form.auth_style = i;
                        cx.notify();
                    }
                }
            }),
        ];
        let mut form = Form {
            original: original.as_ref().map(|e| e.name.clone()),
            preset: None,
            secret: true,
            auth_style: 0,
            name,
            value,
            secret_value,
            host,
            env_var,
            description,
            test_url,
            auth_extra,
            auth_template,
            preset_select,
            auth_select,
            testing: false,
            saving: false,
            test_result: None,
            _subs: subs,
        };
        if let Some(entry) = &original {
            Self::fill(&mut form, entry, window, cx);
        }
        self.form = Some(form);
        self.error = None;
        cx.notify();
    }

    fn fill(form: &mut Form, entry: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        let set = |input: &Entity<InputState>, v: &str, window: &mut Window, cx: &mut Context<Self>| {
            input.update(cx, |s, cx| s.set_value(v.to_string(), window, cx));
        };
        form.preset = entry.preset.clone();
        form.secret = entry.kind == EntryKind::Secret;
        set(&form.name, &entry.name, window, cx);
        set(&form.value, entry.value.as_deref().unwrap_or(""), window, cx);
        set(&form.host, &entry.hosts.join(", "), window, cx);
        set(&form.env_var, &entry.env_var, window, cx);
        set(&form.description, entry.description.as_deref().unwrap_or(""), window, cx);
        set(&form.test_url, entry.test_url.as_deref().unwrap_or(""), window, cx);
        let (style, extra, template) = match &entry.auth {
            Auth::Bearer => (0, String::new(), String::new()),
            Auth::Header { header } => (1, header.clone(), String::new()),
            Auth::Basic { username } => (2, username.clone(), String::new()),
            Auth::Custom { header, template } => (3, header.clone(), template.clone()),
        };
        form.auth_style = style;
        set(&form.auth_extra, &extra, window, cx);
        set(&form.auth_template, &template, window, cx);
        form.auth_select
            .update(cx, |s, cx| s.set_selected_value(&AUTH_STYLES[style].to_string(), window, cx));
    }

    fn apply_preset(&mut self, preset: &Preset, window: &mut Window, cx: &mut Context<Self>) {
        let entry = preset.to_entry();
        if let Some(form) = self.form.as_mut() {
            let keep_name = text(&form.name, cx);
            Self::fill(form, &entry, window, cx);
            if !keep_name.trim().is_empty() && form.original.is_some() {
                form.name.update(cx, |s, cx| s.set_value(keep_name, window, cx));
            }
            form.test_result = None;
        }
        cx.notify();
    }

    /// The entry the form describes, or why it is not valid yet.
    fn form_entry(&self, cx: &gpui::App) -> Result<Entry, String> {
        let form = self.form.as_ref().ok_or("no form")?;
        let name = text(&form.name, cx).trim().to_string();
        let mut entry = if form.secret { Entry::secret(&name) } else { Entry::variable(&name, &text(&form.value, cx)) };
        entry.env_var = text(&form.env_var, cx).trim().to_string();
        entry.description = Some(text(&form.description, cx).trim().to_string()).filter(|d| !d.is_empty());
        entry.preset = form.preset.clone();
        if form.secret {
            entry.hosts = text(&form.host, cx)
                .split([',', ' ', '\n'])
                .filter_map(environment::host_of)
                .collect();
            entry.test_url = Some(text(&form.test_url, cx).trim().to_string()).filter(|u| !u.is_empty());
            let extra = text(&form.auth_extra, cx).trim().to_string();
            entry.auth = match form.auth_style {
                1 => Auth::Header { header: extra },
                2 => Auth::Basic { username: extra },
                3 => Auth::Custom { header: extra, template: text(&form.auth_template, cx) },
                _ => Auth::Bearer,
            };
        }
        entry.validate().map_err(|e| e.to_string())?;
        Ok(entry)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let entry = match self.form_entry(cx) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let Some(form) = self.form.as_mut() else { return };
        let original = form.original.clone();
        let typed = (entry.kind == EntryKind::Secret).then(|| text(&form.secret_value, cx)).filter(|v| !v.is_empty());
        form.saving = true;
        let mut list = self.own_entries();
        list.retain(|e| Some(&e.name) != original.as_ref());
        if list.iter().any(|e| e.name.eq_ignore_ascii_case(&entry.name)) {
            self.error = Some(format!("{} is already defined on this node", entry.name));
            if let Some(f) = self.form.as_mut() {
                f.saving = false;
            }
            cx.notify();
            return;
        }
        list.push(entry.clone());
        self.error = None;
        let (fleet, root, node) = (self.fleet.clone(), self.data_root.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let store = CredentialStore::from_data_root(&root);
                    let account = environment::secret_account(node, &entry.name);
                    if let Some(value) = typed {
                        store.set_named(&account, &value).map_err(|e| e.to_string())?;
                    }
                    if let Some(old) = original.filter(|o| o != &entry.name) {
                        // Renamed: the value follows it.
                        if entry.kind == EntryKind::Secret
                            && let Some(v) = store.get_named(&environment::secret_account(node, &old))
                            && !store.has_named(&account)
                        {
                            store.set_named(&account, &v).map_err(|e| e.to_string())?;
                        }
                        environment::forget_secret(&store, node, &old);
                    }
                    fleet
                        .enqueue_outline(OutlineMutation::SetNodeEnvironment { node_id: node, entries: list })
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.form = None,
                    Err(e) => {
                        this.error = Some(e);
                        if let Some(f) = this.form.as_mut() {
                            f.saving = false;
                        }
                    }
                }
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove(&mut self, name: String, cx: &mut Context<Self>) {
        let mut list = self.own_entries();
        list.retain(|e| e.name != name);
        let (fleet, root, node) = (self.fleet.clone(), self.data_root.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    environment::forget_secret(&CredentialStore::from_data_root(&root), node, &name);
                    fleet
                        .enqueue_outline(OutlineMutation::SetNodeEnvironment { node_id: node, entries: list })
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err();
                this.reload(cx);
            });
        })
        .detach();
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        let entry = match self.form_entry(cx) {
            Ok(e) => e,
            Err(e) => {
                if let Some(f) = self.form.as_mut() {
                    f.test_result = Some((false, e));
                }
                cx.notify();
                return;
            }
        };
        let Some(form) = self.form.as_mut() else { return };
        let typed = text(&form.secret_value, cx);
        form.testing = true;
        form.test_result = None;
        let (root, node) = (self.data_root.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let value = if typed.is_empty() {
                        CredentialStore::from_data_root(&root)
                            .get_named(&environment::secret_account(node, &entry.name))
                    } else {
                        Some(typed)
                    };
                    match value {
                        Some(v) => environment_presets::test_call(&entry, &v),
                        None => environment_presets::TestOutcome {
                            ok: false,
                            message: "enter the secret value to test it".into(),
                        },
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(f) = this.form.as_mut() {
                    f.testing = false;
                    f.test_result = Some((outcome.ok, outcome.message));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn field(label: &'static str, input: impl IntoElement) -> impl IntoElement {
        v_flex()
            .gap_0p5()
            .w_full()
            .child(div().text_xs().font_semibold().child(label))
            .child(input)
    }

    fn render_row(&self, index: usize, r: &Resolved, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let warn = cx.theme().warning;
        let e = &r.entry;
        let secret = e.kind == EntryKind::Secret;
        let state = if !secret {
            "variable".to_string()
        } else if self.is_set.get(&r.account()).copied().unwrap_or(false) {
            "secret · set".to_string()
        } else {
            "secret · not set".to_string()
        };
        let mut detail = format!("${} · {state}", e.env_name());
        if secret && !e.hosts.is_empty() {
            detail.push_str(&format!(" · {} ({})", e.hosts.join(", "), e.auth.label()));
        }
        if !secret {
            detail.push_str(&format!(" · {}", e.value.as_deref().unwrap_or("")));
        }
        if r.inherited {
            detail.push_str(&format!(
                " · from {}",
                self.titles.get(&r.source_node).map_or("an ancestor", String::as_str)
            ));
        }
        let name = e.name.clone();
        let edit_entry = e.clone();
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
                        format!("env-name-{index}"),
                        e.name.clone(),
                        window,
                        cx,
                    )))
                    .child(div().flex_1())
                    .when(!r.inherited && self.enabled, |el| {
                        el.child(
                            Button::new(("env-edit", index))
                                .label("Edit")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_form(Some(edit_entry.clone()), window, cx)
                                })),
                        )
                        .child(
                            Button::new(("env-remove", index))
                                .label("Remove")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(move |this, _, _, cx| this.remove(name.clone(), cx))),
                        )
                    }),
            )
            .child(div().text_xs().text_color(muted).child(selectable_text(
                format!("env-detail-{index}"),
                detail,
                window,
                cx,
            )))
            .when_some(e.description(), |el, d| {
                el.child(div().text_xs().text_color(muted).child(selectable_text(
                    format!("env-desc-{index}"),
                    d.to_string(),
                    window,
                    cx,
                )))
            })
            .when(secret && e.hosts.is_empty(), |el| {
                el.child(div().text_xs().text_color(warn).child(NO_HOST_INVALID))
            })
            .into_any_element()
    }

    fn render_form(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(form) = &self.form else { return div().into_any_element() };
        let muted = cx.theme().muted_foreground;
        let warn = cx.theme().warning;
        let danger = cx.theme().danger;
        let hosts_empty = text(&form.host, cx).trim().is_empty();
        let kind_row = h_flex()
            .gap_1()
            .child(
                Button::new("env-kind-var")
                    .label("Variable")
                    .compact()
                    .selected(!form.secret)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(f) = this.form.as_mut() {
                            f.secret = false;
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("env-kind-secret")
                    .label("Secret")
                    .compact()
                    .selected(form.secret)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(f) = this.form.as_mut() {
                            f.secret = true;
                        }
                        cx.notify();
                    })),
            );
        let mut col = v_flex()
            .gap_2()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().list_active_border)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.form = None;
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(Self::field("Preset", Select::new(&form.preset_select).placeholder("Start from a service preset").small()))
            .child(Self::field("Name", Input::new(&form.name)))
            .child(Self::field("Kind", kind_row));
        if form.secret {
            col = col
                .child(Self::field(
                    if form.original.is_some() { "Value (leave empty to keep the stored one)" } else { "Value" },
                    Input::new(&form.secret_value),
                ))
                .child(Self::field("Host or URL (required)", Input::new(&form.host)))
                .when(hosts_empty, |el| el.child(div().text_xs().text_color(warn).child("A credential needs the host it is used with, e.g. api.example.com")))
                .child(Self::field(
                    "Sent as",
                    Select::new(&form.auth_select).small(),
                ))
                .when(form.auth_style > 0, |el| {
                    el.child(Self::field(
                        match form.auth_style {
                            2 => "Username",
                            _ => "Header name",
                        },
                        Input::new(&form.auth_extra),
                    ))
                })
                .when(form.auth_style == 3, |el| {
                    el.child(Self::field("Template ({value} is the secret)", Input::new(&form.auth_template)))
                })
                .child(Self::field("Environment variable", Input::new(&form.env_var)))
                .child(Self::field("Description", Input::new(&form.description)))
                .child(Self::field("Test URL", Input::new(&form.test_url)));
        } else {
            col = col
                .child(Self::field("Value", Input::new(&form.value)))
                .child(Self::field("Environment variable", Input::new(&form.env_var)))
                .child(Self::field("Description", Input::new(&form.description)));
        }
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("env-save")
                        .label("Save")
                        .primary()
                        .compact()
                        .disabled(form.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                )
                .child(
                    Button::new("env-cancel")
                        .label("Cancel")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.form = None;
                            cx.notify();
                        })),
                )
                .when(form.secret, |el| {
                    el.child(
                        Button::new("env-test")
                            .label(if form.testing { "Testing…" } else { "Test" })
                            .compact()
                            .disabled(form.testing)
                            .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                    )
                }),
        );
        if let Some((ok, message)) = &form.test_result {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(if *ok { cx.theme().success } else { danger })
                    .child(selectable_text("env-test-result", message.clone(), window, cx)),
            );
        }
        if let Some(err) = &self.error {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(selectable_text("env-form-error", err.clone(), window, cx)),
            );
        }
        let _ = muted;
        col.into_any_element()
    }
}

impl Render for EnvironmentEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let visible: Vec<Resolved> =
            self.resolved.iter().filter(|r| self.enabled || r.inherited).cloned().collect();
        let mut col = v_flex().gap_2().px_3().pb_3().w_full();
        if visible.is_empty() {
            col = col.child(div().text_xs().text_color(muted).child(if self.enabled {
                "No variables or secrets yet."
            } else {
                "Nothing inherited."
            }));
        }
        for (i, r) in visible.iter().enumerate() {
            col = col.child(self.render_row(i, r, window, cx));
        }
        if self.enabled && self.form.is_none() {
            col = col.child(
                h_flex().child(
                    Button::new("env-add")
                        .label("Add variable or secret")
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| this.open_form(None, window, cx))),
                ),
            );
        }
        if self.form.is_some() {
            col = col.child(self.render_form(window, cx));
        } else if let Some(err) = &self.error {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(selectable_text("env-error", err.clone(), window, cx)),
            );
        }
        col
    }
}

impl EventEmitter<()> for EnvironmentEditor {}

/// Settings: the user's presets file, edited as text with an explicit Save.
pub struct PresetsEditor {
    data_root: PathBuf,
    editor: Entity<TextareaState>,
    status: Option<(bool, String)>,
}

impl PresetsEditor {
    pub fn new(data_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = environment_presets::editable_text(&data_root);
        let editor = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).rows(16);
            s.set_value(initial, window, cx);
            s
        });
        Self { data_root, editor, status: None }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |s, cx| s.focus(window, cx));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let body = self.editor.read(cx).text().to_string();
        let root = self.data_root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { environment_presets::save_user_file(&root, &body).map_err(|e| format!("{e:#}")) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.status = Some(match result {
                    Ok(()) => (true, "Saved.".to_string()),
                    Err(e) => (false, e),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PresetsEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .w_full()
            .max_w(px(720.))
            .child(div().text_sm().text_color(muted).child(
                "Service presets offered in a node's Environment section. A preset with the same id as a bundled one replaces it.",
            ))
            .child(div().text_xs().text_color(muted).child(selectable_text(
                "presets-path",
                environment_presets::user_file(&self.data_root).display().to_string(),
                window,
                cx,
            )))
            .child(Textarea::new(&self.editor).w_full().h(px(320.)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("presets-save")
                            .label("Save")
                            .primary()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .when_some(self.status.clone(), |el, (ok, msg)| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(if ok { cx.theme().success } else { cx.theme().danger })
                                .child(selectable_text("presets-status", msg, window, cx)),
                        )
                    }),
            )
    }
}
