use tod_core::lifecycle::workflow::{
    Revision,
    actions::{AgentAction, Instruction, PublishedAction, RuntimeContext},
};
use tod_store::{AgentPlatform, lifecycle_config, review::Reviewer};
use uuid::Uuid;
use zgx::{
    compose::prelude::*,
    reactive::{Runtime, Signal},
};
use zgx_desktop::WindowContext;

use crate::{BACKGROUND, LINE, MUTED, PANEL, control};

#[derive(Clone)]
pub enum Command {
    Load,
    History(String),
    Publish {
        base: Option<Revision>,
        spec: AgentAction,
    },
}

#[derive(Clone)]
struct InstructionInput {
    id: Uuid,
    title: Signal<String>,
    text: Signal<String>,
    source: Signal<String>,
    revision: Signal<String>,
}

impl PartialEq for InstructionInput {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl InstructionInput {
    fn new(runtime: &Runtime, value: Option<Instruction>) -> Self {
        let (title, text, source) = value
            .map(|item| (item.title, item.text, item.source))
            .unwrap_or_default();
        Self {
            id: Uuid::new_v4(),
            title: runtime.signal(title),
            text: runtime.signal(text),
            source: runtime.signal(source.as_ref().map(|r| r.id.clone()).unwrap_or_default()),
            revision: runtime.signal(source.map(|r| r.number.to_string()).unwrap_or_default()),
        }
    }

