//! Settings → "Databases": the databases a node running in the cloud may log
//! in to through this machine (`[[database]]` in `sandboxes.toml`,
//! `doc/cloud-sandboxes/database-access.md`).
//!
//! One card per database: what the agent asks for, where it is, who to log in
//! as, and which AWS profile signs the login (RDS IAM; the profile never
//! leaves this machine). **Test** signs a login and connects to the host from
//! here, and sends nothing to it. Nothing reaches a node until Save, and a
//! node is told the names the next time it wakes.
//!
//! The inputs are ordinary form inputs, as in
//! `crate::ui::editable_secret_list`: always editable, Tab moves between
//! them, Escape hands focus back to Settings. Everything that reads or writes
//! `sandboxes.toml`, runs the AWS CLI, or opens a connection runs off the UI
//! thread.

use crate::ui::journey::{self, Source};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, ParentElement, Render, Styled, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable, h_flex, v_flex};
use std::path::PathBuf;
use tod_sandbox::database::{DEFAULT_SANDBOX_PORT, Database};

const CONTEXT: &str = "DatabasesEditor";

actions!(databases_editor, [EditorEnter, EditorEscape]);

/// Asks Settings to take keyboard focus back (Escape in a row).
pub struct Leave;

/// Enter and Escape inside a row's inputs: registered a context deeper than a
/// surface's own, after them.
pub fn register_keyboard_bindings(cx: &mut App) {
    let in_input: &'static str = Box::leak(format!("{CONTEXT} > Input").into_boxed_str());
    cx.bind_keys([
        KeyBinding::new("enter", EditorEnter, Some(in_input)),
        KeyBinding::new("escape", EditorEscape, Some(in_input)),
    ]);
}

/// What the user typed for one database, before it is checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowValues {
    pub name: String,
    pub host: String,
    pub user: String,
    pub dbname: String,
    pub region: String,
    pub profile: String,
    pub port: String,
    pub sandbox_port: String,
}

impl RowValues {
    pub fn from_database(d: &Database) -> Self {
        Self {
            name: d.name.clone(),
            host: d.host.clone(),
            user: d.user.clone(),
            dbname: d.dbname.clone(),
            region: d.region.clone(),
            profile: d.profile.clone().unwrap_or_default(),
            port: d.port.to_string(),
            sandbox_port: d.sandbox_port.to_string(),
        }
    }

    /// Whether the row was left untouched and empty (it is ignored on Save).
    fn is_blank(&self) -> bool {
        let defaults = Self { port: "5432".into(), sandbox_port: DEFAULT_SANDBOX_PORT.to_string(), ..Self::default() };
        let mut me = self.clone();
        me.port = me.port.trim().to_string();
        me.sandbox_port = me.sandbox_port.trim().to_string();
        me == defaults || *self == Self::default()
    }

    /// The database this row stands for, or what is wrong with it.
    pub fn to_database(&self) -> Result<Database, String> {
        let port = |what: &str, text: &str, default: u16| -> Result<u16, String> {
            let text = text.trim();
            if text.is_empty() { Ok(default) } else { text.parse().map_err(|_| format!("{what} {text:?} is not a port number")) }
        };
        let required = |what: &str, text: &str| -> Result<String, String> {
            let text = text.trim();
            if text.is_empty() { Err(format!("{what} is empty")) } else { Ok(text.to_string()) }
        };
        let database = Database {
            name: required("the name", &self.name)?,
            host: required("the host", &self.host)?,
            port: port("the port", &self.port, 5432)?,
            user: required("the user", &self.user)?,
            dbname: required("the database", &self.dbname)?,
            region: required("the region", &self.region)?,
            profile: Some(self.profile.trim().to_string()).filter(|p| !p.is_empty()),
            sandbox_port: port("the sandbox port", &self.sandbox_port, DEFAULT_SANDBOX_PORT)?,
        };
        database.validate().map_err(|e| format!("{e:#}"))?;
        Ok(database)
    }
}

