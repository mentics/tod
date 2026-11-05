//! The Lifecycle config capability's editor (a section of the task editor):
//! the skills the agent for each lifecycle phase uses, and the model and
//! effort it runs with, for this node and everything below it.
//!
//! Each phase is a row showing what applies to this node and where it comes
//! from. Editing a phase is a form with three inputs (skill names, comma
//! separated, a semicolon starting a group that runs in its own session; the
//! model; the effort) and an explicit Save; "Use inherited" removes the phase
//! from this node so the nearest ancestor's applies again, and saving an empty
//! list turns the phase's skills off, while an empty model or effort leaves
//! that one to an ancestor, the node's Agent capability, or the settings.
//! Names are free text and are not checked: a skill may be set up after the
//! config, and a model the platform does not know falls to its default at
//! launch, as the Agent capability's does.
//!
//! Focus: the rows are navigated with the mouse; a phase's form is the edit
//! mode (its input exists only while it is open), and Escape closes it.

use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Window, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::lifecycle_config::{
    self, Agents, Check, Checks, DeleteSandbox, PHASES, PhaseAgent, PrDraftReview, Resolved, ResolvedAgent,
    ResolvedChecks, Skills,
};
use tod_store::outline::OutlineMutation;
use uuid::Uuid;

pub struct LifecycleConfigEditor {
    fleet: Arc<FleetStore>,
    node: Uuid,
    enabled: bool,
    /// What this node itself lists, by phase.
    own: Skills,
    /// The model and effort this node itself sets, by phase.
    own_agents: Agents,
    /// The checks this node itself lists, by phase.
    own_checks: Checks,
    /// The checks that apply to this node for each phase, after inheritance.
    effective_checks: HashMap<&'static str, ResolvedChecks>,
    /// What applies to this node for each phase, after inheritance.
    effective: Vec<(&'static str, Option<Resolved>, ResolvedAgent)>,
    titles: HashMap<Uuid, String>,
    /// Whether the sandbox is deleted at `done`, after inheritance, and
    /// whether this node itself says.
    delete_sandbox: DeleteSandbox,
    own_delete_sandbox: Option<bool>,
    /// Whether `pr_draft` includes the review steps, after inheritance, and
    /// whether this node itself says.
    pr_draft_review: PrDraftReview,
    own_pr_draft_review: Option<bool>,
    form: Option<Form>,
    /// The import/export box: all of this node's checks as YAML.
    transfer: Option<Entity<TextareaState>>,
    error: Option<String>,
}

struct Form {
    phase: &'static str,
    skills: Entity<InputState>,
    model: Entity<InputState>,
    effort: Entity<InputState>,
    /// One row per command the phase's gate runs: a label and the command.
    checks: Vec<CheckRow>,
    saving: bool,
}

struct CheckRow {
    label: Entity<InputState>,
    command: Entity<InputState>,
    dir: Entity<InputState>,
    changed: Entity<InputState>,
    pattern: Entity<InputState>,
}

/// The label a phase is shown under.
fn phase_label(phase: &str) -> &'static str {
    match phase {
        "proposed" => "Proposed",
        "design" => "Design",
        "planning" => "Planning",
        "implement" => "Implement",
        "verify" => "Verify",
        "review" => "Review (finds problems)",
        "fix" => "Fix (resolves the review's findings)",
        "pr" => "Pull request",
        "pr_draft" => "Pull request draft",
        "pr_review" => "Pull request review",
        "merged" => "Merged",
        "released" => "Released",
        "learn" => "Learn",
        _ => "Phase",
    }
}

/// What a phase's skills are for, where the phase name alone does not say.
/// Review and Fix are two agents in two sessions, each with its own list.
fn phase_blurb(phase: &str) -> Option<&'static str> {
    match phase {
        "review" => Some(
            "For the agent that reviews the change and records findings. Optional: leave it empty \
             and the review still runs; add project-specific things to look for.",
        ),
        "fix" => Some(
            "For the separate agent that fixes what the review found, then is reviewed again. \
             Optional: leave it empty and the fix still runs; add project-specific ways to fix.",
        ),
        _ => None,
    }
}

