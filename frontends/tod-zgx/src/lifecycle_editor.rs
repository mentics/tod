use std::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};
use tod_core::lifecycle::workflow::{
    Comparison, Condition, Decision, Definition, FORMAT_VERSION, Field, Node, NodeKind,
    Observation, Observations, Operand, Revision, Route, Source, Trace, Value, ValueType,
    builtin::{ACTIONS, MANUAL, PROVIDERS},
    native,
};
use zgx::{
    canvas::{Canvas, Path, Stroke},
    compose::{Context, prelude::*},
    reactive::{Runtime, Signal},
    semantics::Live,
};
use zgx_desktop::WindowContext;

use crate::{ACCENT, BACKGROUND, INK, LINE, MUTED, PANEL, control};

const BLUE: u32 = 0x87beff;
const WARNING: u32 = 0xffbd91;
const BOX_WIDTH: f32 = 148.;
const BOX_HEIGHT: f32 = 54.;
const COLUMN_STEP: f32 = 190.;
const ROW_STEP: f32 = 106.;
const GRAPH_WIDTH: f32 = 404.;

#[derive(Clone, Debug)]
pub enum Event {
    Load(String),
    Refresh,
    ManageActions,
    Save {
        title: String,
        definition: Definition,
    },
    Publish {
        title: String,
        definition: Definition,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    pub id: String,
    pub title: String,
    pub revision: Option<u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Export {
    title: String,
    definition: Definition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    Node(String),
    Route { from: String, route: String },
    Fields,
    Preview,
}

#[derive(Clone)]
pub struct Editor {
    definition: Signal<Definition>,
    title: Signal<String>,
    selection: Signal<Selection>,
    library: Signal<Vec<LibraryEntry>>,
    browsing: Signal<bool>,
    transferring: Signal<bool>,
    transfer: Signal<String>,
    busy: Signal<bool>,
    starter: Signal<Option<Definition>>,
    actions: Signal<Vec<(String, Revision)>>,
    message: Signal<String>,
    diagram_offset: Signal<f32>,
    details_offset: Signal<f32>,
    examples: Signal<Observations>,
    preview: Signal<Option<Trace>>,
    undo: Signal<Vec<Definition>>,
}

type Dispatch = Rc<dyn Fn(Event)>;

impl Editor {
    pub fn new(cx: &WindowContext) -> Self {
        Self::in_runtime(&cx.ui.runtime)
    }

    fn in_runtime(runtime: &Runtime) -> Self {
        let definition = blank();
        Self {
            selection: runtime.signal(Selection::Node(definition.entry.clone())),
            definition: runtime.signal(definition),
            title: runtime.signal("Untitled workflow".into()),
            library: runtime.signal(Vec::new()),
            browsing: runtime.signal(false),
            transferring: runtime.signal(false),
            transfer: runtime.signal(String::new()),
            busy: runtime.signal(false),
            starter: runtime.signal(None),
            actions: runtime.signal(Vec::new()),
            message: runtime.signal(String::new()),
            diagram_offset: runtime.signal(0.),
            details_offset: runtime.signal(0.),
            examples: runtime.signal(BTreeMap::new()),
            preview: runtime.signal(None),
            undo: runtime.signal(Vec::new()),
        }
    }

    pub fn open(&self, title: String, definition: Definition) {
        let preserve = self.definition.with(|current| {
            current.revision.id == definition.revision.id
                || (current.revision.id.is_empty()
                    && current.nodes == definition.nodes
                    && current.fields == definition.fields)
        });
        self.title.set(title);
        if !preserve {
            self.select(Selection::Node(definition.entry.clone()));
            self.diagram_offset.set(0.);
            self.examples.set(BTreeMap::new());
            self.preview.set(None);
            self.undo.set(Vec::new());
        }
        self.definition.set(definition);
        self.browsing.set(false);
    }

    pub fn set_starter(&self, mut starter: Definition) {
        starter.revision = Revision {
            id: String::new(),
            number: 1,
        };
        self.starter.set(Some(starter));
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
    }

    pub fn set_actions(&self, actions: Vec<(String, Revision)>) {
        self.actions.set(actions);
    }

    pub fn accept_saved(&self, title: String, saved: Definition, submitted: Definition) {
        if self.definition.with(|draft| *draft == submitted) {
            // Typing a new name while the write was running is an edit even
            // when the graph itself matches the submitted draft.
            if self.title.with(|current| current.trim() == title) {
                self.title.set(title);
            }
            self.definition.set(saved);
        } else {
            self.definition.update(|draft| {
                if draft.revision.id == submitted.revision.id {
                    draft.revision = saved.revision;
                }
            });
        }
        self.preview.set(None);
    }

    pub fn update_library(&self, library: Vec<LibraryEntry>) {
        self.library.set(library);
    }

    pub fn status(&self, text: String) {
        self.message.set(text);
    }

    fn select(&self, selection: Selection) {
        self.selection.set(selection);
        self.details_offset.set(0.);
    }

    fn change(&self, edit: impl FnOnce(&mut Definition)) {
        let previous = self.definition.get();
        let mut next = previous.clone();
        edit(&mut next);
        if previous != next {
            self.undo.update(|history| history.push(previous));
            self.definition.set(next);
            self.preview.set(None);
            self.message.set(String::new());
        }
    }

    fn undo(&self) {
        let mut history = self.undo.get();
        let Some(mut previous) = history.pop() else {
            return;
        };
        // Saving can install the catalog's identity while the undo stack
        // still contains edits made on an unsaved draft.
        previous.revision = self.definition.with(|draft| draft.revision.clone());
        self.undo.set(history);
        self.select(Selection::Node(previous.entry.clone()));
        self.definition.set(previous);
        self.preview.set(None);
        self.status("Undid the last edit.".into());
    }

    fn reset(&self, title: String, definition: Definition) {
        self.diagram_offset.set(0.);
        self.examples.set(BTreeMap::new());
        self.preview.set(None);
        self.undo.set(Vec::new());
        self.select(Selection::Node(definition.entry.clone()));
        self.title.set(title);
        self.definition.set(definition);
        self.message.set(String::new());
    }

    fn add_node(&self) {
        let id = new_id("step");
        self.change(|draft| {
            if draft.nodes.is_empty() {
                draft.entry = id.clone();
            }
            draft.nodes.insert(
                id.clone(),
                Node {
                    label: "New step".into(),
                    kind: NodeKind::Decision,
                    outputs: BTreeMap::new(),
                    routes: Vec::new(),
                },
            );
        });
        // The diagram follows new nodes beyond the first viewport.
        let index = self.definition.with(|draft| {
            diagram_order(draft)
                .iter()
                .position(|key| key == &id)
                .unwrap_or_default()
        });
        self.diagram_offset.set((index / 2) as f32 * ROW_STEP);
        self.select(Selection::Node(id));
    }

    fn submit(&self, publish: bool, dispatch: &Dispatch) {
        if self.busy.get() {
            return;
        }
        let definition = self.definition.get();
        let title = self.title.get().trim().to_owned();
        if title.is_empty() {
            self.status("Enter a workflow name.".into());
            return;
        }
        if publish && let Err(errors) = validate(&definition).validate() {
            self.status(format!("{} · {}", errors[0].location, errors[0].message));
            return;
        }
        dispatch(if publish {
            Event::Publish { title, definition }
        } else {
            Event::Save { title, definition }
        });
    }

    pub fn view(&self, dispatch: impl Fn(Event) + 'static) -> View {
        let dispatch: Dispatch = Rc::new(dispatch);
        let graph = self.clone();
        let graph_data = self.definition.clone();
        let details = self.clone();
        let data = self.definition.clone();
        let selected = self.selection.clone();
        let stage = self.preview.clone();
        let actions = self.actions.clone();
        let notice = self.message.clone();
        column()
            .id("workflow-editor")
            .w_full()
            .h_full()
            .gap(10.)
            .child(self.toolbar(&dispatch))
            .child(
                row()
                    .grow()
                    .min_h(0.)
                    .gap(12.)
                    .child(
                        scroll(self.diagram_offset.clone())
                            .id("workflow-diagram")
                            .w(GRAPH_WIDTH + 16.)
                            .h_full()
                            .rounded(8.)
                            .bg(rgb(PANEL))
                            .border(1.)
                            .border_color(rgb(LINE))
                            .child(switch(
                                move || graph_data.get(),
                                move |definition, _| graph.diagram(&definition),
                            )),
                    )
                    .child(
                        column()
                            .id("workflow-inspector")
                            .grow()
                            .min_w(0.)
                            .h_full()
                            .rounded(8.)
                            .bg(rgb(PANEL))
                            .border(1.)
                            .border_color(rgb(LINE))
                            .child(
                                scroll(self.details_offset.clone())
                                    .w_full()
                                    .grow()
                                    .min_h(0.)
                                    .child(switch(
                                        move || {
                                            (data.get(), selected.get(), stage.get(), actions.get())
                                        },
                                        move |(definition, selection, preview, actions), cx| {
                                            details.inspector(
                                                &definition,
                                                selection,
                                                preview,
                                                &actions,
                                                cx,
                                            )
                                        },
                                    )),
                            )
                            .child(
                                text_signal(move || notice.get())
                                    .text_color(rgb(WARNING))
                                    .text_wrap(true)
                                    .live(Live::Polite)
                                    .px(12.),
                            ),
                    ),
            )
            .child(self.library_view(&dispatch))
            .child(self.transfer_view())
    }

    fn toolbar(&self, dispatch: &Dispatch) -> View {
        let browse = self.browsing.clone();
        let refresh = dispatch.clone();
        let add = self.clone();
        let undo = self.clone();
        let history = self.undo.clone();
        let editor = self.clone();
        let save = dispatch.clone();
        let saving = self.busy.clone();
        let publisher = self.clone();
        let publish = dispatch.clone();
        let publishing = self.busy.clone();
        let actions = dispatch.clone();
        row()
            .w_full()
            .gap(6.)
            .items_center()
            .child(
                control("Workflows")
                    .id("workflow-library")
                    .on_click(move || {
                        browse.set(true);
                        refresh(Event::Refresh);
                    }),
            )
            .child(
                text_input("Workflow name", self.title.clone())
                    .id("workflow-name")
                    .grow()
                    .min_w(80.)
                    .h(34.)
                    .px(9.)
                    .rounded(5.)
                    .bg(rgb(PANEL)),
            )
            .child(
                control("Add node")
                    .id("workflow-add-node")
                    .on_click(move || add.add_node()),
            )
            .child(
                control("Undo")
                    .id("workflow-undo")
                    .disabled_when(move || history.with(Vec::is_empty))
                    .on_click(move || undo.undo()),
            )
            .child(self.tab("Data sources", Selection::Fields))
            .child(
                control("Agent actions")
                    .id("workflow-agent-actions")
                    .on_click(move || actions(Event::ManageActions)),
            )
            .child(self.tab("Preview", Selection::Preview))
            .child(
                control("Save draft")
                    .id("workflow-save")
                    .disabled_when(move || saving.get())
                    .on_click(move || editor.submit(false, &save)),
            )
            .child(
                control("Publish")
                    .id("workflow-publish")
                    .disabled_when(move || publishing.get())
                    .on_click(move || publisher.submit(true, &publish)),
            )
    }

    fn tab(&self, label: &str, selection: Selection) -> View {
        let editor = self.clone();
        control(label)
            .id(format!(
                "workflow-tab-{}",
                label.to_lowercase().replace(' ', "-")
            ))
            .on_click(move || editor.select(selection.clone()))
    }

    fn library_view(&self, dispatch: &Dispatch) -> View {
        let editor = self.clone();
        let starter = self.starter.clone();
        let from_template = self.clone();
        let from_default = self.clone();
        let from_automated = self.clone();
        let automated_busy = self.busy.clone();
        let entries = self.library.clone();
        let select = dispatch.clone();
        let loading = self.busy.clone();
        let library_message = self.message.clone();
        let busy = self.busy.clone();
        let duplicate = self.clone();
        let transfer = self.clone();
        modal("Workflows", self.browsing.clone())
            .w(560.)
            .p(15.)
            .gap(9.)
            .rounded(10.)
            .bg(rgb(BACKGROUND))
            .child(text("Workflows").text_size(22.))
            .child(
                row()
                    .gap(6.)
                    .child(
                        control("New workflow")
                            .id("workflow-new")
                            .on_click(move || {
                                if !editor.busy.get() {
                                    editor.reset("Untitled workflow".into(), blank());
                                    editor.browsing.set(false);
                                }
                            }),
                    )
                    .child(switch_or_none(
                        move || starter.get(),
                        move |template, _| {
                            template.map(|template| {
                                let editor = from_template.clone();
                                control("Start from review and fix")
                                    .id("workflow-new-template")
                                    .on_click(move || {
                                        if !editor.busy.get() {
                                            editor.reset("Review and fix".into(), template.clone());
                                            editor.browsing.set(false);
                                        }
                                    })
                            })
                        },
                    )),
            )
            .child(
                row()
                    .gap(6.)
                    .child(
                        control("Start from Default phases")
                            .id("workflow-new-default")
                            .on_click(move || {
                                if !from_default.busy.get() {
                                    from_default.reset(
                                        "Default phases (operator-driven)".into(),
                                        tod_core::lifecycle::workflow::builtin::default_template(),
                                    );
                                    from_default.browsing.set(false);
                                }
                            }),
                    )
                    .child(
                        control("Try automated Default")
                            .id("workflow-new-automated-default")
                            .disabled_when(move || automated_busy.get())
                            .on_click(move || {
                                if !from_automated.busy.get() {
                                    from_automated.reset(
                                        "Automated Default".into(),
                                        tod_core::lifecycle::workflow::builtin::automated_default(),
                                    );
                                    from_automated.browsing.set(false);
                                }
                            }),
                    ),
            )
            .child(
                row()
                    .gap(6.)
                    .child(control("Duplicate current").on_click(move || {
                        if duplicate.busy.get() {
                            return;
                        }
                        let mut definition = duplicate.definition.get();
                        definition.revision = Revision {
                            id: String::new(),
                            number: 1,
                        };
                        duplicate.reset(format!("{} copy", duplicate.title.get()), definition);
                        duplicate.browsing.set(false);
                    }))
                    .child(
                        text_signal(move || {
                            if loading.get() {
                                "Loading workflows…".into()
                            } else {
                                library_message.get()
                            }
                        })
                        .text_size(12.)
                        .text_color(rgb(MUTED))
                        .text_wrap(true)
                        .live(Live::Polite),
                    )
                    .child(control("Import or export").on_click(move || {
                        if transfer.busy.get() {
                            return;
                        }
                        let exported = Export {
                            title: transfer.title.get(),
                            definition: transfer.definition.get(),
                        };
                        if let Ok(json) = serde_json::to_string_pretty(&exported) {
                            transfer.transfer.set(json);
                            transfer.browsing.set(false);
                            transfer.transferring.set(true);
                        }
                    })),
            )
            .child(switch(
                move || entries.get(),
                move |entries, _| {
                    if entries.is_empty() {
                        return hint("No saved workflows").into();
                    }
                    column().gap(6.).children_vec(
                        entries
                            .into_iter()
                            .map(|entry| {
                                let detail = entry.revision.map_or("Draft".into(), |version| {
                                    format!("Published revision {version}")
                                });
                                let select = select.clone();
                                let busy = busy.clone();
                                let disabled = busy.clone();
                                let id = entry.id;
                                control(format!("{} · {detail}", entry.title))
                                    .w_full()
                                    .id(format!("workflow-load-{id}"))
                                    .disabled_when(move || disabled.get())
                                    .on_click(move || {
                                        if !busy.get() {
                                            select(Event::Load(id.clone()));
                                        }
                                    })
                            })
                            .collect(),
                    )
                },
            ))
    }

    fn transfer_view(&self) -> View {
        let editor = self.clone();
        let close = self.transferring.clone();
        modal("Workflow JSON", self.transferring.clone())
            .w(620.)
            .p(15.)
            .gap(9.)
            .rounded(10.)
            .bg(rgb(BACKGROUND))
            .child(heading("Workflow JSON"))
            .child(hint(
                "Copy the current workflow or paste a workflow to open a new draft.",
            ))
            .child(
                text_area("Workflow JSON", self.transfer.clone())
                    .w_full()
                    .h(260.)
                    .bg(rgb(PANEL))
                    .p(8.),
            )
            .child(
                row()
                    .gap(8.)
                    .child(control("Import as new draft").on_click(move || {
                        if editor.busy.get() {
                            return;
                        }
                        match serde_json::from_str::<Export>(&editor.transfer.get()) {
                            Ok(mut document) => {
                                document.definition.revision = Revision {
                                    id: String::new(),
                                    number: 1,
                                };
                                editor.reset(document.title, document.definition);
                                editor.transferring.set(false);
                            }
                            Err(error) => editor.status(format!("Invalid workflow JSON. {error}")),
                        }
                    }))
                    .child(control("Close").on_click(move || {
                        close.set(false);
                    })),
            )
            .child(
                text_signal({
                    let message = self.message.clone();
                    move || message.get()
                })
                .text_color(rgb(WARNING))
                .text_wrap(true)
                .live(Live::Polite),
            )
    }

    fn diagram(&self, draft: &Definition) -> View {
        diagram(draft, |part| {
            let active = self.selection.clone();
            let selected = part.selection.clone();
            let editor = self.clone();
            let selection = part.selection.clone();
            graph_item(button(), part, "workflow")
                .reactive_style(move || graph_colors(active.get() == selected))
                .on_click(move || editor.select(selection.clone()))
        })
    }

    fn inspector(
        &self,
        draft: &Definition,
        selection: Selection,
        preview: Option<Trace>,
        actions: &[(String, Revision)],
        cx: &mut Context,
    ) -> View {
        let base = column().w_full().p(12.).gap(10.);
        match selection {
            Selection::Node(id) => base.child(self.node_form(draft, &id, actions, cx)),
            Selection::Route { from, route } => {
                base.child(self.route_form(draft, &from, &route, cx))
            }
            Selection::Fields => base.child(self.fields_form(draft, cx)),
            Selection::Preview => base.child(self.preview_form(draft, preview, cx)),
        }
    }

    fn node_form(
        &self,
        draft: &Definition,
        id: &str,
        actions: &[(String, Revision)],
        cx: &mut Context,
    ) -> View {
        let Some(node) = draft.nodes.get(id) else {
            return hint("Select a node to configure it.");
        };
        let editor = self.clone();
        let node_id = id.to_owned();
        let name = cx.state(node.label.clone());
        let new_name = name.clone();
        let mut view = column()
            .w_full()
            .gap(9.)
            .child(heading("Node"))
            .child(input("Node name", name).id("workflow-node-name"))
            .child(hint(format!("Node ID  {id}")).selectable(true))
            .child(control("Rename node").on_click(move || {
                let name = new_name.get();
                if !name.trim().is_empty() {
                    editor.change(|draft| {
                        if let Some(node) = draft.nodes.get_mut(&node_id) {
                            node.label = name.trim().into();
                        }
                    });
                }
            }));

        let editor = self.clone();
        let node_id = id.to_owned();
        let phase_editor = self.clone();
        let phase_node = id.to_owned();
        let phase = draft.task_phases.get(id).cloned().unwrap_or_default();
        let phases = std::iter::once(("No task phase".to_owned(), String::new()))
            .chain(
                tod_core::lifecycle::flow::PHASES
                    .iter()
                    .map(|phase| (phase.id.replace('_', " "), phase.id.to_owned())),
            )
            .collect();
        view = view.child(picker(cx, "Task phase", &phase, phases, move |selected| {
            phase_editor.change(|draft| {
                draft
                    .adoption_points
                    .retain(|phase, node| node != &phase_node || phase == &selected);
                if selected.is_empty() {
                    draft.task_phases.remove(&phase_node);
                } else {
                    draft.task_phases.insert(phase_node.clone(), selected);
                }
            });
        }));
        if !phase.is_empty() && phase != "done" {
            let is_adoption_point = draft
                .adoption_points
                .get(&phase)
                .is_some_and(|node| node == id);
            let adoption_editor = self.clone();
            let target = id.to_owned();
            view = view.child(
                control(if is_adoption_point {
                    "Remove adoption point"
                } else {
                    "Make this the adoption point"
                })
                .id("workflow-adoption-point")
                .on_click(move || {
                    adoption_editor.change(|draft| {
                        if is_adoption_point {
                            draft.adoption_points.remove(&phase);
                        } else {
                            draft.adoption_points.insert(phase.clone(), target.clone());
                        }
                    });
                }),
            );
        }
        let mut kinds = vec![
            ("Decision".to_owned(), NodeKind::Decision),
            ("Finish".to_owned(), NodeKind::Finish),
        ];
        kinds.extend(ACTIONS.iter().chain(native::ACTIONS).map(|&(key, label)| {
            (
                label.into(),
                NodeKind::Action {
                    action: Revision {
                        id: key.into(),
                        number: 1,
                    },
                },
            )
        }));
        kinds.extend(actions.iter().map(|(label, revision)| {
            (
                format!("{label} · Revision {}", revision.number),
                NodeKind::Action {
                    action: revision.clone(),
                },
            )
        }));
        if !kinds.iter().any(|(_, kind)| kind == &node.kind) {
            kinds.push((kind_label(&node.kind), node.kind.clone()));
        }
        view = view.child(heading("Action")).child(picker(
            cx,
            "Node action",
            &node.kind,
            kinds,
            move |kind| {
                editor.change(|draft| {
                    if let Some(node) = draft.nodes.get_mut(&node_id) {
                        if let NodeKind::Action { action } = &kind {
                            for &(name, ty) in native::required_outputs(&action.id) {
                                node.outputs.entry(name.into()).or_insert(ty);
                            }
                        }
                        node.kind = kind;
                    }
                });
            },
        ));
        if id != draft.entry {
            let editor = self.clone();
            let node = id.to_owned();
            view = view.child(
                control("Make entry node")
                    .on_click(move || editor.change(|draft| draft.entry.clone_from(&node))),
            );
        }

        view = view.child(heading("Outputs"));
        for (name, value_type) in &node.outputs {
            let editor = self.clone();
            let node = id.to_owned();
            let key = name.clone();
            view = view.child(
                row()
                    .items_center()
                    .gap(6.)
                    .child(hint(format!("{name} · {}", type_label(*value_type))).grow())
                    .child(control("Remove").on_click(move || {
                        editor.change(|draft| {
                            if let Some(node) = draft.nodes.get_mut(&node) {
                                node.outputs.remove(&key);
                            }
                        });
                    })),
            );
        }
        let output = cx.state(String::new());
        let ty = cx.state(ValueType::Text);
        let output_key = output.clone();
        let new_ty = ty.clone();
        let editor = self.clone();
        let node_id = id.to_owned();
        view = view
            .child(input("Output field name", output))
            .child(type_picker(ty))
            .child(control("Add output").on_click(move || {
                let key = output_key.get().trim().to_owned();
                if !key.is_empty() {
                    editor.change(|draft| {
                        if let Some(node) = draft.nodes.get_mut(&node_id) {
                            node.outputs.insert(key, new_ty.get());
                        }
                    });
                }
            }));

        view = view.child(heading("Outgoing routes"));
        for route in &node.routes {
            let selected = Selection::Route {
                from: id.to_owned(),
                route: route.id.clone(),
            };
            let editor = self.clone();
            let destination = draft
                .nodes
                .get(&route.to)
                .map_or(route.to.as_str(), |node| &node.label);
            let condition = if route.when.is_some() {
                "Condition"
            } else {
                "Otherwise"
            };
            view = view.child(
                control(format!("{destination} · {condition}"))
                    .on_click(move || editor.select(selected.clone())),
            );
        }
        if node.kind != NodeKind::Finish {
            let editor = self.clone();
            let node_id = id.to_owned();
            let choices = draft
                .nodes
                .iter()
                .map(|(id, node)| (node.label.clone(), id.clone()))
                .collect();
            view = view.child(picker(
                cx,
                "Connect to…",
                &String::new(),
                choices,
                move |to| {
                    let route_id = new_id("route");
                    editor.change(|draft| {
                        if let Some(node) = draft.nodes.get_mut(&node_id) {
                            let when = node
                                .routes
                                .iter()
                                .any(|route| route.when.is_none())
                                .then(empty_condition);
                            node.routes.push(Route {
                                id: route_id.clone(),
                                to,
                                when,
                            });
                        }
                    });
                    editor.select(Selection::Route {
                        from: node_id.clone(),
                        route: route_id,
                    });
                },
            ));
        }

        let editor = self.clone();
        let target = id.to_owned();
        let attached = draft
            .nodes
            .values()
            .flat_map(|node| &node.routes)
            .filter(|route| route.to == id)
            .count()
            + node.routes.iter().filter(|route| route.to != id).count();
        let bindings = drop_count(&draft.fields, id);
        view.child(heading("Delete node"))
            .child(hint(format!("{attached} connected routes will be removed. {bindings} data bindings will need repair.")))
            .child(control("Delete node").id("workflow-node-delete").on_click(move || editor.delete_node(&target)))
    }

    fn delete_node(&self, id: &str) {
        self.change(|draft| {
            draft.nodes.remove(id);
            draft.task_phases.remove(id);
            draft.adoption_points.retain(|_, node| node != id);
            for node in draft.nodes.values_mut() {
                node.routes.retain(|route| route.to != id);
            }
            if draft.entry == id {
                draft.entry = draft.nodes.keys().next().cloned().unwrap_or_default();
            }
        });
        self.select(Selection::Node(
            self.definition.with(|draft| draft.entry.clone()),
        ));
    }

    fn route_form(&self, draft: &Definition, from: &str, route_id: &str, cx: &mut Context) -> View {
        let Some(node) = draft.nodes.get(from) else {
            return hint("Select an outgoing route.");
        };
        let Some(route) = node.routes.iter().find(|route| route.id == route_id) else {
            return hint("Select an outgoing route.");
        };
        let editor = self.clone();
        let from_id = from.to_owned();
        let edit = route_id.to_owned();
        let back = self.clone();
        let back_id = from_id.clone();
        let options = draft
            .nodes
            .iter()
            .map(|(id, node)| (node.label.clone(), id.clone()))
            .collect();
        let mut view = column()
            .w_full()
            .gap(9.)
            .child(heading("Route"))
            .child(
                control(format!("From {}", node.label))
                    .on_click(move || back.select(Selection::Node(back_id.clone()))),
            )
            .child(picker(cx, "Destination", &route.to, options, move |to| {
                editor.change(|draft| {
                    if let Some(route) = route_mut(draft, &from_id, &edit) {
                        route.to = to;
                    }
                });
            }))
            .child(heading("Condition"));
        let editor = self.clone();
        let from_id = from.to_owned();
        let edit = route_id.to_owned();
        if let Some(condition) = &route.when {
            view = view
                .child(self.condition_view(draft, condition, from, route_id, Vec::new(), cx))
                .child(control("Use otherwise").on_click(move || {
                    editor.change(|draft| {
                        if let Some(route) = route_mut(draft, &from_id, &edit) {
                            route.when = None;
                        }
                    });
                }));
        } else {
            view = view
                .child(hint(
                    "Otherwise, when every conditional route from this node is false.",
                ))
                .child(
                    control("Add condition")
                        .id("workflow-add-condition")
                        .on_click(move || {
                            editor.change(|draft| {
                                let condition = first_comparison(draft);
                                if let Some(route) = route_mut(draft, &from_id, &edit) {
                                    route.when = Some(condition);
                                }
                            });
                        }),
                );
        }
        let editor = self.clone();
        let from_id = from.to_owned();
        let edit = route_id.to_owned();
        let insert = self.clone();
        let origin = from_id.clone();
        let route = edit.clone();
        view.child(control("Insert node").on_click(move || insert.insert_node(&origin, &route)))
            .child(
                control("Delete route")
                    .id("workflow-route-delete")
                    .on_click(move || {
                        editor.change(|draft| {
                            if let Some(node) = draft.nodes.get_mut(&from_id) {
                                node.routes.retain(|route| route.id != edit);
                            }
                        });
                        editor.select(Selection::Node(from_id.clone()));
                    }),
            )
    }

    fn insert_node(&self, from: &str, route_id: &str) {
        let id = new_id("step");
        self.change(|draft| {
            let Some(route) = route_mut(draft, from, route_id) else {
                return;
            };
            let destination = std::mem::replace(&mut route.to, id.clone());
            draft.nodes.insert(
                id.clone(),
                Node {
                    label: "New step".into(),
                    kind: NodeKind::Decision,
                    outputs: BTreeMap::new(),
                    routes: vec![Route {
                        id: new_id("route"),
                        to: destination,
                        when: None,
                    }],
                },
            );
        });
        self.select(Selection::Node(id));
    }

    fn condition_view(
        &self,
        draft: &Definition,
        condition: &Condition,
        from: &str,
        route_id: &str,
        path: Vec<usize>,
        cx: &mut Context,
    ) -> View {
        let edit = GateEdit {
            editor: self.clone(),
            from: from.into(),
            route: route_id.into(),
            path,
        };
        let kind = ExprKind::of(condition);
        let replace = edit.clone();
        let choices = vec![
            ("Compare values".into(), ExprKind::Compare),
            ("All".into(), ExprKind::All),
            ("Any".into(), ExprKind::Any),
            ("Not".into(), ExprKind::Not),
        ];
        let mut view = column()
            .w_full()
            .gap(6.)
            .p(7.)
            .rounded(6.)
            .border(1.)
            .border_color(rgb(LINE))
            .child(picker(cx, "Expression", &kind, choices, move |kind| {
                replace.editor.change(|draft| {
                    let starter = first_comparison(draft);
                    if let Some(when) = replace.target(draft) {
                        if ExprKind::of(when) == kind {
                            return;
                        }
                        *when = match kind {
                            ExprKind::Compare => starter,
                            ExprKind::All => Condition::All(vec![when.clone()]),
                            ExprKind::Any => Condition::Any(vec![when.clone()]),
                            ExprKind::Not => Condition::Not(Box::new(when.clone())),
                        };
                    }
                });
            }));
        match condition {
            Condition::Compare { left, op, right } => {
                let operator = edit.clone();
                let choices = operators(draft, left, right, *op);
                view = view
                    .child(operand_view(cx, draft, left, true, edit.clone()))
                    .child(picker(cx, "Comparison", op, choices, move |replacement| {
                        operator.update(|condition| {
                            if let Condition::Compare { op, .. } = condition {
                                *op = replacement;
                            }
                        });
                    }))
                    .child(operand_view(cx, draft, right, false, edit.clone()));
            }
            Condition::All(children) | Condition::Any(children) => {
                for (index, condition) in children.iter().enumerate() {
                    let mut path = edit.path.clone();
                    path.push(index);
                    view =
                        view.child(self.condition_view(draft, condition, from, route_id, path, cx));
                }
                let append = edit.clone();
                view = view.child(control("Add clause").on_click(move || {
                    append.editor.change(|draft| {
                        let clause = first_comparison(draft);
                        if let Some(Condition::All(children) | Condition::Any(children)) =
                            append.target(draft)
                        {
                            children.push(clause);
                        }
                    });
                }));
            }
            Condition::Not(child) => {
                let mut path = edit.path.clone();
                path.push(0);
                view = view.child(self.condition_view(draft, child, from, route_id, path, cx));
            }
        }
        if let Some((&index, parent)) = edit.path.split_last() {
            let mut parent_edit = edit.clone();
            parent_edit.path = parent.to_vec();
            if let Some(when) = draft
                .nodes
                .get(from)
                .and_then(|node| node.routes.iter().find(|route| route.id == route_id))
                .and_then(|route| route.when.as_ref())
                .and_then(|condition| condition_at(condition, parent))
                && matches!(when, Condition::All(_) | Condition::Any(_))
            {
                view = view.child(control("Remove clause").on_click(move || {
                    parent_edit.update(|condition| {
                        if let Condition::All(children) | Condition::Any(children) = condition
                            && index < children.len()
                        {
                            children.remove(index);
                        }
                    });
                }));
            }
        }
        view
    }

    fn fields_form(&self, draft: &Definition, cx: &mut Context) -> View {
        let mut view = column().w_full().gap(8.).child(heading("Data sources"));
        let choices = field_options(draft);
        for (id, field) in &draft.fields {
            let edit = self.clone();
            let edit_id = id.clone();
            let delete = self.clone();
            let delete_id = id.clone();
            let mut options = choices.clone();
            options.extend(manual_options(id));
            if !options.iter().any(|(_, known)| known == field) {
                options.push((source_label(draft, field), field.clone()));
            }
            view = view.child(
                column()
                    .w_full()
                    .gap(5.)
                    .p(7.)
                    .rounded(5.)
                    .bg(rgb(BACKGROUND))
                    .child(heading(id))
                    .child(picker(cx, "Binding", field, options, move |replacement| {
                        edit.change(|draft| {
                            draft.fields.insert(edit_id.clone(), replacement);
                        });
                    }))
                    .child(hint(format!(
                        "{} gates refer to this field.",
                        field_usage(draft, id)
                    )))
                    .child(control("Remove field").on_click(move || {
                        delete.change(|draft| {
                            draft.fields.remove(&delete_id);
                        });
                    })),
            );
        }
        let name = cx.state(String::new());
        let binding = cx.state(None::<Field>);
        let choose = binding.clone();
        let save = self.clone();
        let options = choices
            .into_iter()
            .chain(manual_options(""))
            .map(|(label, field)| (label, Some(field)))
            .collect();
        view.child(heading("Bind a value"))
            .child(input("Field identifier", name.clone()).id("workflow-field-name"))
            .child(picker(
                cx,
                "Choose a source…",
                &None,
                options,
                move |field| {
                    choose.set(field);
                },
            ))
            .child(
                text_signal({
                    let labels = binding.clone();
                    let draft = draft.clone();
                    move || {
                        labels
                            .get()
                            .as_ref()
                            .map_or(String::new(), |field| source_label(&draft, field))
                    }
                })
                .text_color(rgb(MUTED))
                .text_wrap(true),
            )
            .child(
                control("Add field")
                    .id("workflow-add-field")
                    .on_click(move || {
                        let id = name.get().trim().to_owned();
                        let Some(mut field) = binding.get() else {
                            save.status("Choose a source before adding a field.".into());
                            return;
                        };
                        if id.is_empty() {
                            save.status("Enter a field identifier.".into());
                        } else if save.definition.with(|draft| draft.fields.contains_key(&id)) {
                            save.status("That field identifier is already in use.".into());
                        } else {
                            if let Source::Provider { binding, field } = &mut field.source
                                && binding == MANUAL
                            {
                                field.clone_from(&id);
                            }
                            save.change(|draft| {
                                draft.fields.insert(id, field);
                            });
                        }
                    }),
            )
    }

    fn preview_form(&self, draft: &Definition, preview: Option<Trace>, cx: &mut Context) -> View {
        let mut view = column().w_full().gap(8.).child(heading("Preview"));
        if let Err(errors) = validate(draft).validate() {
            for error in errors {
                view = view.child(
                    hint(format!("{} · {}", error.location, error.message))
                        .text_color(rgb(WARNING)),
                );
            }
            return view;
        }
        let (node, visit) = preview.as_ref().map_or((draft.entry.as_str(), 1), |trace| {
            (trace.node.as_str(), trace.visit)
        });
        let editor = self.clone();
        let step = node.to_owned();
        let title = draft
            .nodes
            .get(node)
            .map_or("Choose a node", |node| node.label.as_str());
        let choose = self.clone();
        let options = draft
            .nodes
            .iter()
            .map(|(id, node)| (node.label.clone(), id.clone()))
            .collect();
        view = view
            .child(picker(cx, "Preview node", &step, options, move |id| {
                choose.evaluate(&id, 1)
            }))
            .child(hint(format!("{title} · Visit {visit}")))
            .child(
                control("Evaluate")
                    .id("workflow-preview-evaluate")
                    .on_click(move || editor.evaluate(&step, visit)),
            );
        if let Some(trace) = preview.as_ref() {
            view = view.child(heading(decision_label(draft, &trace.decision)));
            for route in &trace.routes {
                if let Some(condition) = &route.condition {
                    view = view.child(hint(format!("{} · {:?}", route.route, condition.state)));
                }
            }
            if let Decision::Take { to, .. } = &trace.decision {
                let to = to.clone();
                let editor = self.clone();
                view = view.child(
                    control("Follow route")
                        .id("workflow-preview-follow")
                        .on_click(move || {
                            editor.evaluate(&to, visit.saturating_add(1));
                        }),
                );
            }
        }
        let examples = self.examples.get();
        view = view.child(heading("Sample values"));
        for (id, field) in &draft.fields {
            let editor = self.clone();
            let clear = self.clone();
            let field_id = id.clone();
            let clear_id = id.clone();
            let step = node.to_owned();
            let clear_step = step.clone();
            let current = examples
                .get(id)
                .and_then(|observation| match observation {
                    Observation::Ready { value, .. } => Some(value_text(value)),
                    _ => None,
                })
                .unwrap_or_default();
            let value = cx.state(current);
            let typed = value.clone();
            let value_type = field.value_type;
            view = view.child(
                column()
                    .w_full()
                    .gap(5.)
                    .p(7.)
                    .rounded(5.)
                    .bg(rgb(BACKGROUND))
                    .child(input(&format!("{id} · {}", type_label(value_type)), value))
                    .child(
                        row()
                            .gap(6.)
                            .child(control("Set sample").on_click(move || {
                                let Some(value) = parse_value(value_type, &typed.get()) else {
                                    editor.status(format!(
                                        "Enter a {} for {field_id}.",
                                        type_label(value_type)
                                    ));
                                    return;
                                };
                                editor.examples.update(|observations| {
                                    observations.insert(
                                        field_id.clone(),
                                        Observation::Ready {
                                            value,
                                            evidence: "Sample value entered in ZGX".into(),
                                        },
                                    );
                                });
                                editor.evaluate(&step, visit);
                            }))
                            .child(control("Clear").on_click(move || {
                                clear.examples.update(|observations| {
                                    observations.remove(&clear_id);
                                });
                                clear.evaluate(&clear_step, visit);
                            })),
                    ),
            );
        }
        view
    }

    fn evaluate(&self, node: &str, visit: u64) {
        let definition = validate(&self.definition.get());
        match definition.validate() {
            Ok(workflow) => {
                self.preview
                    .set(workflow.decide(node, visit, &self.examples.get()));
                self.message.set(String::new());
            }
            Err(errors) => self.status(format!("{} · {}", errors[0].location, errors[0].message)),
        }
    }
}

#[derive(Clone)]
struct GateEdit {
    editor: Editor,
    from: String,
    route: String,
    path: Vec<usize>,
}

impl GateEdit {
    fn target<'a>(&self, draft: &'a mut Definition) -> Option<&'a mut Condition> {
        let mut condition = route_mut(draft, &self.from, &self.route)?.when.as_mut()?;
        for &index in &self.path {
            condition = match condition {
                Condition::All(children) | Condition::Any(children) => children.get_mut(index)?,
                Condition::Not(child) if index == 0 => child,
                _ => return None,
            };
        }
        Some(condition)
    }