/// The databases to save for the rows as the user left them, or an error
/// naming the row. A row never touched is ignored.
pub fn plan_save(rows: &[RowValues]) -> Result<Vec<Database>, String> {
    let mut out: Vec<Database> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if row.is_blank() {
            continue;
        }
        let n = i + 1;
        let database = row.to_database().map_err(|e| format!("Row {n}: {e}."))?;
        if let Some(first) = out.iter().find(|d| d.name == database.name) {
            return Err(format!("Row {n}: {} is already used by another row.", first.name));
        }
        if let Some(other) = out.iter().find(|d| d.sandbox_port == database.sandbox_port) {
            return Err(format!(
                "Row {n}: sandbox port {} is already used by {}; give one of them another.",
                database.sandbox_port, other.name
            ));
        }
        out.push(database);
    }
    Ok(out)
}

struct Row {
    uid: u64,
    name: Entity<InputState>,
    host: Entity<InputState>,
    user: Entity<InputState>,
    dbname: Entity<InputState>,
    region: Entity<InputState>,
    profile: Entity<InputState>,
    port: Entity<InputState>,
    sandbox_port: Entity<InputState>,
    testing: bool,
    /// What the last Test found.
    result: Option<Result<String, String>>,
}

impl Row {
    fn values(&self, cx: &App) -> RowValues {
        let text = |e: &Entity<InputState>| e.read(cx).text().to_string();
        RowValues {
            name: text(&self.name),
            host: text(&self.host),
            user: text(&self.user),
            dbname: text(&self.dbname),
            region: text(&self.region),
            profile: text(&self.profile),
            port: text(&self.port),
            sandbox_port: text(&self.sandbox_port),
        }
    }
}

pub struct DatabasesEditor {
    data_root: PathBuf,
    rows: Vec<Row>,
    next_uid: u64,
    focus_handle: FocusHandle,
    loaded: bool,
    saving: bool,
    error: Option<String>,
    status: Option<String>,
}

impl EventEmitter<Leave> for DatabasesEditor {}

