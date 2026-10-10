//! The Lifecycle config capability's editor (a section of the task editor):
//! the skills the agent for each lifecycle phase uses, for this node and
//! everything below it.
//!
//! Each phase is a row showing what applies to this node and where it comes
//! from. Editing a phase is a form with one input (skill names, comma
//! separated) and an explicit Save; "Use inherited" removes the phase from this
//! node so the nearest ancestor's applies again, and saving an empty list turns
//! the phase's skills off. Names are free text and are not checked: a skill may
//! be set up after the config.
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
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::lifecycle_config::{self, PHASES, Resolved, Skills};
use tod_store::outline::OutlineMutation;
use uuid::Uuid;

pub struct LifecycleConfigEditor {
    fleet: Arc<FleetStore>,
    node: Uuid,
    enabled: bool,
    /// What this node itself lists, by phase.
    own: Skills,
    /// What applies to this node for each phase, after inheritance.
    effective: Vec<(&'static str, Option<Resolved>)>,
    titles: HashMap<Uuid, String>,
    form: Option<Form>,
    error: Option<String>,
}

struct Form {
    phase: &'static str,
    skills: Entity<InputState>,
    saving: bool,
}

/// The label a phase is shown under.
fn phase_label(phase: &str) -> &'static str {
    match phase {
        "proposed" => "Proposed",
        "design" => "Design",
        "planning" => "Planning",
        "implement" => "Implement",
        "verify" => "Verify",
        "review" => "Review",
        "fix" => "Fix review findings",
        "pr" => "Pull request",
        "merged" => "Merged",
        "released" => "Released",
        "learn" => "Learn",
        _ => "Phase",
    }
}

impl LifecycleConfigEditor {
    pub fn new(fleet: Arc<FleetStore>, node: Uuid, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            fleet,
            node,
            enabled: false,
            own: Skills::new(),
            effective: Vec::new(),
            titles: HashMap::new(),
            form: None,
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
                    let mut effective = Vec::new();
                    let mut titles = HashMap::new();
                    for phase in PHASES {
                        let resolved =
                            fleet.read(|conn| lifecycle_config::resolve(conn, node, phase)).ok().flatten();
                        if let Some(r) = resolved.as_ref().filter(|r| r.inherited)
                            && !titles.contains_key(&r.source_node)
                        {
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
                        effective.push((phase, resolved));
                    }
                    (own, effective, titles)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                (this.own, this.effective, this.titles) = loaded;
                cx.notify();
            });
        })
        .detach();
    }

    fn open_form(&mut self, phase: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.own.get(phase).map(|s| s.join(", ")).unwrap_or_default();
        let skills = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("skill names, comma-separated (empty: none)");
            state.set_value(current, window, cx);
            state
        });
        self.form = Some(Form { phase, skills, saving: false });
        self.error = None;
        cx.notify();
    }

    /// Write this node's skills with `change` applied to a copy of them.
    fn write(&mut self, change: impl FnOnce(&mut Skills) + Send + 'static, cx: &mut Context<Self>) {
        let mut skills = self.own.clone();
        change(&mut skills);
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

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let phase = form.phase;
        let names: Vec<String> = form
            .skills
            .read(cx)
            .text()
            .to_string()
            .split(',')
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        self.write(move |skills| drop(skills.insert(phase.to_string(), names)), cx);
    }

    fn use_inherited(&mut self, phase: &'static str, cx: &mut Context<Self>) {
        self.write(move |skills| drop(skills.remove(phase)), cx);
    }

    fn render_row(
        &self,
        index: usize,
        phase: &'static str,
        resolved: &Option<Resolved>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let has_own = self.own.contains_key(phase);
        let summary = match resolved {
            None => "no skills".to_string(),
            Some(r) if r.skills.is_empty() => "none (turned off)".to_string(),
            Some(r) => r.skills.join(", "),
        };
        let source = resolved.as_ref().filter(|r| r.inherited).map(|r| {
            format!(
                "from {}",
                self.titles.get(&r.source_node).map_or("an ancestor", String::as_str)
            )
        });
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
            .child(div().text_xs().text_color(muted).child(selectable_text(
                format!("lc-summary-{index}"),
                match &source {
                    Some(from) => format!("{summary} · {from}"),
                    None => summary,
                },
                window,
                cx,
            )))
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
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Used in this order. Leave empty to give this node and what is below it no skills for the phase."),
            )
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
            "Skills the agent for each phase uses on this node and everything below it. A phase set lower down replaces this one's for that phase only."
        } else {
            "Skills inherited from an ancestor. Enable the capability to set this node's own."
        }));
        let rows: Vec<(&'static str, Option<Resolved>)> = self.effective.clone();
        for (i, (phase, resolved)) in rows.iter().enumerate() {
            col = col.child(self.render_row(i, phase, resolved, window, cx));
        }
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