    fn update(&self, operation: impl FnOnce(&mut Condition)) {
        self.editor.change(|draft| {
            if let Some(condition) = self.target(draft) {
                operation(condition);
            }
        });
    }

    fn operand(&self, left: bool, value: Operand) {
        self.update(|condition| {
            if let Condition::Compare {
                left: lhs,
                right: rhs,
                ..
            } = condition
            {
                *(if left { lhs } else { rhs }) = value;
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExprKind {
    Compare,
    All,
    Any,
    Not,
}

impl ExprKind {
    fn of(condition: &Condition) -> Self {
        match condition {
            Condition::Compare { .. } => Self::Compare,
            Condition::All(_) => Self::All,
            Condition::Any(_) => Self::Any,
            Condition::Not(_) => Self::Not,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum InputKind {
    Field(String),
    Literal(ValueType),
}

fn operand_view(
    cx: &mut Context,
    draft: &Definition,
    operand: &Operand,
    left: bool,
    edit: GateEdit,
) -> View {
    let current = match operand {
        Operand::Field(id) => InputKind::Field(id.clone()),
        Operand::Literal(value) => InputKind::Literal(value_type(value)),
    };
    let options = draft
        .fields
        .iter()
        .map(|(id, field)| {
            (
                format!("{id} · {}", type_label(field.value_type)),
                InputKind::Field(id.clone()),
            )
        })
        .chain(
            types()
                .into_iter()
                .map(|(name, ty)| (format!("{name} literal"), InputKind::Literal(ty))),
        )
        .collect();
    let select = edit.clone();
    let mut view = column().w_full().gap(5.).child(picker(
        cx,
        if left { "Left value" } else { "Right value" },
        &current,
        options,
        move |kind| {
            let value = match kind {
                InputKind::Field(id) => Operand::Field(id),
                InputKind::Literal(ty) => Operand::Literal(default_value(ty)),
            };
            select.operand(left, value);
        },
    ));
    if let Operand::Literal(value) = operand {
        let ty = value_type(value);
        let input_value = cx.state(value_text(value));
        let read = input_value.clone();
        view =
            view.child(input("Literal value", input_value))
                .child(control("Set literal").on_click(move || {
                    if let Some(value) = parse_value(ty, &read.get()) {
                        edit.operand(left, Operand::Literal(value));
                    } else {
                        edit.editor
                            .status(format!("Enter a {} literal.", type_label(ty)));
                    }
                }));
    }
    view
}

fn operators(
    draft: &Definition,
    left: &Operand,
    right: &Operand,
    current: Comparison,
) -> Vec<(String, Comparison)> {
    let ty = |operand: &Operand| match operand {
        Operand::Field(id) => draft.fields.get(id).map(|field| field.value_type),
        Operand::Literal(value) => Some(value_type(value)),
    };
    [
        ("Equals", Comparison::Equal),
        ("Does not equal", Comparison::NotEqual),
        ("Less than", Comparison::Less),
        ("At most", Comparison::LessOrEqual),
        ("Greater than", Comparison::Greater),
        ("At least", Comparison::GreaterOrEqual),
    ]
    .into_iter()
    .filter(|&(_, op)| {
        op == current
            || matches!(op, Comparison::Equal | Comparison::NotEqual)
            || (ty(left) == Some(ValueType::Integer) && ty(right) == Some(ValueType::Integer))
    })
    .map(|(label, op)| (label.into(), op))
    .collect()
}

fn field_options(draft: &Definition) -> Vec<(String, Field)> {
    let outputs = draft.nodes.iter().flat_map(|(id, node)| {
        node.outputs.iter().map(move |(field, ty)| Field {
            value_type: *ty,
            source: Source::NodeOutput {
                node: id.clone(),
                field: field.clone(),
            },
        })
    });
    let providers = PROVIDERS.iter().flat_map(|&(id, _, fields)| {
        fields.iter().map(move |&(field, ty)| Field {
            value_type: ty,
            source: Source::Provider {
                binding: id.into(),
                field: field.into(),
            },
        })
    });
    outputs
        .chain(providers)
        .map(|field| (source_label(draft, &field), field))
        .collect()
}

fn manual_options(field: &str) -> Vec<(String, Field)> {
    types()
        .into_iter()
        .map(|(label, value_type)| {
            (
                format!("Operator-supplied {label}"),
                Field {
                    value_type,
                    source: Source::Provider {
                        binding: MANUAL.into(),
                        field: field.into(),
                    },
                },
            )
        })
        .collect()
}

fn source_label(draft: &Definition, field: &Field) -> String {
    let (source, key) = match &field.source {
        Source::NodeOutput { node, field } => (
            draft
                .nodes
                .get(node)
                .map_or(node.as_str(), |node| &node.label),
            field,
        ),
        Source::Provider { binding, field } => (
            PROVIDERS
                .iter()
                .find(|(id, _, _)| *id == binding)
                .map_or(binding.as_str(), |(_, title, _)| *title),
            field,
        ),
    };
    let ty = type_label(field.value_type);
    if key.is_empty() {
        format!("{source} · {ty}")
    } else {
        format!("{source} · {key} · {ty}")
    }
}

fn route_mut<'a>(draft: &'a mut Definition, from: &str, id: &str) -> Option<&'a mut Route> {
    draft
        .nodes
        .get_mut(from)?
        .routes
        .iter_mut()
        .find(|route| route.id == id)
}

fn condition_at<'a>(mut condition: &'a Condition, path: &[usize]) -> Option<&'a Condition> {
    for &index in path {
        condition = match condition {
            Condition::All(children) | Condition::Any(children) => children.get(index)?,
            Condition::Not(child) if index == 0 => child,
            _ => return None,
        };
    }
    Some(condition)
}

fn decision_label(draft: &Definition, decision: &Decision) -> String {
    match decision {
        Decision::Take { to, .. } => format!(
            "Take route to {}",
            draft.nodes.get(to).map_or(to.as_str(), |node| &node.label)
        ),
        Decision::Complete => "Workflow complete".into(),
        Decision::OperatorMove { to, .. } => format!("Operator moved the task to {to}"),
        Decision::Blocked => "No route matches".into(),
        Decision::Waiting => "Waiting for values".into(),
        Decision::Error => "A condition could not be evaluated".into(),
        Decision::Conflict { routes } => format!("Several routes match · {}", routes.join(", ")),
    }
}

fn field_usage(draft: &Definition, field: &str) -> usize {
    fn uses(condition: &Condition, field: &str) -> bool {
        let matches = |operand: &Operand| matches!(operand, Operand::Field(id) if id == field);
        match condition {
            Condition::Compare { left, right, .. } => matches(left) || matches(right),
            Condition::All(children) | Condition::Any(children) => {
                children.iter().any(|child| uses(child, field))
            }
            Condition::Not(child) => uses(child, field),
        }
    }
    draft
        .nodes
        .values()
        .flat_map(|node| &node.routes)
        .filter(|route| {
            route
                .when
                .as_ref()
                .is_some_and(|condition| uses(condition, field))
        })
        .count()
}

fn drop_count(fields: &BTreeMap<String, Field>, node: &str) -> usize {
    fields.values().filter(|field| matches!(&field.source, Source::NodeOutput { node: owner, .. } if owner == node)).count()
}

fn first_comparison(draft: &Definition) -> Condition {
    let (left, value) = draft.fields.iter().next().map_or(
        (Operand::Field(String::new()), Value::Boolean(false)),
        |(id, field)| (Operand::Field(id.clone()), default_value(field.value_type)),
    );
    Condition::Compare {
        left,
        op: Comparison::Equal,
        right: Operand::Literal(value),
    }
}

fn empty_condition() -> Condition {
    Condition::All(Vec::new())
}

fn blank() -> Definition {
    Definition {
        task_phases: BTreeMap::new(),
        adoption_points: BTreeMap::new(),
        format: FORMAT_VERSION,
        revision: Revision {
            id: String::new(),
            number: 1,
        },
        entry: "start".into(),
        nodes: BTreeMap::from([(
            "start".into(),
            Node {
                label: "Start".into(),
                kind: NodeKind::Decision,
                outputs: BTreeMap::new(),
                routes: Vec::new(),
            },
        )]),
        fields: BTreeMap::new(),
    }
}

fn validate(definition: &Definition) -> Definition {
    let mut draft = definition.clone();
    if draft.revision.id.is_empty() {
        draft.revision.id = "draft".into();
    }
    draft
}

struct GraphPart {
    selection: Selection,
    id: String,
    title: String,
    label: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn graph_item(view: View, part: GraphPart, prefix: &str) -> View {
    view.id(format!("{prefix}-{}", part.id))
        .accessible_label(part.label)
        .absolute()
        .left(part.x)
        .top(part.y)
        .w(part.w)
        .h(part.h)
        .items_center()
        .justify_center()
        .rounded(6.)
        .border(1.)
        .px(4.)
        .child(
            text(part.title)
                .text_size(12.)
                .text_color(rgb(INK))
                .text_wrap(true),
        )
}

fn graph_colors(active: bool) -> Styles {
    Styles::new()
        .bg(rgb(if active { ACCENT } else { BACKGROUND }))
        .border_color(rgb(if active { BLUE } else { LINE }))
}

#[cfg(feature = "live")]
pub fn run_diagram(definition: &Definition, current: &str) -> View {
    diagram(definition, |mut part| {
        let active = matches!(&part.selection, Selection::Node(id) if id == current);
        if active {
            part.label.push_str(", current step");
        }
        graph_item(div(), part, "workflow-execution")
            .bg(rgb(if active { ACCENT } else { BACKGROUND }))
            .border_color(rgb(if active { BLUE } else { LINE }))
    })
}

fn diagram(draft: &Definition, mut item: impl FnMut(GraphPart) -> View) -> View {
    let ids = diagram_order(draft);
    let positions: BTreeMap<_, _> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            (
                id.clone(),
                (
                    31. + (index % 2) as f32 * COLUMN_STEP,
                    28. + (index / 2) as f32 * ROW_STEP,
                ),
            )
        })
        .collect();
    let height = (ids.len().div_ceil(2) as f32 * ROW_STEP + 28.).max(130.);
    let mut edges = Vec::new();
    let mut controls = Vec::new();
    for id in ids {
        let node = &draft.nodes[&id];
        let (x, y) = positions[&id];
        let selection = Selection::Node(id.clone());
        let label = format!("{} · {}", node.label, kind_label(&node.kind));
        controls.push(item(GraphPart {
            selection,
            id: format!("node-{id}"),
            title: node.label.clone(),
            label,
            x,
            y,
            w: BOX_WIDTH,
            h: BOX_HEIGHT,
        }));
        for (index, route) in node.routes.iter().enumerate() {
            let Some(&(to_x, to_y)) = positions.get(&route.to) else {
                continue;
            };
            let level = 14. + index.min(4) as f32 * 6.;
            let start = (x + BOX_WIDTH / 2., y + BOX_HEIGHT);
            let end = (to_x + BOX_WIDTH / 2., to_y);
            let rail = if x == to_x {
                x + BOX_WIDTH + 10.
            } else if x < to_x {
                191.
            } else {
                208.
            };
            let bend = y + BOX_HEIGHT + level;
            let landing = to_y - 10.;
            let path = vec![
                start,
                (start.0, bend),
                (rail, bend),
                (rail, landing),
                (end.0, landing),
                end,
            ];
            controls.push(item(GraphPart {
                selection: Selection::Route {
                    from: id.clone(),
                    route: route.id.clone(),
                },
                id: format!("route-{}", route.id),
                title: if route.when.is_some() { "◇" } else { "→" }.into(),
                label: format!(
                    "Route from {} to {}",
                    node.label, draft.nodes[&route.to].label
                ),
                x: rail - 11.,
                y: bend - 10.,
                w: 22.,
                h: 20.,
            }));
            edges.push(path);
        }
    }
    overlay()
        .w(GRAPH_WIDTH)
        .h(height)
        .child(
            canvas(move |_| {
                let mut canvas = Canvas::new();
                for points in &edges {
                    arrow(&mut canvas, points);
                }
                canvas
            })
            .w_full()
            .h_full(),
        )
        .children_vec(controls)
}

fn diagram_order(draft: &Definition) -> Vec<String> {
    let mut result = Vec::new();
    let mut queue = VecDeque::from([draft.entry.as_str()]);
    while let Some(id) = queue.pop_front() {
        if let Some(node) = draft.nodes.get(id)
            && !result.iter().any(|known| known == id)
        {
            result.push(id.to_owned());
            queue.extend(node.routes.iter().map(|route| route.to.as_str()));
        }
    }
    for id in draft.nodes.keys() {
        if !result.contains(id) {
            result.push(id.clone());
        }
    }
    result
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

fn kind_label(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Decision => "Decision".into(),
        NodeKind::Finish => "Finish".into(),
        NodeKind::Action { action } => ACTIONS
            .iter()
            .chain(native::ACTIONS)
            .find(|(key, _)| *key == action.id)
            .map_or_else(
                || format!("{} · Rev {}", action.id, action.number),
                |(_, label)| (*label).into(),
            ),
    }
}

fn types() -> Vec<(String, ValueType)> {
    [ValueType::Boolean, ValueType::Integer, ValueType::Text]
        .into_iter()
        .map(|ty| (type_label(ty).into(), ty))
        .collect()
}

fn type_picker(value: Signal<ValueType>) -> View {
    let current = value.clone();
    switch(
        move || current.get(),
        move |selected, cx| {
            let value = value.clone();
            picker(cx, "Output type", &selected, types(), move |ty| {
                value.set(ty);
            })
        },
    )
}

fn value_type(value: &Value) -> ValueType {
    match value {
        Value::Boolean(_) => ValueType::Boolean,
        Value::Integer(_) => ValueType::Integer,
        Value::Text(_) => ValueType::Text,
    }
}

fn type_label(ty: ValueType) -> &'static str {
    match ty {
        ValueType::Boolean => "Boolean",
        ValueType::Integer => "Integer",
        ValueType::Text => "Text",
    }
}

fn default_value(ty: ValueType) -> Value {
    match ty {
        ValueType::Boolean => Value::Boolean(false),
        ValueType::Integer => Value::Integer(0),
        ValueType::Text => Value::Text(String::new()),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::Text(value) => value.clone(),
    }
}

fn parse_value(ty: ValueType, raw: &str) -> Option<Value> {
    match ty {
        ValueType::Boolean => raw
            .trim()
            .to_ascii_lowercase()
            .parse()
            .ok()
            .map(Value::Boolean),
        ValueType::Integer => raw.trim().parse().ok().map(Value::Integer),
        ValueType::Text => Some(Value::Text(raw.into())),
    }
}

fn input(label: &str, signal: Signal<String>) -> View {
    column().w_full().gap(4.).child(hint(label)).child(
        text_input(label, signal)
            .w_full()
            .h(30.)
            .px(7.)
            .rounded(5.)
            .bg(rgb(BACKGROUND)),
    )
}

fn hint(title: impl Into<String>) -> View {
    text(title.into())
        .text_size(12.)
        .text_wrap(true)
        .text_color(rgb(MUTED))
}

fn heading(title: impl Into<String>) -> View {
    text(title.into())
        .text_size(17.)
        .text_wrap(true)
        .text_color(rgb(INK))
}

fn picker<T: Clone + PartialEq + 'static>(
    cx: &mut Context,
    label: &str,
    selected: &T,
    options: Vec<(String, T)>,
    choose: impl Fn(T) + 'static,
) -> View {
    let open = cx.state(false);
    let choose = Rc::new(choose);
    let title = options
        .iter()
        .find(|(_, value)| value == selected)
        .map_or_else(|| label.into(), |(name, _)| format!("{label} · {name}"));
    if options.len() > 10 {
        let open_picker = open.clone();
        let close = open.clone();
        let offset = cx.state(0.);
        return column()
            .child(control(title).on_click(move || {
                open_picker.set(true);
            }))
            .child(
                modal(label, open.clone())
                    .lazy()
                    .w(460.)
                    .p(14.)
                    .gap(9.)
                    .rounded(9.)
                    .bg(rgb(BACKGROUND))
                    .child(heading(label))
                    .child(
                        scroll(offset)
                            .h(235.)
                            .child(row().gap(6.).flex_wrap().children(options.into_iter().map(
                                |(label, value)| {
                                    let choose = choose.clone();
                                    let open = open.clone();
                                    control(label).on_click(move || {
                                        open.set(false);
                                        choose(value.clone());
                                    })
                                },
                            ))),
                    )
                    .child(control("Cancel").on_click(move || {
                        close.set(false);
                    })),
            );
    }
    let anchor = control(title);
    menu(label, open.clone(), anchor).children_vec(
        options
            .into_iter()
            .map(|(label, value)| {
                let choose = choose.clone();
                let close = open.clone();
                menu_item(label).on_click(move || {
                    close.set(false);
                    choose(value.clone());
                })
            })
            .collect(),
    )
}

fn arrow(canvas: &mut Canvas, points: &[(f32, f32)]) {
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
            rgb(MUTED),
            Stroke {
                width: 1.4,
                ..Stroke::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_reply_adopts_persisted_identity_without_reverting_intervening_edits() {
        let runtime = Runtime::new();
        let editor = Editor::in_runtime(&runtime);
        let submitted = editor.definition.get();
        editor.add_node();
        editor.title.set("Edited during save".into());

        let mut saved = submitted.clone();
        saved.revision = Revision {
            id: "catalog-id".into(),
            number: 2,
        };
        editor.accept_saved("Untitled workflow".into(), saved, submitted);

        let current = editor.definition.get();
        assert_eq!(current.nodes.len(), 2);
        assert_eq!(current.revision.id, "catalog-id");
        assert_eq!(current.revision.number, 2);
        assert_eq!(editor.title.get(), "Edited during save");

        editor.undo();
        let current = editor.definition.get();
        assert_eq!(current.nodes.len(), 1);
        assert_eq!(current.revision.id, "catalog-id");
        assert_eq!(current.revision.number, 2);
    }

    #[test]
    fn inserting_a_node_preserves_its_parent_route_condition_and_destination() {
        let runtime = Runtime::new();
        let editor = Editor::in_runtime(&runtime);
        let starter = tod_core::lifecycle::workflow::builtin::review_workflow();
        let old = starter.nodes["decision"]
            .routes
            .iter()
            .find(|route| route.id == "accept")
            .unwrap()
            .clone();
        editor.reset("Review and fix".into(), starter);

        editor.insert_node("decision", &old.id);
        let definition = editor.definition.get();
        let updated = definition.nodes["decision"]
            .routes
            .iter()
            .find(|route| route.id == old.id)
            .unwrap();
        assert_eq!(updated.when, old.when);
        assert_ne!(updated.to, old.to);
        let inserted = &definition.nodes[&updated.to];
        assert_eq!(inserted.routes[0].to, old.to);
        assert!(inserted.routes[0].when.is_none());
        assert!(validate(&definition).validate().is_ok());
    }
}