impl LifecycleConfigEditor {
    pub fn new(fleet: Arc<FleetStore>, node: Uuid, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            fleet,
            node,
            enabled: false,
            own: Skills::new(),
            own_agents: Agents::new(),
            own_checks: Checks::new(),
            effective_checks: HashMap::new(),
            effective: Vec::new(),
            titles: HashMap::new(),
            delete_sandbox: DeleteSandbox { enabled: true, source_node: None },
            own_delete_sandbox: None,
            pr_draft_review: PrDraftReview { enabled: false, source_node: None },
            own_pr_draft_review: None,
            form: None,
            transfer: None,
            error: None,
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
            self.reload(cx);
        }
    }

    /// Re-read the node's skills, and what each phase resolves to, off the UI thread.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let (fleet, node) = (self.fleet.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let own = fleet.read(|conn| lifecycle_config::skills(conn, node)).unwrap_or_default();
                    let own_agents = fleet.read(|conn| lifecycle_config::agents(conn, node)).unwrap_or_default();
                    let own_checks = fleet.read(|conn| lifecycle_config::checks(conn, node)).unwrap_or_default();
                    let mut effective_checks = HashMap::new();
                    let mut effective = Vec::new();
                    let mut titles = HashMap::new();
                    let mut title_of = |source: Uuid| {
                        if source == node || titles.contains_key(&source) {
                            return;
                        }
                        let title = fleet
                            .read(|conn| {
                                Ok(tod_store::outline::repos::NodeRepo::new(conn).get(source)?.map(|n| n.title))
                            })
                            .ok()
                            .flatten();
                        titles.insert(source, title.unwrap_or_else(|| "an ancestor".into()));
                    };
                    for phase in PHASES {
                        let resolved =
                            fleet.read(|conn| lifecycle_config::resolve(conn, node, phase)).ok().flatten();
                        if let Some(r) = resolved.as_ref() {
                            title_of(r.source_node);
                        }
                        let agent = fleet
                            .read(|conn| lifecycle_config::resolve_agent(conn, node, phase))
                            .unwrap_or_default();
                        for source in [&agent.model, &agent.effort].into_iter().flatten().map(|(_, s)| *s) {
                            title_of(source);
                        }
                        if let Some(checks) =
                            fleet.read(|conn| lifecycle_config::resolve_checks(conn, node, phase)).ok().flatten()
                        {
                            title_of(checks.source_node);
                            effective_checks.insert(phase, checks);
                        }
                        effective.push((phase, resolved, agent));
                    }
                    let delete = fleet
                        .read(|conn| lifecycle_config::resolve_delete_sandbox(conn, node))
                        .unwrap_or(DeleteSandbox { enabled: true, source_node: None });
                    let own_delete = fleet
                        .read(|conn| lifecycle_config::own_delete_sandbox(conn, node))
                        .unwrap_or(None);
                    if let Some(source) = delete.source_node {
                        title_of(source);
                    }
                    let review = fleet
                        .read(|conn| lifecycle_config::resolve_pr_draft_review(conn, node))
                        .unwrap_or(PrDraftReview { enabled: false, source_node: None });
                    let own_review = fleet
                        .read(|conn| lifecycle_config::own_pr_draft_review(conn, node))
                        .unwrap_or(None);
                    if let Some(source) = review.source_node {
                        title_of(source);
                    }
                    (own, own_agents, effective, titles, delete, own_delete, review, own_review, own_checks, effective_checks)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                (
                    this.own,
                    this.own_agents,
                    this.effective,
                    this.titles,
                    this.delete_sandbox,
                    this.own_delete_sandbox,
                    this.pr_draft_review,
                    this.own_pr_draft_review,
                    this.own_checks,
                    this.effective_checks,
                ) = loaded;
                cx.notify();
            });
        })
        .detach();
    }

    fn open_form(&mut self, phase: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.own.get(phase).map(|s| lifecycle_config::format_list(s)).unwrap_or_default();
        let skills = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("skills: a, b; c runs c in its own session (empty: none)");
            state.set_value(current, window, cx);
            state
        });
        let agent = self.own_agents.get(phase).cloned().unwrap_or_default();
        let model = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("model (empty: inherited)");
            state.set_value(agent.model.unwrap_or_default(), window, cx);
            state
        });
        let effort = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("effort (empty: inherited)");
            state.set_value(agent.effort.unwrap_or_default(), window, cx);
            state
        });
        let checks = self
            .own_checks
            .get(phase)
            .map(|list| list.iter().map(|c| Self::check_row(c, window, cx)).collect())
            .unwrap_or_default();
        self.form = Some(Form { phase, skills, model, effort, checks, saving: false });
        self.error = None;
        cx.notify();
    }

    fn check_row(check: &Check, window: &mut Window, cx: &mut Context<Self>) -> CheckRow {
        let mut input = |placeholder: &str, value: &str, cx: &mut Context<Self>| {
            let value = value.to_string();
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        CheckRow {
            label: input("label (optional)", &check.label, cx),
            command: input("command (exit 0 passes)", &check.command, cx),
            dir: input("run in (e.g. flagship)", &check.dir, cx),
            changed: input("only if changed (default: run in)", &check.changed, cx),
            pattern: input("files (e.g. *.py)", &check.pattern, cx),
        }
    }

    fn add_check_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::check_row(&Check::default(), window, cx);
        if let Some(form) = self.form.as_mut() {
            form.checks.push(row);
        }
        cx.notify();
    }

    /// Open the import/export box, filled with this node's own checks as YAML
    /// (empty when it has none, ready to paste into).
    fn open_transfer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = if self.own_checks.is_empty() {
            String::new()
        } else {
            match serde_yaml::to_string(&self.own_checks) {
                Ok(text) => text,
                Err(e) => {
                    self.error = Some(format!("could not write the checks as YAML: {e}"));
                    cx.notify();
                    return;
                }
            }
        };
        self.transfer = Some(cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .rows(14)
                .placeholder("phase:\n  - label: pyright\n    command: uv run basedpyright\n    dir: flagship\n    pattern: \"*.py\"");
            state.set_value(text, window, cx);
            state
        }));
        self.error = None;
        cx.notify();
    }

    /// Replace all of this node's own checks with the YAML in the box.
    fn apply_transfer(&mut self, cx: &mut Context<Self>) {
        let Some(box_) = &self.transfer else { return };
        let text = box_.read(cx).text().to_string();
        let checks: Checks = if text.trim().is_empty() {
            Checks::new()
        } else {
            match serde_yaml::from_str(&text) {
                Ok(checks) => checks,
                Err(e) => {
                    self.error = Some(format!("the checks are not valid: {e}"));
                    cx.notify();
                    return;
                }
            }
        };
        let (fleet, node) = (self.fleet.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    fleet
                        .enqueue_outline(OutlineMutation::SetNodeLifecycleChecks { node_id: node, checks })
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.transfer = None;
                        this.error = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_transfer(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut block = v_flex()
            .gap_1()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(div().text_sm().font_semibold().child("Checks, all phases at once"));
        if let Some(box_) = &self.transfer {
            block = block
                .child(Textarea::new(box_).w_full().h(gpui::px(260.)))
                .child(div().text_xs().text_color(muted).child(
                    "YAML: a phase, then its checks (label, command, dir, changed, pattern). Apply replaces \
                     every check this node lists, in every phase. Empty clears them.",
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("lc-transfer-apply")
                                .label("Apply")
                                .primary()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| this.apply_transfer(cx))),
                        )
                        .child(
                            Button::new("lc-transfer-close")
                                .label("Close")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.transfer = None;
                                    cx.notify();
                                })),
                        ),
                );
        } else {
            block = block.child(
                h_flex().gap_2().child(
                    Button::new("lc-transfer-open")
                        .label("Import / export checks")
                        .ghost()
                        .compact()
                        .disabled(!self.enabled)
                        .on_click(cx.listener(|this, _, window, cx| this.open_transfer(window, cx))),
                ),
            );
        }
        block.into_any_element()
    }

    fn remove_check_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(form) = self.form.as_mut()
            && index < form.checks.len()
        {
            form.checks.remove(index);
        }
        cx.notify();
    }

    /// Write this node's skills, model, effort and checks with `change`
    /// applied to a copy of them.
    fn write(
        &mut self,
        change: impl FnOnce(&mut Skills, &mut Agents, &mut Checks) + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let mut skills = self.own.clone();
        let mut agents = self.own_agents.clone();
        let mut checks = self.own_checks.clone();
        change(&mut skills, &mut agents, &mut checks);
        if let Some(form) = self.form.as_mut() {
            form.saving = true;
        }
        let (fleet, node) = (self.fleet.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    fleet
                        .enqueue_outline(OutlineMutation::SetNodeLifecycleConfig { node_id: node, skills })
                        .and_then(|()| {
                            fleet.enqueue_outline(OutlineMutation::SetNodeLifecycleAgents { node_id: node, agents })
                        })
                        .and_then(|()| {
                            fleet.enqueue_outline(OutlineMutation::SetNodeLifecycleChecks { node_id: node, checks })
                        })
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.form = None;
                        this.error = None;
                    }
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

    /// Set what this node says about deleting its sandbox at `done`
    /// (`None`: leave it to an ancestor again).
    fn write_delete_sandbox(&mut self, value: Option<bool>, cx: &mut Context<Self>) {
        let (fleet, node) = (self.fleet.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    fleet
                        .enqueue_outline(OutlineMutation::SetNodeLifecycleDeleteSandbox {
                            node_id: node,
                            delete_sandbox: value,
                        })
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err();
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Set whether `pr_draft` includes the review steps for this node
    /// (`None`: leave it to an ancestor again).
    fn write_pr_draft_review(&mut self, value: Option<bool>, cx: &mut Context<Self>) {
        let (fleet, node) = (self.fleet.clone(), self.node);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    fleet
                        .enqueue_outline(OutlineMutation::SetNodeLifecyclePrDraftReview {
                            node_id: node,
                            pr_draft_review: value,
                        })
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err();
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let phase = form.phase;
        let text = form.skills.read(cx).text().to_string();
        let names = match lifecycle_config::parse_list(&text) {
            Ok(names) => names,
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        let field = |input: &Entity<InputState>| {
            Some(input.read(cx).text().to_string().trim().to_string()).filter(|v| !v.is_empty())
        };
        let agent = PhaseAgent { model: field(&form.model), effort: field(&form.effort) };
        let mut checks = Vec::new();
        for row in &form.checks {
            let (label, command) = (field(&row.label), field(&row.command));
            let scope = [field(&row.dir), field(&row.changed), field(&row.pattern)];
            match (label, command) {
                (None, None) if scope.iter().all(Option::is_none) => {}
                (label, Some(command)) => checks.push(Check {
                    label: label.unwrap_or_default(),
                    command,
                    dir: scope[0].clone().unwrap_or_default(),
                    changed: scope[1].clone().unwrap_or_default(),
                    pattern: scope[2].clone().unwrap_or_default(),
                }),
                (label, None) => {
                    self.error = Some(format!("the check `{}` has no command", label.unwrap_or_default()));
                    cx.notify();
                    return;
                }
            }
        }
        self.write(
            move |skills, agents, all_checks| {
                if checks.is_empty() {
                    all_checks.remove(phase);
                } else {
                    all_checks.insert(phase.to_string(), checks);
                }
                skills.insert(phase.to_string(), names);
                if agent.is_empty() {
                    agents.remove(phase);
                } else {
                    agents.insert(phase.to_string(), agent);
                }
            },
            cx,
        );
    }

    fn use_inherited(&mut self, phase: &'static str, cx: &mut Context<Self>) {
        self.write(
            move |skills, agents, checks| {
                checks.remove(phase);
                skills.remove(phase);
                agents.remove(phase);
            },
            cx,
        );
    }

    fn render_row(
        &self,
        index: usize,
        phase: &'static str,
        resolved: &Option<Resolved>,
        agent: &ResolvedAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let has_own = self.own.contains_key(phase)
            || self.own_agents.contains_key(phase)
            || self.own_checks.contains_key(phase);
        let from = |source: Uuid| {
            (source != self.node)
                .then(|| format!(" · from {}", self.titles.get(&source).map_or("an ancestor", String::as_str)))
                .unwrap_or_default()
        };
        let skills = match resolved {
            None => "no skills".to_string(),
            Some(r) if r.skills.is_empty() => "none (no extra skills)".to_string(),
            Some(r) => lifecycle_config::format_list(&r.skills),
        };
        let mut summary = format!(
            "{skills}{}",
            resolved.as_ref().map(|r| from(r.source_node)).unwrap_or_default()
        );
        if let Some((model, source)) = &agent.model {
            summary.push_str(&format!("\nmodel {model}{}", from(*source)));
        }
        if let Some((effort, source)) = &agent.effort {
            summary.push_str(&format!("\neffort {effort}{}", from(*source)));
        }
        if let Some(checks) = self.effective_checks.get(phase) {
            if checks.checks.is_empty() {
                summary.push_str(&format!("\nno checks{}", from(checks.source_node)));
            } else {
                for check in &checks.checks {
                    summary.push_str(&format!("\ncheck {}: {}", check.name(), check.command));
                }
                summary.push_str(&from(checks.source_node).replacen(" · ", "\nchecks ", 1));
            }
        }
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
                    .child(div().text_sm().font_semibold().child(phase_label(phase)))
                    .child(div().flex_1())
                    .when(self.enabled && self.form.is_none(), |el| {
                        el.child(
                            Button::new(("lc-edit", index))
                                .label("Edit")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(move |this, _, window, cx| this.open_form(phase, window, cx))),
                        )
                        .when(has_own, |el| {
                            el.child(
                                Button::new(("lc-inherit", index))
                                    .label("Use inherited")
                                    .ghost()
                                    .compact()
                                    .on_click(cx.listener(move |this, _, _, cx| this.use_inherited(phase, cx))),
                            )
                        })
                    }),
            )
            .when_some(phase_blurb(phase), |el, blurb| {
                el.child(div().text_xs().text_color(muted).child(blurb))
            })
            .child(div().text_xs().text_color(muted).child(selectable_text(
                format!("lc-summary-{index}"),
                summary,
                window,
                cx,
            )))
            .when(phase == "pr_draft", |el| el.child(self.render_pr_draft_review(cx)))
            .into_any_element()
    }

    /// The "Delete sandbox" choice for the final state (`done`), which has no
    /// agent and so no row of its own above.
    fn render_delete_sandbox(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let from = self.delete_sandbox.source_node.filter(|source| *source != self.node).map(|source| {
            format!("from {}", self.titles.get(&source).map_or("an ancestor", String::as_str))
        });
        h_flex()
            .gap_2()
            .items_center()
            .child(
                Checkbox::new("lc-delete-sandbox")
                    .label("Delete sandbox")
                    .checked(self.delete_sandbox.enabled)
                    .disabled(!self.enabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| this.write_delete_sandbox(Some(*checked), cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("when a node reaches done, its cloud sandbox is deleted (uncommitted work is kept)"),
            )
            .when_some(from, |el, from| el.child(div().text_xs().text_color(muted).child(from)))
            .when(self.enabled && self.own_delete_sandbox.is_some(), |el| {
                el.child(
                    Button::new("lc-delete-sandbox-inherit")
                        .label("Use inherited")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.write_delete_sandbox(None, cx))),
                )
            })
            .into_any_element()
    }

    /// The "Include review steps" choice on the `pr_draft` row.
    fn render_pr_draft_review(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let from = self.pr_draft_review.source_node.filter(|source| *source != self.node).map(|source| {
            format!("from {}", self.titles.get(&source).map_or("an ancestor", String::as_str))
        });
        h_flex()
            .gap_2()
            .items_center()
            .child(
                Checkbox::new("lc-pr-draft-review")
                    .label("Include review steps")
                    .checked(self.pr_draft_review.enabled)
                    .disabled(!self.enabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| this.write_pr_draft_review(Some(*checked), cx))),
            )
            .child(div().text_xs().text_color(muted).child(
                "once the bots and checks are done, run the Review phase's code review on the pull request \
                 and fix what it finds with the Fix phase's skills",
            ))
            .when_some(from, |el, from| el.child(div().text_xs().text_color(muted).child(from)))
            .when(self.enabled && self.own_pr_draft_review.is_some(), |el| {
                el.child(
                    Button::new("lc-pr-draft-review-inherit")
                        .label("Use inherited")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.write_pr_draft_review(None, cx))),
                )
            })
            .into_any_element()
    }

    fn render_form(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(form) = &self.form else { return div().into_any_element() };
        v_flex()
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
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .child(format!("Skills for {}", phase_label(form.phase))),
            )
            .child(Input::new(&form.skills))
            .when_some(phase_blurb(form.phase), |el, blurb| {
                el.child(div().text_xs().text_color(cx.theme().muted_foreground).child(blurb))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Used in this order. Leave empty to give this node and what is below it no skills for the phase."),
            )
            .child(div().text_xs().font_semibold().child("Model and effort"))
            .child(
                h_flex()
                    .gap_2()
                    .w_full()
                    .child(div().flex_1().child(Input::new(&form.model)))
                    .child(div().flex_1().child(Input::new(&form.effort))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("What the agent for this phase runs with, over the node's Agent capability and the settings. Leave one empty to inherit it."),
            )
            .child(div().text_xs().font_semibold().child("Checks"))
            .children(form.checks.iter().enumerate().map(|(i, row)| {
                v_flex()
                    .gap_1()
                    .w_full()
                    .child(
                        h_flex()
                            .gap_2()
                            .w_full()
                            .items_center()
                            .child(div().w_40().child(Input::new(&row.label)))
                            .child(div().flex_1().child(Input::new(&row.command)))
                            .child(
                                Button::new(("lc-check-remove", i))
                                    .label("Remove")
                                    .ghost()
                                    .compact()
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_check_row(i, cx))),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .w_full()
                            .child(div().flex_1().child(Input::new(&row.dir)))
                            .child(div().flex_1().child(Input::new(&row.changed)))
                            .child(div().flex_1().child(Input::new(&row.pattern))),
                    )
            }))
            .child(
                h_flex().child(
                    Button::new("lc-check-add")
                        .label("Add check")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| this.add_check_row(window, cx))),
                ),
            )
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                "Commands (type checking, lint, tests) the app runs in the node's files before this phase's gate \
                 and again after every change. A command that exits 0 passes; any other exit fails, and its \
                 output goes to the fix agent. The label names it in messages. \"Run in\" is a directory under \
                 the node's files (a submodule, say); \"only if changed\" (default: the same directory) and \
                 \"files\" make the check apply only when a matching file there changed on this branch, \
                 committed or not, and hand those files to the command as $TOD_CHANGED_FILES, one per line. \
                 With none listed here, the inherited checks apply.",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("lc-save")
                            .label("Save")
                            .primary()
                            .compact()
                            .disabled(form.saving)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .child(
                        Button::new("lc-cancel")
                            .label("Cancel")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.form = None;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.error.clone(), |el, err| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(selectable_text("lc-form-error", err, window, cx)),
                )
            })
            .into_any_element()
    }
}

impl Render for LifecycleConfigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut col = v_flex().gap_2().px_3().pb_3().w_full();
        col = col.child(div().text_xs().text_color(muted).child(if self.enabled {
            "Skills, model and effort for the agent of each phase on this node and everything below it. A phase set lower down replaces this one's for that phase only."
        } else {
            "Skills, model and effort inherited from an ancestor. Enable the capability to set this node's own."
        }));
        let rows: Vec<(&'static str, Option<Resolved>, ResolvedAgent)> = self.effective.clone();
        for (i, (phase, resolved, agent)) in rows.iter().enumerate() {
            col = col.child(self.render_row(i, phase, resolved, agent, window, cx));
        }
        col = col.child(
            v_flex()
                .gap_0p5()
                .w_full()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(div().text_sm().font_semibold().child("Done"))
                .child(self.render_delete_sandbox(cx)),
        );
        col = col.child(self.render_transfer(cx));
        if self.form.is_some() {
            col = col.child(self.render_form(window, cx));
        } else if let Some(err) = &self.error {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(selectable_text("lc-error", err.clone(), window, cx)),
            );
        }
        col
    }
}

impl EventEmitter<()> for LifecycleConfigEditor {}
