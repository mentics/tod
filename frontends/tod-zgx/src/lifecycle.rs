use std::collections::BTreeMap;
use tod_core::lifecycle::flow::{self, WorkAction, WorkCycle};
use zgx::{
    canvas::{Canvas, Path, Stroke},
    compose::prelude::*,
    reactive::Signal,
    semantics::Live,
};
use zgx_desktop::WindowContext;

use crate::{ACCENT, App, BACKGROUND, INK, LINE, MUTED, PANEL, control};

const GRAPH_WIDTH: f32 = 412.;
const NODE_WIDTH: f32 = 106.;
const NODE_HEIGHT: f32 = 30.;
const HOP: f32 = 132.;
const ROW: f32 = 107.;
const FIRST_X: f32 = 74.;
const FIRST_Y: f32 = 48.;
const AMBER: u32 = 0xe6b26b;
const BLUE: u32 = 0x87beff;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agent {
    pub takes_skills: bool,
    pub own_skills: Option<String>,
    pub own_model: String,
    pub own_effort: String,
    pub effective_skills: String,
    pub effective_agent: String,
    pub checks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Criterion {
    pub label: String,
    pub outcome: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Data {
    pub node: Option<String>,
    pub title: String,
    pub phase: String,
    pub editable: bool,
    pub review_pr_changes: bool,
    pub delete_sandbox: Option<bool>,
    pub gates: BTreeMap<&'static str, Vec<Criterion>>,
    pub agents: BTreeMap<&'static str, Agent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    Phase(&'static str),
    Gate(&'static str),
    Action(&'static str, WorkAction),
    Return(usize),
    SupportingAgents,
}

#[derive(Clone)]
pub struct Panel {
    pub data: Signal<Data>,
    pub notice: Signal<String>,
    pub busy: Signal<bool>,
    selection: Signal<Selection>,
    diagram_offset: Signal<f32>,
    details_offset: Signal<f32>,
    editing: Signal<Option<&'static str>>,
    skills: Signal<String>,
    model: Signal<String>,
    effort: Signal<String>,
}

impl Panel {
    pub fn new(cx: &WindowContext) -> Self {
        Self {
            data: cx.ui.signal(Data::default()),
            notice: cx.ui.signal(String::new()),
            busy: cx.ui.signal(false),
            selection: cx.ui.signal(Selection::Phase(flow::PHASES[0].id)),
            diagram_offset: cx.ui.signal(0.),
            details_offset: cx.ui.signal(0.),
            editing: cx.ui.signal(None),
            skills: cx.ui.signal(String::new()),
            model: cx.ui.signal(String::new()),
            effort: cx.ui.signal(String::new()),
        }
    }

    pub fn open(&self, data: Data) {
        let phase = flow::find_phase(&data.phase).unwrap_or(&flow::PHASES[0]);
        self.data.set(data);
        self.select(Selection::Phase(phase.id));
        self.diagram_offset
            .set((position(phase.id).1 - 140.).max(0.));
    }

    fn select(&self, selection: Selection) {
        self.selection.set(selection);
        self.details_offset.set(0.);
        self.editing.set(None);
        self.notice.set(String::new());
    }

    #[cfg(feature = "live")]
    pub fn saved(&self) {
        self.editing.set(None);
        self.notice.set("Saved lifecycle settings.".into());
    }

    pub fn view(&self, app: &App) -> View {
        let graph_state = self.data.clone();
        let graph_panel = self.clone();
        let detail_state = self.clone();
        let detail_panel = self.clone();
        let title = self.data.clone();
        let notice = self.notice.clone();
        let app = app.clone();
        let run_panel = app.clone();
        let support = self.clone();
        column()
            .id("lifecycle-workspace")
            .w_full()
            .h_full()
            .gap(10.)
            .child(
                row()
                    .items_center()
                    .gap(12.)
                    .child(
                        column()
                            .grow()
                            .gap(3.)
                            .child(text("Default lifecycle").text_size(20.))
                            .child(
                                text_signal(move || title.with(|data| data.title.clone()))
                                    .text_size(12.)
                                    .text_color(rgb(MUTED)),
                            ),
                    )
                    .child(
                        control("Task workflow run")
                            .id("show-workflow-run")
                            .on_click(move || run_panel.show_workflow_run()),
                    )
                    .child(
                        control("Supporting agents")
                            .id("lifecycle-support")
                            .on_click(move || {
                                if !support.busy.get() {
                                    support.select(Selection::SupportingAgents);
                                }
                            }),
                    ),
            )
            .child(
                row()
                    .grow()
                    .min_h(0.)
                    .gap(12.)
                    .child(
                        scroll(self.diagram_offset.clone())
                            .id("lifecycle-diagram-scroll")
                            .w(GRAPH_WIDTH + 18.)
                            .h_full()
                            .rounded(8.)
                            .bg(rgb(PANEL))
                            .border(1.)
                            .border_color(rgb(LINE))
                            .child(switch(
                                move || {
                                    graph_state
                                        .with(|data| (data.phase.clone(), data.review_pr_changes))
                                },
                                move |(phase, pr_review), _| graph_panel.diagram(&phase, pr_review),
                            )),
                    )
                    .child(
                        column()
                            .id("lifecycle-inspector")
                            .grow()
                            .min_w(0.)
                            .h_full()
                            .rounded(8.)
                            .bg(rgb(PANEL))
                            .border(1.)
                            .border_color(rgb(LINE))
                            .child(text("Details").text_size(12.).text_color(rgb(MUTED)).p(12.))
                            .child(div().w_full().h(1.).bg(rgb(LINE)))
                            .child(
                                scroll(self.details_offset.clone())
                                    .id("lifecycle-details-scroll")
                                    .grow()
                                    .min_h(0.)
                                    .w_full()
                                    .child(switch(
                                        move || {
                                            (
                                                detail_state.selection.get(),
                                                detail_state.data.get(),
                                                detail_state.editing.get(),
                                            )
                                        },
                                        move |(selection, data, editing), _| {
                                            detail_panel.inspector(selection, data, editing, &app)
                                        },
                                    )),
                            )
                            .child(switch_or_none(
                                move || notice.get(),
                                |status, _| {
                                    (!status.is_empty()).then(|| {
                                        text(status).p(12.).text_wrap(true).live(Live::Polite)
                                    })
                                },
                            )),
                    ),
            )
    }

    fn diagram(&self, current: &str, review_pr_changes: bool) -> View {
        let mut edges = Vec::new();
        let mut nodes = Vec::new();
        for phase in &flow::PHASES {
            let id = phase.id;
            let start = position(id);
            let title = name(id);
            let label = if current == id {
                format!("{title}, current phase")
            } else {
                title.clone()
            };
            nodes.push(self.node(
                Selection::Phase(id),
                format!("lifecycle-phase-{id}"),
                title,
                label,
                start,
                (NODE_WIDTH, NODE_HEIGHT),
                current == id,
            ));
            let Some(next) = flow::next_phase(id) else {
                continue;
            };
            let end = position(next.id);
            let horizontal = start.1 == end.1;
            let cycle = phase.cycle.or_else(|| {
                (review_pr_changes && tod_core::pr_draft_review::reviews_on_pr(id))
                    .then_some(flow::REVIEW_CYCLE)
            });
            let mut gate = middle(start, end);
            if !horizontal && cycle.is_some() {
                // Leave room for the in-phase cycle under the phase box.
                gate.0 += 61.;
                let direction = (end.1 - start.1).signum();
                edges.push((
                    vec![
                        (start.0 + NODE_WIDTH / 2., start.1),
                        (gate.0, start.1),
                        (gate.0, gate.1 - 9. * direction),
                    ],
                    MUTED,
                ));
                edges.push((
                    vec![
                        (gate.0, gate.1 + 9. * direction),
                        (gate.0, end.1),
                        (end.0 + NODE_WIDTH / 2., end.1),
                    ],
                    MUTED,
                ));
            } else {
                let entry = boundary(end, start, NODE_WIDTH / 2., NODE_HEIGHT / 2.);
                edges.push((vec![boundary(gate, end, 9., 9.), entry], MUTED));
                if phase.cycle.is_none() {
                    let exit = boundary(start, end, NODE_WIDTH / 2., NODE_HEIGHT / 2.);
                    edges.push((vec![exit, boundary(gate, start, 9., 9.)], MUTED));
                }
            }

            nodes.push(self.node(
                Selection::Gate(id),
                format!("lifecycle-gate-{id}"),
                "◇".into(),
                phase.gate.into(),
                gate,
                (22., 24.),
                false,
            ));
            if let Some(cycle) = cycle {
                self.cycle(id, start, cycle, &mut edges, &mut nodes);
                if phase.cycle.is_some() && horizontal {
                    let direction = (gate.0 - start.0).signum();
                    let from = (start.0 + direction * 43., start.1 + 34.);
                    let lane = gate.0 - direction * 7.;
                    edges.push((vec![from, (lane, from.1), (lane, gate.1)], MUTED));
                }
            }
        }
        for (index, route) in flow::RETURN_PATHS.iter().enumerate() {
            let (from, to) = (position(route.from), position(route.to));
            let (points, button) = if from.1 == to.1 {
                let y = from.1 - 30.;
                (
                    vec![
                        (from.0, from.1 - 15.),
                        (from.0, y),
                        (to.0, y),
                        (to.0, to.1 - 15.),
                    ],
                    (middle(from, to).0, y),
                )
            } else {
                let rail = 12.;
                let y = FIRST_Y - 27.;
                (
                    vec![
                        (from.0 - 53., from.1),
                        (rail, from.1),
                        (rail, y),
                        (to.0, y),
                        (to.0, to.1 - 15.),
                    ],
                    (rail, (from.1 + y) / 2.),
                )
            };
            edges.push((points, AMBER));
            nodes.push(self.node(
                Selection::Return(index),
                format!("lifecycle-return-{index}"),
                "↶".into(),
                format!("{} → {}", route.when, name(route.to)),
                button,
                (22., 24.),
                false,
            ));
        }
        let last = position(flow::PHASES.last().expect("Default has phases").id);
        overlay()
            .w(GRAPH_WIDTH)
            .h(last.1 + 29.)
            .child(
                canvas(move |_| {
                    let mut drawing = Canvas::new();
                    for (points, color) in &edges {
                        arrow(&mut drawing, points, *color);
                    }
                    drawing
                })
                .w_full()
                .h_full(),
            )
            .children_vec(nodes)
    }

    fn cycle(
        &self,
        id: &'static str,
        phase: (f32, f32),
        cycle: WorkCycle,
        edges: &mut Vec<(Vec<(f32, f32)>, u32)>,
        nodes: &mut Vec<View>,
    ) {
        let (x, y) = phase;
        let check = y + 34.;
        let fix = y + 72.;
        edges.push((vec![(x, y + 15.), (x, check - 12.)], MUTED));
        edges.push((vec![(x, check + 12.), (x, fix - 12.)], AMBER));
        edges.push((
            vec![
                (x - 43., fix),
                (x - 56., fix),
                (x - 56., check),
                (x - 43., check),
            ],
            AMBER,
        ));
        for (action, at, key, title) in [
            (cycle.check, check, "check", "Run review"),
            (cycle.rework, fix, "rework", "Fix findings"),
        ] {
            nodes.push(self.node(
                Selection::Action(id, action),
                format!("lifecycle-{key}-{id}"),
                title.into(),
                action.label().into(),
                (x, at),
                (86., 24.),
                false,
            ));
        }
    }

    fn node(
        &self,
        selection: Selection,
        id: String,
        title: String,
        label: String,
        (x, y): (f32, f32),
        (w, h): (f32, f32),
        current: bool,
    ) -> View {
        let active = self.selection.clone();
        let panel = self.clone();
        let edge = matches!(selection, Selection::Return(_));
        let gate = matches!(selection, Selection::Gate(_));
        button()
            .id(id)
            .accessible_label(label)
            .absolute()
            .left(x - w / 2.)
            .top(y - h / 2.)
            .w(w)
            .h(h)
            .items_center()
            .justify_center()
            .rounded(if gate || edge { 12. } else { 6. })
            .bg(rgb(if gate { PANEL } else { BACKGROUND }))
            .border(1.)
            .border_color(rgb(LINE))
            .reactive_style(move || {
                let selected = active.get() == selection;
                Styles::new()
                    .bg(rgb(if selected {
                        ACCENT
                    } else if gate {
                        PANEL
                    } else {
                        BACKGROUND
                    }))
                    .border_color(rgb(if selected || current {
                        BLUE
                    } else if edge {
                        AMBER
                    } else {
                        LINE
                    }))
            })
            .child(
                text(title)
                    .text_size(if gate { 20. } else { 12. })
                    .text_color(rgb(if edge { AMBER } else { INK })),
            )
            .on_click(move || {
                if !panel.busy.get() {
                    panel.select(selection);
                }
            })
    }

    fn inspector(
        &self,
        selected: Selection,
        data: Data,
        editing: Option<&'static str>,
        app: &App,
    ) -> View {
        let (title, description, keys) = match selected {
            Selection::Phase(id) => {
                let phase = flow::find_phase(id).expect("valid phase");
                let description = if data.phase == id {
                    format!("Current phase · {}", phase.work)
                } else {
                    phase.work.into()
                };
                (name(id), description, phase.settings)
            }
            Selection::Gate(id) => {
                let phase = flow::find_phase(id).expect("valid gate phase");
                let next = flow::next_phase(id).expect("valid gate destination");
                (
                    phase.gate.into(),
                    format!("{} → {}", name(id), name(next.id)),
                    &[][..],
                )
            }
            Selection::Action(id, action) => {
                let cycle = flow::REVIEW_CYCLE;
                let condition = if action == cycle.check {
                    cycle.retry_when
                } else {
                    cycle.rework_when
                };
                let mut content = detail(action.label(), &format!("{} · {condition}", name(id)));
                content = content.child(self.agent(action.settings_key(), &data, editing, app));
                return content;
            }
            Selection::Return(index) => {
                let path = &flow::RETURN_PATHS[index];
                (
                    format!("{} → {}", name(path.from), name(path.to)),
                    path.when.into(),
                    &[][..],
                )
            }
            Selection::SupportingAgents => (
                "Supporting agents".into(),
                String::new(),
                flow::SUPPORTING_AGENTS,
            ),
        };
        let mut content = detail(&title, &description);
        for key in keys {
            content = content.child(self.agent(key, &data, editing, app));
        }
        if let Selection::Gate(id) = selected {
            if let Some(criteria) = data.gates.get(id).filter(|criteria| !criteria.is_empty()) {
                for check in criteria {
                    let mut card = section()
                        .child(text(check.label.clone()).text_wrap(true))
                        .child(
                            text(check.outcome.clone())
                                .text_size(12.)
                                .text_color(rgb(MUTED)),
                        );
                    if let Some(description) = &check.detail {
                        card = card.child(text(description.clone()).text_wrap(true));
                    }
                    content = content.child(card);
                }
            }
        }
        if selected == Selection::Phase("done")
            && let Some(delete) = data.delete_sandbox
        {
            content = content.child(
                text(if delete {
                    "Cloud sandbox is deleted on completion."
                } else {
                    "Cloud sandbox is kept on completion."
                })
                .text_wrap(true),
            );
        }
        content
    }

    fn agent(
        &self,
        key: &'static str,
        data: &Data,
        editing: Option<&'static str>,
        app: &App,
    ) -> View {
        let agent = data.agents.get(key).cloned().unwrap_or_default();
        let title = name(&key.replace('_', " "));
        let mut card = section().child(text(title).text_size(13.));
        if !agent.effective_skills.is_empty() {
            card = card.child(
                text(agent.effective_skills.clone())
                    .text_wrap(true)
                    .text_color(rgb(MUTED)),
            );
        }
        if !agent.effective_agent.is_empty() {
            card = card.child(
                text(agent.effective_agent.clone())
                    .text_wrap(true)
                    .text_color(rgb(MUTED)),
            );
        }
        for check in &agent.checks {
            card = card.child(text(check.clone()).text_wrap(true).text_color(rgb(MUTED)));
        }
        if editing == Some(key) {
            let save = app.clone();
            let form = self.clone();
            let cancel = self.clone();
            let inherit = app.clone();
            let old_skills = agent.own_skills.clone();
            if agent.takes_skills {
                card = card.child(input("Skills", &self.skills));
            }
            card.child(input("Model", &self.model))
                .child(input("Effort", &self.effort))
                .child(
                    row()
                        .gap(4.)
                        .child(control("Save").id("lifecycle-save").on_click(move || {
                            let skills = form.skills.get();
                            let skills = if skills.is_empty() && old_skills.is_none() {
                                None
                            } else {
                                Some(skills)
                            };
                            save.save_lifecycle_agent(
                                key,
                                skills,
                                form.model.get(),
                                form.effort.get(),
                            );
                        }))
                        .child(control("Cancel").on_click(move || {
                            if !cancel.busy.get() {
                                cancel.editing.set(None);
                            }
                        })),
                )
                .child(control("Use inherited").on_click(move || {
                    inherit.save_lifecycle_agent(key, None, String::new(), String::new())
                }))
        } else if data.editable {
            let form = self.clone();
            card.child(
                control("Edit")
                    .id(format!("lifecycle-edit-{key}"))
                    .on_click(move || {
                        if !form.busy.get() {
                            form.skills
                                .set(agent.own_skills.clone().unwrap_or_default());
                            form.model.set(agent.own_model.clone());
                            form.effort.set(agent.own_effort.clone());
                            form.editing.set(Some(key));
                        }
                    }),
            )
        } else {
            card
        }
    }
}

fn section() -> View {
    column()
        .w_full()
        .gap(7.)
        .p(10.)
        .rounded(6.)
        .bg(rgb(BACKGROUND))
        .border(1.)
        .border_color(rgb(LINE))
}

fn detail(title: &str, description: &str) -> View {
    let body = column()
        .w_full()
        .p(12.)
        .gap(10.)
        .child(text(title.to_owned()).text_size(18.).text_wrap(true));
    if description.is_empty() {
        body
    } else {
        body.child(
            text(description.to_owned())
                .text_wrap(true)
                .text_color(rgb(MUTED)),
        )
    }
}

fn input(label: &str, value: &Signal<String>) -> View {
    column()
        .gap(4.)
        .child(text(label.to_owned()).text_size(12.).text_color(rgb(MUTED)))
        .child(
            text_input(label.to_owned(), value.clone())
                .w_full()
                .h(32.)
                .px(6.)
                .rounded(4.)
                .border(1.)
                .border_color(rgb(LINE)),
        )
}

fn name(id: &str) -> String {
    match id {
        "active" => "Implement".into(),
        "verifying" => "Verify".into(),
        "pr" => "Open PR".into(),
        "pr_draft" => "PR draft".into(),
        "pr_review" => "PR review".into(),
        _ => {
            let mut chars = id.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

fn position(id: &str) -> (f32, f32) {
    let index = flow::PHASES
        .iter()
        .position(|phase| phase.id == id)
        .expect("known phase");
    let row = index / 3;
    let col = if row % 2 == 0 {
        index % 3
    } else {
        2 - index % 3
    };
    (FIRST_X + col as f32 * HOP, FIRST_Y + row as f32 * ROW)
}

fn middle((x, y): (f32, f32), (a, b): (f32, f32)) -> (f32, f32) {
    ((x + a) / 2., (y + b) / 2.)
}

fn boundary((x, y): (f32, f32), (a, b): (f32, f32), w: f32, h: f32) -> (f32, f32) {
    (
        x + (a - x).clamp(-1., 1.) * w,
        y + (b - y).clamp(-1., 1.) * h,
    )
}

fn arrow(canvas: &mut Canvas, points: &[(f32, f32)], color: u32) {
    let mut path = Path::builder();
    let (x, y) = points[0];
    path.move_to(x, y);
    for &(x, y) in &points[1..] {
        path.line_to(x, y);
    }
    let [(a, b), (x, y)] = points[points.len() - 2..] else {
        return;
    };
    let dx = (x - a).clamp(-1., 1.);
    let dy = (y - b).clamp(-1., 1.);
    path.move_to(x - 5. * dx + 3. * dy, y - 5. * dy - 3. * dx);
    path.line_to(x, y);
    path.line_to(x - 5. * dx - 3. * dy, y - 5. * dy + 3. * dx);
    if let Ok(path) = path.build() {
        canvas.stroke(
            path,
            rgb(color),
            Stroke {
                width: 1.4,
                ..Stroke::default()
            },
        );
    }
}