    fn value(&self) -> Result<Instruction, String> {
        let id = self.source.get();
        let version = self.revision.get();
        let source = if id.trim().is_empty() && version.trim().is_empty() {
            None
        } else {
            let number = version
                .trim()
                .parse()
                .map_err(|_| "A source revision needs a positive number.".to_string())?;
            Some(Revision {
                id: id.trim().into(),
                number,
            })
        };
        Ok(Instruction {
            title: self.title.get(),
            text: self.text.get(),
            source,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextKind {
    Task,
    Obligations,
    InheritedContext,
    Plan,
    ObligationVerdicts,
    ReviewFindings,
    ReviewerAssignment,
    WorkHistory,
    PullRequests,
    IncomingChanges,
    WorkingDirectory,
    Checks,
    ConfiguredSkills,
    TriageRequest,
    IncidentSamples,
    ProcessReviewLessons,
    NodeOutput,
}

impl ContextKind {
    const ALL: [Self; 17] = [
        Self::Task,
        Self::Obligations,
        Self::InheritedContext,
        Self::Plan,
        Self::ObligationVerdicts,
        Self::ReviewFindings,
        Self::ReviewerAssignment,
        Self::WorkHistory,
        Self::PullRequests,
        Self::IncomingChanges,
        Self::WorkingDirectory,
        Self::Checks,
        Self::ConfiguredSkills,
        Self::TriageRequest,
        Self::IncidentSamples,
        Self::ProcessReviewLessons,
        Self::NodeOutput,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Task => "Task description",
            Self::Obligations => "Obligations",
            Self::InheritedContext => "Inherited context",
            Self::Plan => "Plan steps",
            Self::ObligationVerdicts => "Obligation verdicts",
            Self::ReviewFindings => "Review findings",
            Self::ReviewerAssignment => "Reviewer assignment",
            Self::WorkHistory => "Work history",
            Self::PullRequests => "Linked pull requests",
            Self::IncomingChanges => "Incoming changes",
            Self::WorkingDirectory => "Working directory",
            Self::Checks => "Configured checks",
            Self::ConfiguredSkills => "Configured skill group",
            Self::TriageRequest => "Pending triage request",
            Self::IncidentSamples => "Claimed incident samples",
            Self::ProcessReviewLessons => "Process review lessons",
            Self::NodeOutput => "Output from a workflow node",
        }
    }
}

#[derive(Clone)]
struct ContextInput {
    id: Uuid,
    kind: ContextKind,
    first: Signal<String>,
    second: Signal<String>,
}

impl PartialEq for ContextInput {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl ContextInput {
    fn from_source(runtime: &Runtime, source: RuntimeContext) -> Self {
        use ContextKind as K;
        let (kind, first, second) = match source {
            RuntimeContext::Task => (K::Task, None, None),
            RuntimeContext::Obligations { phase } => (K::Obligations, phase, None),
            RuntimeContext::InheritedContext => (K::InheritedContext, None, None),
            RuntimeContext::Plan => (K::Plan, None, None),
            RuntimeContext::ObligationVerdicts => (K::ObligationVerdicts, None, None),
            RuntimeContext::ReviewFindings => (K::ReviewFindings, None, None),
            RuntimeContext::ReviewerAssignment { reviewer } => (
                K::ReviewerAssignment,
                reviewer.map(|reviewer| reviewer.as_str().to_owned()),
                None,
            ),
            RuntimeContext::WorkHistory => (K::WorkHistory, None, None),
            RuntimeContext::PullRequests => (K::PullRequests, None, None),
            RuntimeContext::IncomingChanges => (K::IncomingChanges, None, None),
            RuntimeContext::WorkingDirectory => (K::WorkingDirectory, None, None),
            RuntimeContext::Checks { phase } => (K::Checks, Some(phase), None),
            RuntimeContext::ConfiguredSkills { phase, group } => {
                (K::ConfiguredSkills, Some(phase), Some(group.to_string()))
            }
            RuntimeContext::NodeOutput { node, field } => (K::NodeOutput, Some(node), field),
            RuntimeContext::TriageRequest => (K::TriageRequest, None, None),
            RuntimeContext::IncidentSamples => (K::IncidentSamples, None, None),
            RuntimeContext::ProcessReviewLessons => (K::ProcessReviewLessons, None, None),
        };
        Self {
            id: Uuid::new_v4(),
            kind,
            first: runtime.signal(first.unwrap_or_default()),
            second: runtime.signal(second.unwrap_or_default()),
        }
    }

    fn new(runtime: &Runtime, kind: ContextKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            first: runtime.signal(String::new()),
            second: runtime.signal(String::new()),
        }
    }

    fn value(&self) -> Result<RuntimeContext, String> {
        let first = self.first.get().trim().to_owned();
        let second = self.second.get().trim().to_owned();
        Ok(match self.kind {
            ContextKind::Task => RuntimeContext::Task,
            ContextKind::Obligations => RuntimeContext::Obligations {
                phase: optional(first),
            },
            ContextKind::InheritedContext => RuntimeContext::InheritedContext,
            ContextKind::Plan => RuntimeContext::Plan,
            ContextKind::ObligationVerdicts => RuntimeContext::ObligationVerdicts,
            ContextKind::ReviewFindings => RuntimeContext::ReviewFindings,
            ContextKind::ReviewerAssignment => RuntimeContext::ReviewerAssignment {
                reviewer: optional(first)
                    .map(|role| Reviewer::parse(&role).map_err(|err| err.to_string()))
                    .transpose()?,
            },
            ContextKind::WorkHistory => RuntimeContext::WorkHistory,
            ContextKind::PullRequests => RuntimeContext::PullRequests,
            ContextKind::IncomingChanges => RuntimeContext::IncomingChanges,
            ContextKind::WorkingDirectory => RuntimeContext::WorkingDirectory,
            ContextKind::Checks => {
                if first.is_empty() {
                    return Err("Choose a lifecycle phase for the configured checks.".into());
                }
                RuntimeContext::Checks { phase: first }
            }
            ContextKind::ConfiguredSkills => {
                let group = if second.is_empty() {
                    0
                } else {
                    second
                        .parse()
                        .map_err(|_| "Enter a nonnegative skill group index.".to_string())?
                };
                if first.is_empty() {
                    return Err("Choose a lifecycle phase for the configured skill group.".into());
                }
                RuntimeContext::ConfiguredSkills {
                    phase: first,
                    group,
                }
            }
            ContextKind::NodeOutput => {
                if first.is_empty() {
                    return Err("Enter the source workflow node ID.".into());
                }
                RuntimeContext::NodeOutput {
                    node: first,
                    field: optional(second),
                }
            }
            ContextKind::TriageRequest => RuntimeContext::TriageRequest,
            ContextKind::IncidentSamples => RuntimeContext::IncidentSamples,
            ContextKind::ProcessReviewLessons => RuntimeContext::ProcessReviewLessons,
        })
    }

    fn controls(&self, runtime: &Runtime) -> View {
        let controls = column().gap(5.);
        match self.kind {
            ContextKind::Obligations => controls.child(input(
                "Lifecycle phase (blank includes all)",
                self.first.clone(),
            )),
            ContextKind::Checks => controls.child(phase_picker(
                runtime,
                "Checks phase",
                self.first.clone(),
                lifecycle_config::takes_checks,
            )),
            ContextKind::ReviewerAssignment => {
                controls.child(reviewer_picker(runtime, self.first.clone()))
            }
            ContextKind::ConfiguredSkills => controls
                .child(phase_picker(
                    runtime,
                    "Skill phase",
                    self.first.clone(),
                    lifecycle_config::takes_skills,
                ))
                .child(input("Group index (first group is 0)", self.second.clone())),
            ContextKind::NodeOutput => controls
                .child(input("Source node ID", self.first.clone()))
                .child(input(
                    "Output field (blank includes all)",
                    self.second.clone(),
                )),
            _ => controls,
        }
    }
}

fn optional(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn phase_picker(
    runtime: &Runtime,
    label: &'static str,
    choice: Signal<String>,
    accepts: fn(&str) -> bool,
) -> View {
    let opened = runtime.signal(false);
    let selected = choice.clone();
    let activate = opened.clone();
    let picker = choice_dialog(
        label,
        &opened,
        lifecycle_config::PHASES
            .into_iter()
            .filter(|phase| accepts(phase))
            .map(|phase| {
                let choice = choice.clone();
                let opened = opened.clone();
                control(phase.replace('_', " ")).on_click(move || {
                    choice.set(phase.into());
                    opened.set(false);
                })
            }),
    );
    row()
        .gap(6.)
        .items_center()
        .child(control(label).on_click(move || {
            activate.set(true);
        }))
        .child(picker)
        .child(switch(
            move || selected.get(),
            |phase, _| {
                text(phase.replace('_', " "))
                    .text_size(12.)
                    .text_color(rgb(MUTED))
            },
        ))
}

fn reviewer_picker(runtime: &Runtime, role: Signal<String>) -> View {
    let opened = runtime.signal(false);
    let activate = opened.clone();
    let selected = role.clone();
    let choices = [
        ("Solo reviewer", ""),
        ("Main reviewer", Reviewer::Main.as_str()),
        ("Risk reviewer", Reviewer::Risk.as_str()),
    ];
    row()
        .gap(6.)
        .items_center()
        .child(control("Reviewer role").on_click(move || {
            activate.set(true);
        }))
        .child(choice_dialog(
            "Reviewer role",
            &opened,
            choices.into_iter().map(|(label, key)| {
                let role = role.clone();
                let opened = opened.clone();
                control(label).on_click(move || {
                    role.set(key.into());
                    opened.set(false);
                })
            }),
        ))
        .child(switch(
            move || selected.get(),
            move |key, _| {
                let label = choices
                    .iter()
                    .find(|(_, value)| *value == key)
                    .map(|(label, _)| *label);
                text(label.unwrap_or("Choose reviewer role"))
                    .text_size(12.)
                    .text_color(rgb(MUTED))
            },
        ))
}

fn choice_dialog(
    title: &'static str,
    opened: &Signal<bool>,
    choices: impl IntoIterator<Item = View>,
) -> View {
    let cancel = opened.clone();
    modal(title, opened.clone())
        .lazy()
        .w(460.)
        .p(14.)
        .gap(9.)
        .rounded(9.)
        .bg(rgb(BACKGROUND))
        .child(text(title).text_size(19.))
        .child(row().gap(6.).flex_wrap().children(choices))
        .child(control("Cancel").on_click(move || {
            cancel.set(false);
        }))
}

#[derive(Clone)]
pub struct Panel {
    pub library: Signal<Vec<PublishedAction>>,
    pub history: Signal<Vec<PublishedAction>>,
    pub status: Signal<String>,
    pub busy: Signal<bool>,
    base: Signal<Option<Revision>>,
    provider: Signal<AgentPlatform>,
    title: Signal<String>,
    prompt: Signal<String>,
    model: Signal<String>,
    effort: Signal<String>,
    skills: Signal<Vec<InstructionInput>>,
    context: Signal<Vec<InstructionInput>>,
    runtime_context: Signal<Vec<ContextInput>>,
    offset: Signal<f32>,
    runtime: Runtime,
}

impl Panel {
    pub fn new(cx: &WindowContext) -> Self {
        Self {
            library: cx.ui.signal(Vec::new()),
            history: cx.ui.signal(Vec::new()),
            status: cx.ui.signal(String::new()),
            busy: cx.ui.signal(false),
            base: cx.ui.signal(None),
            provider: cx.ui.signal(AgentPlatform::default()),
            title: cx.ui.signal(String::new()),
            prompt: cx.ui.signal(String::new()),
            model: cx.ui.signal(String::new()),
            effort: cx.ui.signal(String::new()),
            skills: cx.ui.signal(Vec::new()),
            context: cx.ui.signal(Vec::new()),
            runtime_context: cx.ui.signal(Vec::new()),
            offset: cx.ui.signal(0.),
            runtime: cx.ui.runtime.clone(),
        }
    }

    pub fn open(&self, asset: Option<PublishedAction>) {
        let Some(asset) = asset else {
            self.runtime.batch(|| {
                self.base.set(None);
                self.title.set(String::new());
                self.prompt.set(String::new());
                self.model.set(String::new());
                self.effort.set(String::new());
                self.skills.set(Vec::new());
                self.context.set(Vec::new());
                self.runtime_context.set(Vec::new());
                self.provider.set(AgentPlatform::default());
            });
            return;
        };
        self.runtime.batch(|| {
            self.base.set(Some(asset.revision));
            self.title.set(asset.spec.title);
            self.prompt.set(asset.spec.prompt);
            self.provider.set(asset.spec.provider);
            self.model.set(asset.spec.model.unwrap_or_default());
            self.effort.set(asset.spec.effort.unwrap_or_default());
            self.skills.set(
                asset
                    .spec
                    .skills
                    .into_iter()
                    .map(|item| InstructionInput::new(&self.runtime, Some(item)))
                    .collect(),
            );
            self.context.set(
                asset
                    .spec
                    .context
                    .into_iter()
                    .map(|item| InstructionInput::new(&self.runtime, Some(item)))
                    .collect(),
            );
            self.runtime_context.set(
                asset
                    .spec
                    .runtime_context
                    .into_iter()
                    .map(|source| ContextInput::from_source(&self.runtime, source))
                    .collect(),
            );
        });
    }

    pub fn open_published(&self, action: PublishedAction) {
        // A background save adopts the committed revision while keeping any
        // form edits the user made after submitting.
        self.base.set(Some(action.revision));
    }

    fn spec(&self) -> Result<AgentAction, String> {
        let instructions = |items: &Signal<Vec<InstructionInput>>| {
            items.with(|items| {
                items
                    .iter()
                    .map(InstructionInput::value)
                    .collect::<Result<Vec<_>, _>>()
            })
        };
        let setting = |field: &Signal<String>| {
            let value = field.get();
            (!value.trim().is_empty()).then(|| value)
        };
        Ok(AgentAction {
            title: self.title.get(),
            prompt: self.prompt.get(),
            provider: self.provider.get(),
            skills: instructions(&self.skills)?,
            context: instructions(&self.context)?,
            runtime_context: self.runtime_context.with(|sources| {
                sources
                    .iter()
                    .map(ContextInput::value)
                    .collect::<Result<Vec<_>, _>>()
            })?,
            model: setting(&self.model),
            effort: setting(&self.effort),
        })
    }

    pub fn view(&self, dispatch: impl Fn(Command) + 'static, back: impl Fn() + 'static) -> View {
        let dispatch = std::rc::Rc::new(dispatch);
        let library = self.library.clone();
        let history = self.history.clone();
        let busy = self.busy.clone();
        let publish_busy = self.busy.clone();
        let notice = self.status.clone();
        let new = self.clone();
        let edit = self.clone();
        let edit_history = self.clone();
        let save = self.clone();
        let publish = dispatch.clone();
        let send_history = dispatch.clone();
        let base = self.base.clone();
        let provider = self.provider.clone();

        column()
            .id("workflow-action-assets")
            .w_full()
            .h_full()
            .gap(8.)
            .child(
                row()
                    .gap(8.)
                    .items_center()
                    .child(text("Agent actions").grow().text_size(20.))
                    .child(
                        control("Publish action revision")
                            .id("workflow-action-publish")
                            .disabled_when(move || publish_busy.get())
                            .on_click(move || {
                                if !save.busy.get() {
                                    match save.spec() {
                                        Ok(spec) => publish(Command::Publish {
                                            base: save.base.get(),
                                            spec,
                                        }),
                                        Err(error) => {
                                            save.status.set(error);
                                        }
                                    }
                                }
                            }),
                    )
                    .child(
                        control("Back to workflows")
                            .id("workflow-actions-back")
                            .on_click(back),
                    ),
            )
            .child(
                text_signal(move || {
                    if busy.get() {
                        "Working…".into()
                    } else {
                        notice.get()
                    }
                })
                .text_wrap(true)
                .text_color(rgb(MUTED)),
            )
            .child(
                scroll(self.offset.clone())
                    .id("workflow-actions-scroll")
                    .w_full()
                    .grow()
                    .min_h(0.)
                    .child(
                        row()
                            .w_full()
                            .items_start()
                            .gap(14.)
                            .child(
                                column()
                                    .w(220.)
                                    .gap(6.)
                                    .child(
                                        control("New action").id("workflow-action-new").on_click(
                                            move || {
                                                if !new.busy.get() {
                                                    new.open(None);
                                                }
                                            },
                                        ),
                                    )
                                    .child(switch(
                                        move || library.get(),
                                        move |items, _| {
                                            let mut rows = column().gap(5.);
                                            for asset in items {
                                                let open = edit.clone();
                                                let history = send_history.clone();
                                                let id = asset.revision.id.clone();
                                                rows = rows.child(
                                                    row()
                                                        .gap(5.)
                                                        .child(
                                                            control(format!(
                                                                "{} · r{}",
                                                                asset.spec.title,
                                                                asset.revision.number
                                                            ))
                                                            .grow()
                                                            .id(format!(
                                                                "workflow-open-action-{id}"
                                                            ))
                                                            .on_click(move || {
                                                                if !open.busy.get() {
                                                                    open.open(Some(asset.clone()));
                                                                }
                                                            }),
                                                        )
                                                        .child(control("History").on_click(
                                                            move || {
                                                                history(Command::History(
                                                                    id.clone(),
                                                                ))
                                                            },
                                                        )),
                                                );
                                            }
                                            rows
                                        },
                                    ))
                                    .child(switch(
                                        move || history.get(),
                                        move |items, _| {
                                            column().gap(5.).children(items.into_iter().map(
                                                |asset| {
                                                    let open = edit_history.clone();
                                                    control(format!(
                                                        "{} · r{}",
                                                        asset.spec.title, asset.revision.number
                                                    ))
                                                    .on_click(move || {
                                                        if !open.busy.get() {
                                                            open.open(Some(asset.clone()));
                                                        }
                                                    })
                                                },
                                            ))
                                        },
                                    )),
                            )
                            .child(
                                column()
                                    .grow()
                                    .min_w(0.)
                                    .gap(8.)
                                    .p(10.)
                                    .rounded(8.)
                                    .bg(rgb(PANEL))
                                    .child(
                                        text_signal(move || {
                                            base.with(|base| {
                                                base.as_ref()
                                                    .map(|revision| {
                                                        format!(
                                                            "Editing revision {}",
                                                            revision.number
                                                        )
                                                    })
                                                    .unwrap_or_else(|| "New action".into())
                                            })
                                        })
                                        .text_size(16.),
                                    )
                                    .child(input("Action title", self.title.clone()))
                                    .child(area("Prompt", self.prompt.clone(), 140.))
                                    .child(
                                        row().gap(6.).children(
                                            [
                                                AgentPlatform::Claude,
                                                AgentPlatform::OpenAi,
                                                AgentPlatform::Cursor,
                                                AgentPlatform::Google,
                                            ]
                                            .into_iter()
                                            .map(
                                                |option| {
                                                    let select = provider.clone();
                                                    let active = provider.clone();
                                                    control(option.label())
                                                        .reactive_style(move || {
                                                            Styles::new().border(1.).border_color(
                                                                rgb(if active.get() == option {
                                                                    0x87beff
                                                                } else {
                                                                    LINE
                                                                }),
                                                            )
                                                        })
                                                        .on_click(move || {
                                                            select.set(option);
                                                        })
                                                },
                                            ),
                                        ),
                                    )
                                    .child(input(
                                        "Model (blank uses provider default)",
                                        self.model.clone(),
                                    ))
                                    .child(input(
                                        "Effort (blank uses provider default)",
                                        self.effort.clone(),
                                    ))
                                    .child(self.instructions("Skills", self.skills.clone()))
                                    .child(self.instructions("Context text", self.context.clone()))
                                    .child(self.context_sources()),
                            ),
                    ),
            )
    }

    fn context_sources(&self) -> View {
        let opened = self.runtime.signal(false);
        let source = self.runtime_context.clone();
        let rows = self.runtime_context.clone();
        let runtime = self.runtime.clone();
        let busy = self.busy.clone();
        let activate = opened.clone();
        let anchor = control("Add source")
            .id("workflow-context-add-source")
            .disabled_when(move || busy.get())
            .on_click(move || {
                activate.set(true);
            });
        let picker = choice_dialog(
            "Runtime context sources",
            &opened,
            ContextKind::ALL.into_iter().map(|kind| {
                let opened = opened.clone();
                let source = source.clone();
                let runtime = runtime.clone();
                control(kind.label()).on_click(move || {
                    opened.set(false);
                    source.update(|rows| rows.push(ContextInput::new(&runtime, kind)));
                })
            }),
        );
        let current = self.runtime_context.clone();
        let runtime = self.runtime.clone();
        column()
            .gap(5.)
            .child(
                row()
                    .gap(8.)
                    .items_center()
                    .child(text("Runtime context").grow())
                    .child(anchor)
                    .child(picker),
            )
            .child(
                text("Each run reads these sources for the action's task or workflow visit.")
                    .text_size(12.)
                    .text_wrap(true)
                    .text_color(rgb(MUTED)),
            )
            .child(switch(
                move || current.get(),
                move |entries, _| {
                    let count = entries.len();
                    column()
                        .gap(6.)
                        .children(entries.into_iter().enumerate().map(|(index, item)| {
                            let id = item.id;
                            let up = rows.clone();
                            let down = rows.clone();
                            let remove = rows.clone();
                            column()
                                .gap(5.)
                                .p(8.)
                                .rounded(5.)
                                .border(1.)
                                .border_color(rgb(LINE))
                                .id(format!("workflow-context-source-{id}"))
                                .child(
                                    row()
                                        .gap(5.)
                                        .items_center()
                                        .child(text(item.kind.label()).grow())
                                        .child(
                                            control("Move up")
                                                .disabled_when(move || index == 0)
                                                .on_click(move || {
                                                    up.update(|entries| {
                                                        move_context(entries, id, -1)
                                                    });
                                                }),
                                        )
                                        .child(
                                            control("Move down")
                                                .disabled_when(move || index + 1 == count)
                                                .on_click(move || {
                                                    down.update(|entries| {
                                                        move_context(entries, id, 1)
                                                    });
                                                }),
                                        )
                                        .child(control("Remove").on_click(move || {
                                            remove.update(|entries| {
                                                entries.retain(|entry| entry.id != id)
                                            });
                                        })),
                                )
                                .child(item.controls(&runtime))
                        }))
                },
            ))
    }

    fn instructions(&self, label: &'static str, items: Signal<Vec<InstructionInput>>) -> View {
        let source = items.clone();
        let add = items.clone();
        let runtime = self.runtime.clone();
        column()
            .gap(5.)
            .child(
                row()
                    .gap(8.)
                    .items_center()
                    .child(text(label).grow())
                    .child(control("Add").on_click(move || {
                        add.update(|items| items.push(InstructionInput::new(&runtime, None)));
                    })),
            )
            .child(switch(
                move || source.get(),
                move |values, _| {
                    column().gap(6.).children(values.into_iter().map(|item| {
                        let remove = items.clone();
                        let id = item.id;
                        column()
                            .gap(5.)
                            .p(8.)
                            .rounded(5.)
                            .border(1.)
                            .border_color(rgb(LINE))
                            .child(
                                row()
                                    .gap(5.)
                                    .child(input("Title", item.title).grow())
                                    .child(control("Remove").on_click(move || {
                                        remove.update(|items| items.retain(|item| item.id != id));
                                    })),
                            )
                            .child(area("Instruction text", item.text, 70.))
                            .child(
                                row()
                                    .gap(5.)
                                    .child(input("Source asset ID (optional)", item.source).grow())
                                    .child(input("Source revision", item.revision).w(110.)),
                            )
                    }))
                },
            ))
    }
}

fn move_context(sources: &mut [ContextInput], id: Uuid, offset: isize) {
    let Some(position) = sources.iter().position(|entry| entry.id == id) else {
        return;
    };
    if let Some(next) = position.checked_add_signed(offset)
        && next < sources.len()
    {
        sources.swap(position, next);
    }
}

fn input(label: &str, value: Signal<String>) -> View {
    column()
        .gap(3.)
        .child(text(label.to_owned()).text_size(12.).text_color(rgb(MUTED)))
        .child(
            text_input(label.to_owned(), value)
                .h(32.)
                .px(8.)
                .bg(rgb(BACKGROUND))
                .border(1.)
                .border_color(rgb(LINE)),
        )
}

fn area(label: &str, value: Signal<String>, height: f32) -> View {
    column()
        .gap(3.)
        .child(text(label.to_owned()).text_size(12.).text_color(rgb(MUTED)))
        .child(
            text_area(label.to_owned(), value)
                .h(height)
                .p(8.)
                .bg(rgb(BACKGROUND))
                .border(1.)
                .border_color(rgb(LINE)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_context_editor_round_trips_configured_and_visit_sources() {
        let runtime = Runtime::new();
        let sources = [
            RuntimeContext::WorkingDirectory,
            RuntimeContext::ReviewerAssignment { reviewer: None },
            RuntimeContext::ReviewerAssignment {
                reviewer: Some(Reviewer::Risk),
            },
            RuntimeContext::IncomingChanges,
            RuntimeContext::TriageRequest,
            RuntimeContext::IncidentSamples,
            RuntimeContext::ProcessReviewLessons,
            RuntimeContext::Checks {
                phase: "review".into(),
            },
            RuntimeContext::ConfiguredSkills {
                phase: "verify".into(),
                group: 2,
            },
            RuntimeContext::NodeOutput {
                node: "review".into(),
                field: Some("reviewed_commit".into()),
            },
            RuntimeContext::Obligations { phase: None },
        ];

        for source in sources {
            let input = ContextInput::from_source(&runtime, source.clone());
            assert_eq!(input.value(), Ok(source));
        }
    }

    #[test]
    fn runtime_context_editor_preserves_source_order_and_checks_required_parameters() {
        let runtime = Runtime::new();
        let task = ContextInput::new(&runtime, ContextKind::Task);
        let skills = ContextInput::new(&runtime, ContextKind::ConfiguredSkills);
        let output = ContextInput::new(&runtime, ContextKind::NodeOutput);
        let mut sources = vec![task.clone(), skills.clone(), output.clone()];

        move_context(&mut sources, task.id, -1);
        move_context(&mut sources, output.id, -1);
        assert_eq!(sources[0].id, task.id);
        assert_eq!(sources[1].id, output.id);
        assert_eq!(sources[2].id, skills.id);

        assert!(output.value().is_err());
        output.first.set("review".into());
        assert_eq!(
            output.value(),
            Ok(RuntimeContext::NodeOutput {
                node: "review".into(),
                field: None
            })
        );
        skills.first.set("review".into());
        assert_eq!(
            skills.value(),
            Ok(RuntimeContext::ConfiguredSkills {
                phase: "review".into(),
                group: 0
            })
        );
        skills.second.set("-1".into());
        assert!(skills.value().is_err());
    }
}