impl DatabasesEditor {
    pub fn new(data_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            data_root,
            rows: Vec::new(),
            next_uid: 0,
            focus_handle: cx.focus_handle(),
            loaded: false,
            saving: false,
            error: None,
            status: None,
        };
        this.reload(window, cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.rows.first() {
            Some(row) => row.name.update(cx, |s, cx| s.focus(window, cx)),
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// Re-read `sandboxes.toml`, off the UI thread.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.data_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    tod_store::fleet::sandbox::Sandboxes::load(&root)
                        .and_then(|s| s.databases())
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loaded = true;
                match loaded {
                    Ok(databases) => {
                        this.rows.clear();
                        for d in &databases {
                            this.push_row(RowValues::from_database(d), window, cx);
                        }
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn push_row(&mut self, values: RowValues, window: &mut Window, cx: &mut Context<Self>) {
        let input = |placeholder: &'static str, value: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        self.next_uid += 1;
        self.rows.push(Row {
            uid: self.next_uid,
            name: input("prod", values.name, window, cx),
            host: input("the server's own name, e.g. the RDS instance endpoint", values.host, window, cx),
            user: input("database user", values.user, window, cx),
            dbname: input("database", values.dbname, window, cx),
            region: input("us-gov-east-1", values.region, window, cx),
            profile: input("AWS profile (aws sso login --profile …)", values.profile, window, cx),
            port: input("5432", values.port, window, cx),
            sandbox_port: input("15432", values.sandbox_port, window, cx),
            testing: false,
            result: None,
        });
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let blank = RowValues { port: "5432".into(), sandbox_port: self.free_sandbox_port(cx).to_string(), ..Default::default() };
        // At the top, where it is on screen and takes focus: the panel does
        // not scroll to a row added below the fold.
        self.push_row(blank, window, cx);
        if let Some(row) = self.rows.pop() {
            row.name.update(cx, |s, cx| s.focus(window, cx));
            self.rows.insert(0, row);
        }
        self.record(cx, "databases_add_row", Source::Click);
        cx.notify();
    }

    /// The first sandbox port from the default up that no row uses.
    fn free_sandbox_port(&self, cx: &App) -> u16 {
        let used: Vec<String> = self.rows.iter().map(|r| r.sandbox_port.read(cx).text().to_string().trim().to_string()).collect();
        (DEFAULT_SANDBOX_PORT..DEFAULT_SANDBOX_PORT + 100)
            .find(|p| !used.contains(&p.to_string()))
            .unwrap_or(DEFAULT_SANDBOX_PORT)
    }

    fn remove(&mut self, uid: u64, cx: &mut Context<Self>) {
        if let Some(i) = self.rows.iter().position(|r| r.uid == uid) {
            self.rows.remove(i);
            self.record(cx, "databases_remove_row", Source::Click);
            cx.notify();
        }
    }

    /// A `UserAction` with what was on screen: Add, Save (primary), Test.
    fn record(&self, cx: &mut App, action: &str, source: Source) {
        let button = |id: &str, label: &str, primary: bool, disabled: bool| tod_journey::PresentedAction {
            id: id.to_string(),
            label: label.to_string(),
            primary,
            disabled,
        };
        let presented = tod_journey::Presented {
            actions: vec![
                button("databases_add_row", "Add", false, false),
                button("databases_save", "Save", true, self.saving || !self.loaded),
                button("databases_test", "Test", false, !self.loaded),
            ],
            focused: Some(action.to_string()),
            notices: Vec::new(),
        };
        journey::record_action(cx, tod_store::conversation::Focus::Project, action, source, "settings_databases", presented);
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.loaded {
            return;
        }
        let rows: Vec<RowValues> = self.rows.iter().map(|r| r.values(cx)).collect();
        let databases = match plan_save(&rows) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.record(cx, "databases_save", Source::Click);
        self.saving = true;
        self.error = None;
        self.status = None;
        cx.notify();
        let root = self.data_root.clone();
        let count = databases.len();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(&root)?;
                    sandboxes.replace_databases(databases)
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.status = Some(match count {
                            0 => "Saved. No database is set up: nodes in the cloud cannot log in to any.".to_string(),
                            n => format!(
                                "Saved {n}. A node running in the cloud learns of them the next time it wakes; \
                                 your machine must be on, with tod running, to answer its logins."
                            ),
                        });
                    }
                    Err(e) => this.error = Some(format!("Saving failed: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Signs a login with the row's profile and connects to its host from
    /// here; sends nothing to the database.
    fn test(&mut self, uid: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter_mut().find(|r| r.uid == uid) else { return };
        let values = row.values(cx);
        let database = match values.to_database() {
            Ok(d) => d,
            Err(e) => {
                row.result = Some(Err(format!("{e}.")));
                cx.notify();
                return;
            }
        };
        row.testing = true;
        row.result = None;
        self.record(cx, "databases_test", Source::Click);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { database.check() }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if let Some(row) = this.rows.iter_mut().find(|r| r.uid == uid) {
                    row.testing = false;
                    row.result = Some(result.map_err(|e| format!("{e:#}")));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn field(label: &'static str, input: &Entity<InputState>) -> impl IntoElement {
    v_flex()
        .gap_0p5()
        .flex_1()
        .min_w_0()
        .child(style::text_dense_muted(div()).child(label))
        .child(Input::new(input))
}

impl Render for DatabasesEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = v_flex()
            .id("databases-editor")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .gap_3()
            .w_full()
            .max_w(px(760.))
            .on_action(cx.listener(|_, _: &EditorEnter, _, _| {}))
            .on_action(cx.listener(|_, _: &EditorEscape, _, cx| cx.emit(Leave)))
            // A surface that focuses itself on every click must not take
            // focus from the input just clicked.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(style::text_muted(div()).child(
                "Databases a node running in the cloud may log in to through this machine, for databases that only \
                 your machine can reach (an IP allowlist, a VPN). The agent asks by name (`tod-cli db`); this machine \
                 signs a 15-minute login with the AWS profile and carries the connection. No AWS credential is ever \
                 stored in a sandbox. It works while this machine is on and tod is running, and you are signed in \
                 with `aws sso login`. The login has the database user's privileges: for an agent, a read-only user \
                 is the safe choice.",
            ));
        if !self.loaded {
            col = col.child(style::text_dense_muted(div()).child("Loading…"));
        }
        // Above the cards, so Add and Save are never below the fold: the panel
        // does not scroll to a focused control.
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("databases-add")
                        .icon(gpui_component::IconName::Plus)
                        .label("Add a database")
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                )
                .child(
                    Button::new("databases-save")
                        .label(if self.saving { "Saving…" } else { "Save" })
                        .primary()
                        .compact()
                        .disabled(self.saving || !self.loaded)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                ),
        );
        if let Some(err) = self.error.clone() {
            col = col.child(style::text_error(div()).child(selectable_text("databases-error", err, window, cx)));
        }
        if let Some(status) = self.status.clone() {
            col = col.child(style::text_dense_muted(div()).child(selectable_text("databases-status", status, window, cx)));
        }
        for row in &self.rows {
            let uid = row.uid;
            let result = row.result.clone();
            let mut card = v_flex()
                .gap_2()
                .p_3()
                .border_1()
                .rounded_md()
                .child(
                    h_flex()
                        .gap_2()
                        .child(field("Name (what the agent asks for)", &row.name))
                        .child(field("Database user", &row.user))
                        .child(field("Database", &row.dbname)),
                )
                .child(h_flex().gap_2().child(field("Host (the server's own name)", &row.host)))
                .child(
                    h_flex()
                        .gap_2()
                        .child(field("AWS region", &row.region))
                        .child(field("AWS profile", &row.profile))
                        .child(field("Port", &row.port))
                        .child(field("Port in the sandbox", &row.sandbox_port)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new(("databases-test", uid))
                                .label(if row.testing { "Testing…" } else { "Test" })
                                .compact()
                                .disabled(row.testing)
                                .tooltip("Signs a login with the AWS profile and connects to the host from this machine. Sends nothing to the database.")
                                .on_click(cx.listener(move |this, _, window, cx| this.test(uid, window, cx))),
                        )
                        .child(
                            Button::new(("databases-remove", uid))
                                .icon(gpui_kit_assets::IconName::Trash)
                                .ghost()
                                .compact()
                                .tooltip("Remove this database (it takes effect on Save)")
                                .on_click(cx.listener(move |this, _, _, cx| this.remove(uid, cx))),
                        ),
                );
            if let Some(result) = result {
                card = card.child(match result {
                    Ok(t) => style::text_dense_muted(div()).child(selectable_text(("databases-result", uid), t, window, cx)),
                    Err(t) => style::text_error(div()).child(selectable_text(("databases-result", uid), t, window, cx)),
                });
            }
            col = col.child(card);
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, port: &str) -> RowValues {
        RowValues {
            name: name.into(),
            host: "x.rds.example".into(),
            user: "u".into(),
            dbname: "d".into(),
            region: "us-east-1".into(),
            profile: "p".into(),
            port: "5432".into(),
            sandbox_port: port.into(),
        }
    }

    #[test]
    fn rows_become_databases_and_back() {
        let dbs = plan_save(&[row("prod", "15432")]).unwrap();
        assert_eq!(dbs[0].profile.as_deref(), Some("p"));
        assert_eq!(RowValues::from_database(&dbs[0]), row("prod", "15432"));
    }

    #[test]
    fn blank_ports_take_their_defaults_and_a_blank_row_is_ignored() {
        let mut r = row("prod", "");
        r.port = String::new();
        let dbs = plan_save(&[RowValues::default(), r, RowValues { port: "5432".into(), sandbox_port: "15432".into(), ..Default::default() }]).unwrap();
        assert_eq!(dbs.len(), 1);
        assert_eq!((dbs[0].port, dbs[0].sandbox_port), (5432, DEFAULT_SANDBOX_PORT));
    }

    #[test]
    fn mistakes_name_the_row() {
        let err = plan_save(&[row("a", "15432"), RowValues { host: String::new(), ..row("b", "15433") }]).unwrap_err();
        assert_eq!(err, "Row 2: the host is empty.");
        let err = plan_save(&[row("a", "15432"), row("a", "15433")]).unwrap_err();
        assert_eq!(err, "Row 2: a is already used by another row.");
        let err = plan_save(&[row("a", "15432"), row("b", "15432")]).unwrap_err();
        assert!(err.starts_with("Row 2: sandbox port 15432 is already used by a"), "{err}");
        let err = plan_save(&[row("a", "http")]).unwrap_err();
        assert!(err.contains("is not a port number"), "{err}");
        let err = plan_save(&[RowValues { host: "a b".into(), ..row("a", "15432") }]).unwrap_err();
        assert!(err.starts_with("Row 1:") && err.contains("plain host name"), "{err}");
    }
}
