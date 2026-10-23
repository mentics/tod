//! Settings → "Orchestrator GitHub": the GitHub tokens the cloud orchestrator
//! reads with, as an inline list of rows, each ONE repository pattern
//! (`owner/repo`, `owner/*` or `*`) and ONE token
//! (`crate::ui::editable_secret_list`).
//!
//! Each row is one `tod_store::orchestrator_github::Entry` with a single
//! pattern, its label that pattern. An entry saved by an older version with
//! several repositories shows one row per repository, and on Save its stored
//! token follows each row to the row's new id.
//!
//! Tokens are write-only: typed into a masked input, stored in the credential
//! store, never read back for display and never in the JSON file, a label, a
//! log line, an error or a journey record. Save writes the list and the
//! tokens only; nothing reaches the orchestrator until Apply
//! (`tod_core::orchestrator_github::apply`, which blocks on the network and so
//! runs on the background executor). Everything that touches the credential
//! store or the network runs off the UI thread.

use crate::ui::editable_secret_list::{
    EditableSecretList, Labels, ListEvent, RowData, RowValues,
};
use crate::ui::journey::{self, Source};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tod_store::orchestrator_github::{self as store, Config, Entry};

/// Asks Settings to take keyboard focus back (Escape in a row).
pub struct Leave;

/// Enter and Escape inside the rows' inputs.
pub fn register_keyboard_bindings(cx: &mut App) {
    crate::ui::editable_secret_list::register_keyboard_bindings(cx);
}

/// One row per repository of every entry. `stored` says which entry ids have
/// a token stored; a row's tag is the id of the entry it came from.
pub fn rows_from_config(config: &Config, stored: &HashMap<String, bool>) -> Vec<RowData> {
    config
        .tokens
        .iter()
        .flat_map(|entry| {
            let has = stored.get(&entry.id).copied().unwrap_or(false);
            entry.repos.iter().map(move |repo| RowData {
                key: repo.clone(),
                stored: has,
                tag: entry.id.clone(),
            })
        })
        .collect()
}

/// What to do with a token when saving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenOp {
    /// The token stored under `from` is also stored under `to`.
    Copy { from: String, to: String },
    /// The user typed this token for `id`.
    Set { id: String, token: String },
    /// `id` no longer exists: forget its token.
    Delete { id: String },
}

/// A save: the config to write and the token operations, in the order to run
/// them (copies read before anything is written; deletes last).
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub config: Config,
    pub ops: Vec<TokenOp>,
}

/// The config and token operations for the rows as the user left them, or an
/// error naming the row. A row with neither pattern nor token is ignored.
pub fn plan_save(old: &Config, rows: &[RowValues]) -> Result<Plan, String> {
    struct Kept<'a> {
        n: usize,
        pattern: String,
        row: &'a RowValues,
    }
    let mut kept: Vec<Kept> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        if row.key.is_empty() && row.secret.is_empty() && !row.stored {
            continue;
        }
        if row.key.is_empty() {
            return Err(format!("Row {n}: enter a repository (owner/repo, owner/*, or *)."));
        }
        store::check_pattern(&row.key).map_err(|e| format!("Row {n}: {e}"))?;
        if let Some(first) = kept.iter().find(|k| k.pattern.eq_ignore_ascii_case(&row.key)) {
            return Err(format!("Row {n}: {} is already in row {}.", row.key, first.n));
        }
        if row.secret.is_empty() && !(row.stored && !row.tag.is_empty()) {
            return Err(format!("Row {n}: paste a token for {}; none is stored.", row.key));
        }
        kept.push(Kept { n, pattern: row.key.clone(), row });
    }

    // Ids: a row that is exactly its old single-repository entry keeps the
    // entry's id; every other row gets one made from its pattern.
    let mut ids: Vec<Option<String>> = vec![None; kept.len()];
    let mut taken: HashSet<String> = HashSet::new();
    for (k, slot) in kept.iter().zip(ids.iter_mut()) {
        let same = old.tokens.iter().find(|e| {
            e.id == k.row.tag && e.repos.len() == 1 && e.repos[0].eq_ignore_ascii_case(&k.pattern)
        });
        if let Some(entry) = same
            && taken.insert(entry.id.clone())
        {
            *slot = Some(entry.id.clone());
        }
    }
    let mut entries: Vec<Entry> = taken
        .iter()
        .map(|id| Entry { id: id.clone(), label: String::new(), repos: Vec::new() })
        .collect();
    for (k, slot) in kept.iter().zip(ids.iter_mut()) {
        if slot.is_none() {
            let id = store::new_id(&k.pattern, &entries);
            entries.push(Entry { id: id.clone(), label: String::new(), repos: Vec::new() });
            *slot = Some(id);
        }
    }

    let mut config = Config::default();
    let mut copies = Vec::new();
    let mut sets = Vec::new();
    for (k, id) in kept.iter().zip(ids) {
        let id = id.expect("every kept row has an id");
        config.tokens.push(Entry { id: id.clone(), label: k.pattern.clone(), repos: vec![k.pattern.clone()] });
        if !k.row.secret.is_empty() {
            sets.push(TokenOp::Set { id, token: k.row.secret.clone() });
        } else if k.row.tag != id {
            copies.push(TokenOp::Copy { from: k.row.tag.clone(), to: id });
        }
    }
    config.check().map_err(|e| format!("{e:#}"))?;
    let new_ids: HashSet<&str> = config.tokens.iter().map(|e| e.id.as_str()).collect();
    let deletes: Vec<TokenOp> = old
        .tokens
        .iter()
        .filter(|e| !new_ids.contains(e.id.as_str()))
        .map(|e| TokenOp::Delete { id: e.id.clone() })
        .collect();
    let mut ops = copies;
    ops.extend(sets);
    ops.extend(deletes);
    Ok(Plan { config, ops })
}

/// Runs a plan against the credential store and the data root, then returns
/// which ids have a token. Off the UI thread.
fn execute(root: &Path, plan: &Plan) -> Result<HashMap<String, bool>, String> {
    // Read every token to copy before anything is overwritten or deleted.
    let mut copies = Vec::new();
    for op in &plan.ops {
        if let TokenOp::Copy { from, to } = op {
            let token = store::token(root, from)
                .ok_or_else(|| format!("The token stored for {from:?} could not be read: paste it again."))?;
            copies.push((to.clone(), token));
        }
    }
    for (to, token) in copies {
        store::set_token(root, &to, &token).map_err(|e| format!("Storing a token failed: {e}"))?;
    }
    for op in &plan.ops {
        if let TokenOp::Set { id, token } = op {
            store::set_token(root, id, token).map_err(|e| format!("Storing a token failed: {e}"))?;
        }
    }
    plan.config.save(root).map_err(|e| format!("Saving the list failed: {e:#}"))?;
    for op in &plan.ops {
        if let TokenOp::Delete { id } = op {
            store::delete_token(root, id);
        }
    }
    Ok(plan.config.tokens.iter().map(|e| (e.id.clone(), store::has_token(root, &e.id))).collect())
}

/// "N tokens, M routes", with the right plurals.
pub fn applied_summary(tokens: usize, routes: usize) -> String {
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    format!("{}, {}", plural(tokens, "token", "tokens"), plural(routes, "route", "routes"))
}

pub struct OrchestratorGithubEditor {
    data_root: PathBuf,
    list: Entity<EditableSecretList>,
    /// The saved config the rows were made from.
    config: Config,
    loaded: bool,
    saving: bool,
    applying: bool,
    error: Option<String>,
    status: Option<Result<String, String>>,
    _subscription: Subscription,
}

impl EventEmitter<Leave> for OrchestratorGithubEditor {}

impl OrchestratorGithubEditor {
    pub fn new(data_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| {
            EditableSecretList::new(
                Labels {
                    key_placeholder: "owner/repo, owner/*, or *".into(),
                    secret_placeholder: "GitHub token (write-only)".into(),
                    secret_stored_placeholder: "token stored: type to replace".into(),
                    add: "Add".into(),
                    remove_tooltip: "Remove this row. A stored token also needs Save, then Apply, to leave the orchestrator".into(),
                    stored_note: "A token is stored for this row. Leave the field empty to keep it. Removing the row needs Save, then Apply, to take the token off the orchestrator.".into(),
                },
                cx,
            )
        });
        let subscription = cx.subscribe(&list, |this, _, event: &ListEvent, cx| match event {
            ListEvent::Added => this.record(cx, "orchestrator_github_add_row", Source::Click, Vec::new()),
            ListEvent::Removed { stored } => {
                let notices = if *stored {
                    vec!["The row had a stored token: removing it needs Save, then Apply".to_string()]
                } else {
                    Vec::new()
                };
                this.record(cx, "orchestrator_github_remove_row", Source::Click, notices)
            }
            ListEvent::Leave => cx.emit(Leave),
        });
        let mut this = Self {
            data_root,
            list,
            config: Config::default(),
            loaded: false,
            saving: false,
            applying: false,
            error: None,
            status: None,
            _subscription: subscription,
        };
        this.reload(window, cx);
        this
    }

    /// Re-read the list and which tokens are stored, off the UI thread.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.data_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let config = Config::load(&root).map_err(|e| format!("{e:#}"))?;
                    let stored = config
                        .tokens
                        .iter()
                        .map(|e| (e.id.clone(), store::has_token(&root, &e.id)))
                        .collect::<HashMap<_, _>>();
                    Ok::<_, String>((config, stored))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loaded = true;
                match loaded {
                    Ok((config, stored)) => {
                        let rows = rows_from_config(&config, &stored);
                        this.config = config;
                        this.list.update(cx, |list, cx| list.set_rows(rows, window, cx));
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| list.focus(window, cx));
    }

    /// A `UserAction` with what was on screen: Add, Save and Apply (Save is
    /// primary), never a token.
    fn record(&self, cx: &mut App, action: &str, source: Source, notices: Vec<String>) {
        let button = |id: &str, label: &str, primary: bool, disabled: bool| tod_journey::PresentedAction {
            id: id.to_string(),
            label: label.to_string(),
            primary,
            disabled,
        };
        let presented = tod_journey::Presented {
            actions: vec![
                button("orchestrator_github_add_row", "Add", false, false),
                button("orchestrator_github_save", "Save", true, self.saving || !self.loaded),
                button("orchestrator_github_apply", "Apply to the orchestrator", false, self.applying || !self.loaded),
            ],
            focused: Some(action.to_string()),
            notices,
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

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.loaded {
            return;
        }
        let rows = self.list.read(cx).values(cx);
        let plan = match plan_save(&self.config, &rows) {
            Ok(plan) => plan,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.record(cx, "orchestrator_github_save", Source::Click, Vec::new());
        self.saving = true;
        self.error = None;
        self.status = None;
        cx.notify();
        let root = self.data_root.clone();
        let config = plan.config.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { execute(&root, &plan) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(stored) => {
                        let rows = rows_from_config(&config, &stored);
                        this.config = config;
                        this.list.update(cx, |list, cx| list.set_rows(rows, window, cx));
                        this.status = Some(Ok(
                            "Saved. Nothing reaches the orchestrator until you press Apply.".to_string()
                        ));
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying || !self.loaded {
            return;
        }
        self.record(cx, "orchestrator_github_apply", Source::Click, Vec::new());
        self.applying = true;
        self.status = None;
        cx.notify();
        let root = self.data_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tod_core::orchestrator_github::apply(&root) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.applying = false;
                this.status = Some(match result {
                    Ok(applied) => Ok(format!("Applied: {}.", applied_summary(applied.tokens, applied.routes))),
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for OrchestratorGithubEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = v_flex()
            .gap_2()
            .w_full()
            .max_w(px(720.))
            .child(style::text_muted(div()).child(
                "The orchestrator reads GitHub (merges, releases) for waiting nodes so their sandboxes need not wake. \
                 A token covers one organization: list each repository with the token that reads it; \
                 the most specific pattern wins. Save keeps the list here; nothing reaches the orchestrator until Apply.",
            ));
        if !self.loaded {
            col = col.child(style::text_dense_muted(div()).child("Loading…"));
        }
        col = col.child(self.list.clone());
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("orch-gh-save")
                        .label(if self.saving { "Saving…" } else { "Save" })
                        .primary()
                        .compact()
                        .disabled(self.saving || !self.loaded)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                )
                .child(
                    Button::new("orch-gh-apply")
                        .label(if self.applying { "Applying…" } else { "Apply to the orchestrator" })
                        .compact()
                        .disabled(self.applying || !self.loaded)
                        .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                ),
        );
        if let Some(err) = self.error.clone() {
            col = col.child(style::text_error(div()).child(selectable_text("orch-gh-error", err, window, cx)));
        }
        if let Some(status) = self.status.clone() {
            col = col.child(match status {
                Ok(t) => style::text_dense_muted(div()).child(selectable_text("orch-gh-status", t, window, cx)),
                Err(t) => style::text_error(div()).child(selectable_text("orch-gh-status", t, window, cx)),
            });
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, repos: &[&str]) -> Entry {
        Entry { id: id.into(), label: id.into(), repos: repos.iter().map(|s| s.to_string()).collect() }
    }
    fn row(key: &str, secret: &str, stored: bool, tag: &str) -> RowValues {
        RowValues { key: key.into(), secret: secret.into(), stored, tag: tag.into() }
    }

    #[test]
    fn a_multi_repo_entry_splits_into_one_row_per_repo() {
        let config = Config { tokens: vec![entry("acme", &["acme/api", "acme/*"]), entry("x", &["x/y"])] };
        let stored = HashMap::from([("acme".to_string(), true)]);
        let rows = rows_from_config(&config, &stored);
        assert_eq!(
            rows,
            vec![
                RowData { key: "acme/api".into(), stored: true, tag: "acme".into() },
                RowData { key: "acme/*".into(), stored: true, tag: "acme".into() },
                RowData { key: "x/y".into(), stored: false, tag: "x".into() },
            ]
        );
    }

    #[test]
    fn untouched_single_rows_keep_their_ids_and_tokens() {
        let old = Config { tokens: vec![entry("acme-api", &["acme/api"])] };
        let plan = plan_save(&old, &[row("acme/api", "", true, "acme-api")]).unwrap();
        assert_eq!(
            plan.config.tokens,
            vec![Entry { id: "acme-api".into(), label: "acme/api".into(), repos: vec!["acme/api".into()] }]
        );
        assert!(plan.ops.is_empty());
    }

    #[test]
    fn a_legacy_entry_is_split_and_its_token_carried_over() {
        let old = Config { tokens: vec![entry("acme", &["acme/api", "acme/*"])] };
        let rows = [row("acme/api", "", true, "acme"), row("acme/*", "", true, "acme")];
        let plan = plan_save(&old, &rows).unwrap();
        let ids: Vec<&str> = plan.config.tokens.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["acme-api", "acme"]);
        assert!(plan.config.tokens.iter().all(|e| e.repos.len() == 1 && e.label == e.repos[0]));
        // "acme/*" keeps the id "acme" (its token is already there); only "acme/api" copies.
        assert_eq!(plan.ops, vec![TokenOp::Copy { from: "acme".into(), to: "acme-api".into() }]);
        // The old id lives on as a new one, so it is not deleted.
        assert!(!plan.ops.iter().any(|op| matches!(op, TokenOp::Delete { .. })));
    }

    #[test]
    fn an_old_id_no_row_uses_is_deleted() {
        let old = Config { tokens: vec![entry("acme", &["acme/api", "other/x"])] };
        let rows = [row("acme/api", "", true, "acme"), row("other/x", "", true, "acme")];
        let plan = plan_save(&old, &rows).unwrap();
        assert!(plan.ops.contains(&TokenOp::Delete { id: "acme".into() }));
        assert!(plan.ops.contains(&TokenOp::Copy { from: "acme".into(), to: "acme-api".into() }));
        assert!(plan.ops.contains(&TokenOp::Copy { from: "acme".into(), to: "other-x".into() }));
    }

    #[test]
    fn removing_a_row_deletes_its_token() {
        let old = Config { tokens: vec![entry("a", &["a/b"]), entry("c", &["c/d"])] };
        let plan = plan_save(&old, &[row("c/d", "", true, "c")]).unwrap();
        assert_eq!(plan.ops, vec![TokenOp::Delete { id: "a".into() }]);
        assert_eq!(plan.config.tokens.len(), 1);
    }

    #[test]
    fn a_typed_token_replaces_the_stored_one() {
        let old = Config { tokens: vec![entry("a-b", &["a/b"])] };
        let plan = plan_save(&old, &[row("a/b", "ghp_new", true, "a-b")]).unwrap();
        assert_eq!(plan.ops, vec![TokenOp::Set { id: "a-b".into(), token: "ghp_new".into() }]);
    }

    #[test]
    fn a_new_row_needs_a_token_and_a_valid_unique_pattern() {
        let old = Config::default();
        let err = plan_save(&old, &[row("a/b", "", false, "")]).unwrap_err();
        assert!(err.starts_with("Row 1:"), "{err}");
        let err = plan_save(&old, &[row("a/b", "t", false, ""), row("nonsense", "t", false, "")]).unwrap_err();
        assert!(err.starts_with("Row 2:") && err.contains("nonsense"), "{err}");
        let err = plan_save(&old, &[row("a/b", "t", false, ""), row("A/B", "t", false, "")]).unwrap_err();
        assert_eq!(err, "Row 2: A/B is already in row 1.");
        let err = plan_save(&old, &[row("", "t", false, "")]).unwrap_err();
        assert!(err.starts_with("Row 1:"));
        let ok = plan_save(&old, &[row("a/b", "t", false, "")]).unwrap();
        assert_eq!(ok.config.tokens[0].id, "a-b");
        assert_eq!(ok.ops, vec![TokenOp::Set { id: "a-b".into(), token: "t".into() }]);
    }

    #[test]
    fn a_blank_row_is_ignored_and_ids_never_collide() {
        let old = Config::default();
        let plan = plan_save(&old, &[row("", "", false, ""), row("a/b", "t", false, ""), row("a-b/*", "t", false, "")])
            .unwrap();
        assert_eq!(plan.config.tokens.len(), 2);
        assert_ne!(plan.config.tokens[0].id, plan.config.tokens[1].id);
    }

    #[test]
    fn errors_never_carry_a_token() {
        let err = plan_save(&Config::default(), &[row("bad", "ghp_SECRET", false, "")]).unwrap_err();
        assert!(!err.contains("ghp_SECRET"));
    }

    #[test]
    fn summary_has_plurals() {
        assert_eq!(applied_summary(1, 1), "1 token, 1 route");
        assert_eq!(applied_summary(2, 3), "2 tokens, 3 routes");
    }
}
