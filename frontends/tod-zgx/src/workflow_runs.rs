use std::collections::BTreeMap;
use tod_core::lifecycle::workflow::{
    Definition, Observation, Observations, Revision, Value, ValueType,
    improvement::{CapturedContext, Comparison, Subject},
    metrics::RevisionMetrics,
    migration,
    runner::{ActionRequest, Run, Visit, VisitTrace},
};
use uuid::Uuid;
use zgx::{
    compose::prelude::*,
    reactive::{Runtime, Signal},
};
use zgx_desktop::WindowContext;

use crate::{App, BACKGROUND, LINE, MUTED, PANEL, control};

#[derive(Clone, Debug)]
pub enum Command {
    Load,
    Inspect(Uuid),
    InspectContext {
        run: Uuid,
        visit: u64,
    },
    RunAgent(Run),
    PreviewMigration {
        run: Uuid,
        target: Revision,
    },
    Migrate(migration::Preview),
    PreviewAdoption(Revision),
    Adopt(migration::AdoptionPreview),
    PreviewDetach(Uuid),
    Detach(migration::DetachPreview),
    Assign(Revision),
    Resume {
        run: Run,
        observations: Observations,
    },
    Complete {
        request: ActionRequest,
        values: BTreeMap<String, Value>,
        evidence: String,
    },
    Cancel(Run),
    Replay {
        run: Uuid,
    },
    Lesson {
        run: Uuid,
        sequence: u64,
        subject: String,
        text: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub title: String,
    pub cloud_location: bool,
    pub choices: Vec<(String, Revision)>,
    pub mapped_workflows: std::collections::BTreeSet<String>,
    pub run: Option<Run>,
    pub active_run: Option<Uuid>,
    pub history: Vec<Run>,
    pub definition: Option<Definition>,
    pub node_label: String,
    pub request: Option<ActionRequest>,
    pub agent_pending: bool,
    pub action_fields: BTreeMap<String, ValueType>,
    pub manual_fields: BTreeMap<String, ValueType>,
    pub traces: Vec<VisitTrace>,
    pub subjects: BTreeMap<u64, Vec<Subject>>,
    pub metrics: Vec<RevisionMetrics>,
    pub visits: Vec<Visit>,
    pub replay: Option<Comparison>,
    pub captured_context: Option<(u64, Vec<CapturedContext>)>,
    pub migration: Option<migration::Preview>,
    pub adoption: Option<migration::AdoptionPreview>,
    pub detach: Option<migration::DetachPreview>,
}

#[derive(Clone)]
pub struct Panel {
    pub task: Signal<Option<(String, String)>>,
    pub state: Signal<Snapshot>,
    pub notice: Signal<String>,
    pub busy: Signal<bool>,
    offset: Signal<f32>,
    runtime: Runtime,
}

impl Panel {
    pub fn new(cx: &WindowContext) -> Self {
        Self {
            task: cx.ui.signal(None),
            state: cx.ui.signal(Snapshot::default()),
            notice: cx.ui.signal(String::new()),
            busy: cx.ui.signal(false),
            offset: cx.ui.signal(0.),
            runtime: cx.ui.runtime.clone(),
        }
    }

    pub fn open(&self, task: Option<(String, String)>) {
        self.state.set(Snapshot {
            title: task
                .as_ref()
                .map(|(_, title)| title.clone())
                .unwrap_or_default(),
            ..Default::default()
        });
        self.task.set(task);
        self.notice.set(String::new());
        self.offset.set(0.);
    }

    pub fn view(&self, app: &App) -> View {
        let state = self.state.clone();
        let panel = self.clone();
        let app = app.clone();
        let notice = self.notice.clone();
        let busy = self.busy.clone();
        let refresh = app.clone();
        column()
            .id("workflow-runs")
            .w_full()
            .h_full()
            .gap(10.)
            .child(
                row()
                    .gap(8.)
                    .items_center()
                    .child(text("Workflow run").grow().text_size(20.))
                    .child(
                        control("Refresh")
                            .id("workflow-run-refresh")
                            .on_click(move || refresh.workflow_run_command(Command::Load)),
                    ),
            )
            .child(
                text_signal(move || {
                    busy.get()
                        .then(|| "Working…".to_string())
                        .unwrap_or_else(|| notice.get())
                })
                .text_wrap(true)
                .text_color(rgb(MUTED)),
            )
            .child(
                scroll(self.offset.clone())
                    .id("workflow-run-scroll")
                    .grow()
                    .min_h(0.)
                    .w_full()
                    .child(switch(
                        move || state.get(),
                        move |state, _| panel.content(state, &app),
                    )),
            )
    }

    fn content(&self, state: Snapshot, app: &App) -> View {
        let mut body = column().gap(12.).w_full();
        if state.title.is_empty() {
            return body.child(
                text("Open a task in a live workspace to manage its workflow.").text_wrap(true),
            );
        }
        body = body.child(text(state.title).text_size(17.));
        if state.cloud_location {
            body = body.child(text("Choose a local devcontainer location to run a workflow on this task.").text_wrap(true));
        }
        let historical = state
            .run
            .as_ref()
            .is_some_and(|run| state.active_run != Some(run.id));
        if state.history.len() > 1 || (state.active_run.is_none() && !state.history.is_empty()) {
            let mut history = section().child(text("Run history"));
            if historical {
                let app = app.clone();
                history = history.child(control("Back to task").on_click(move || {
                    app.workflow_run_command(Command::Load);
                }));
            }
            for (index, recorded) in state.history.iter().enumerate() {
                let id = recorded.id;
                let navigate = app.clone();
                let number = state.history.len() - index;
                history = history.child(
                    control(format!(
                        "Run {number} · Revision {} · {:?}",
                        recorded.revision.number, recorded.status
                    ))
                    .id(format!("workflow-run-{id}"))
                    .on_click(move || navigate.workflow_run_command(Command::Inspect(id))),
                );
            }
            body = body.child(history);
        }
        if !state.metrics.is_empty() {
            let mut metrics = section().child(text("Run totals by workflow revision"));
            for revision in &state.metrics {
                let title = state
                    .choices
                    .iter()
                    .find(|(_, current)| current.id == revision.revision.id)
                    .map_or(revision.revision.id.as_str(), |(title, _)| title.as_str());
                metrics = metrics.child(
                    text(format!(
                        "{} · revision {} · {}/{} completed · {} visits ({} repeats) · {} ms waiting · {} input / {} output tokens",
                        title,
                        revision.revision.number,
                        revision.completed_runs,
                        revision.runs,
                        revision.visits,
                        revision.revisits,
                        revision.wait_ms,
                        revision.input_tokens,
                        revision.output_tokens,
                    ))
                    .text_wrap(true),
                );
            }
            body = body.child(metrics);
        }
        let allow_assignment = !state.cloud_location && !historical
            && state
                .run
                .as_ref()
                .is_none_or(|run| run.status.is_terminal());
        if allow_assignment {
            let mut choices = section().child(text("Assign a published workflow"));
            if state.choices.is_empty() {
                choices = choices.child(
                    text("Publish a revision from Workflows to assign it here.")
                        .text_wrap(true)
                        .text_color(rgb(MUTED)),
                );
            }
            for (title, revision) in state.choices.iter().cloned() {
                let app = app.clone();
                let id = format!("workflow-assign-{}", revision.id);
                let mapped = state.mapped_workflows.contains(&revision.id);
                choices = choices.child(
                    control(format!("{title} · revision {}", revision.number))
                        .id(id)
                        .on_click(move || {
                            let command = if mapped {
                                Command::PreviewAdoption(revision.clone())
                            } else {
                                Command::Assign(revision.clone())
                            };
                            app.workflow_run_command(command)
                        }),
                );
            }
            body = body.child(choices);
        }
        if let Some(preview) = state.adoption {
            let mut details = section()
                .child(text(format!("Adopt from {}", preview.stay.phase)))
                .child(text(preview.warning.clone()).text_wrap(true));
            if preview.blocked_by_live_work {
                details = details.child(text("Stop running work before adopting."));
            } else {
                let app = app.clone();
                details =
                    details.child(control("Adopt workflow").id("workflow-adopt").on_click(
                        move || app.workflow_run_command(Command::Adopt(preview.clone())),
                    ));
            }
            body = body.child(details);
        }
        let Some(run) = state.run else { return body };
        body = body.child(
            section()
                .child(text(state.node_label).text_size(17.))
                .child(
                    text(format!(
                        "{:?} · Visit {} · Revision {}",
                        run.status, run.visit, run.revision.number
                    ))
                    .text_color(rgb(MUTED)),
                ),
        );
        if !state.cloud_location && !historical && !run.status.is_terminal() {
            if let Some((_, next)) = state.choices.iter().find(|(_, revision)| {
                revision.id == run.revision.id && revision.number != run.revision.number
            }) {
                let app = app.clone();
                let id = run.id;
                let target = next.clone();
                body = body.child(
                    control(format!("Preview revision {} migration", next.number))
                        .id("workflow-migration-preview")
                        .on_click(move || {
                            app.workflow_run_command(Command::PreviewMigration {
                                run: id,
                                target: target.clone(),
                            })
                        }),
                );
            }
            if let Some(request) = state.request {
                body = body.child(self.action_form(request, state.action_fields, app));
            } else if state.agent_pending {
                let start = app.clone();
                let selected = run.clone();
                body = body.child(
                    control("Start or resume agent")
                        .id("workflow-run-agent")
                        .on_click(move || {
                            start.workflow_run_command(Command::RunAgent(selected.clone()))
                        }),
                );
                body = body.child(context_button(app, run.id, run.visit));
            } else {
                body = body.child(self.resume_form(run.clone(), state.manual_fields, app));
            }
            let cancel = app.clone();
            let active = run.clone();
            body = body.child(
                control("Cancel run")
                    .id("workflow-cancel")
                    .on_click(move || cancel.workflow_run_command(Command::Cancel(active.clone()))),
            );
        }
        if !historical {
            let id = run.id;
            let app = app.clone();
            body = body.child(
                control("Preview return to legacy runner")
                    .id("workflow-detach-preview")
                    .on_click(move || app.workflow_run_command(Command::PreviewDetach(id))),
            );
        }
        if let Some(preview) = state.detach {
            let mut details = section()
                .child(text("Return to legacy runner"))
                .child(text(preview.warning.clone()).text_wrap(true));
            if preview.blocked_by_live_work {
                details = details.child(text("Stop running work before detaching."));
            } else {
                let app = app.clone();
                details =
                    details.child(control("Detach workflow").id("workflow-detach").on_click(
                        move || app.workflow_run_command(Command::Detach(preview.clone())),
                    ));
            }
            body = body.child(details);
        }
        if let Some(preview) = state.migration {
            let mut changes = section()
                .child(text(format!("Revision {} migration", preview.target.number)).text_size(17.))
                .child(text(preview.warning.clone()).text_wrap(true));
            for (name, change) in [
                ("Current node outputs", &preview.outputs),
                ("Workflow fields", &preview.fields),
            ] {
                for (kind, keys) in [
                    ("Added", &change.added),
                    ("Removed", &change.removed),
                    ("Changed", &change.changed),
                ] {
                    if !keys.is_empty() {
                        changes = changes.child(
                            text(format!("{name} · {kind} · {}", keys.join(", "))).text_wrap(true),
                        );
                    }
                }
            }
            body = if let Some(reason) = &preview.blocked_by_phase {
                body.child(changes.child(text(reason.clone()).text_wrap(true)))
            } else if preview.blocked_by_live_work {
                body.child(changes.child(
                    text("Stop running work before applying this revision.").text_wrap(true),
                ))
            } else {
                let apply = app.clone();
                body.child(
                    changes.child(
                        control("Restart on this revision")
                            .id("workflow-migrate")
                            .on_click(move || {
                                apply.workflow_run_command(Command::Migrate(preview.clone()))
                            }),
                    ),
                )
            };
        }
        if !state.traces.is_empty() {
            let compare = app.clone();
            let id = run.id;
            body = body.child(
                control("Compare with current draft")
                    .id("workflow-replay")
                    .on_click(move || compare.workflow_run_command(Command::Replay { run: id })),
            );
        }
        if let Some(comparison) = state.replay {
            let changed = comparison
                .visits
                .iter()
                .filter(|visit| visit.changed == Some(true))
                .count();
            let unevaluated = comparison
                .visits
                .iter()
                .filter(|visit| visit.changed.is_none())
                .count();
            let mut report = section().child(text("Draft replay").text_size(17.)).child(
                text(format!(
                    "{changed} changed decisions · {unevaluated} unavailable comparisons"
                ))
                .text_wrap(true),
            );
            for entry in comparison.visits {
                let result = entry
                    .replay
                    .as_ref()
                    .map(|trace| describe(&trace.decision))
                    .unwrap_or_else(|| {
                        if matches!(
                            entry.recorded.decision,
                            tod_core::lifecycle::workflow::Decision::OperatorMove { .. }
                        ) {
                            "Operator move".into()
                        } else {
                            "Node removed".into()
                        }
                    });
                report = report.child(
                    text(format!(
                        "Visit {} · {} → {}",
                        entry.recorded.visit,
                        describe(&entry.recorded.decision),
                        result
                    ))
                    .text_wrap(true),
                );
                if !entry.missing_fields.is_empty() {
                    report = report.child(
                        text(format!(
                            "Missing observations · {}",
                            entry.missing_fields.join(", ")
                        ))
                        .text_wrap(true)
                        .text_color(rgb(MUTED)),
                    );
                }
            }
            body = body.child(report);
        }
        if let Some((visit, captures)) = state.captured_context {
            let view = app.clone();
            let run_id = run.id;
            let mut context = section().child(
                row()
                    .items_center()
                    .gap(8.)
                    .child(text(format!("Recorded launch context · Visit {visit}")).grow())
                    .child(
                        control("Close context")
                            .on_click(move || view.workflow_run_command(Command::Inspect(run_id))),
                    ),
            );
            if captures.is_empty() {
                context = context.child(text("This visit has no recorded runtime context."));
            }
            for capture in captures {
                context = context
                    .child(
                        text(format!(
                            "Conversation {} · Turn {}",
                            capture.conversation, capture.turn
                        ))
                        .text_wrap(true),
                    )
                    .child(text(capture.text).text_wrap(true));
            }
            body = body.child(context);
        }
        for entry in state.traces.into_iter().rev() {
            let mut detail = section()
                .child(
                    text(format!(
                        "Visit {} · {}",
                        entry.trace.visit, entry.trace.node
                    ))
                    .text_size(15.),
                )
                .child(text(describe(&entry.trace.decision)).text_wrap(true));
            if let Some(output) = state
                .visits
                .iter()
                .find(|visit| visit.number == entry.trace.visit)
                .and_then(|visit| visit.output.as_ref())
            {
                for revision in &output.assets {
                    detail = detail.child(
                        text(format!("{} · revision {}", revision.id, revision.number))
                            .text_wrap(true)
                            .text_color(rgb(MUTED)),
                    );
                }
                let metrics = &output.metrics;
                if output
                    .evidence
                    .iter()
                    .any(|evidence| evidence.starts_with("conversation/"))
                {
                    detail = detail.child(context_button(app, run.id, entry.trace.visit));
                }
                if metrics.duration_ms > 0 || metrics.output_tokens > 0 {
                    detail = detail.child(
                        text(format!(
                            "{} ms · {} input tokens · {} output tokens",
                            metrics.duration_ms, metrics.input_tokens, metrics.output_tokens
                        ))
                        .text_wrap(true)
                        .text_color(rgb(MUTED)),
                    );
                }
            }
            for route in entry.trace.routes {
                let result = route.condition.map_or_else(
                    || "otherwise".into(),
                    |condition| format!("{:?}", condition.state),
                );
                detail = detail.child(
                    text(format!("{} · {}", route.route, result))
                        .text_wrap(true)
                        .text_color(rgb(MUTED)),
                );
            }
            for (field, observation) in entry.trace.observations {
                let result = match observation {
                    Observation::Ready { value, evidence } => {
                        format!("{} · {}", display(value), evidence)
                    }
                    Observation::Waiting { reason } => format!("Waiting · {reason}"),
                    Observation::Error { reason } => format!("Error · {reason}"),
                };
                detail = detail.child(text(format!("{field} · {result}")).text_wrap(true));
            }
            let note = self.runtime.signal(String::new());
            let submit_note = note.clone();
            let app = app.clone();
            let id = run.id;
            let sequence = entry.sequence;
            let subject = self.runtime.signal("node".to_owned());
            if let Some(subjects) = state.subjects.get(&sequence) {
                let labeled = subjects.clone();
                let selected = subject.clone();
                let show = self.runtime.signal(false);
                let toggle = show.clone();
                let close = show.clone();
                detail = detail
                    .child(text_signal(move || {
                        let key = selected.get();
                        labeled.iter().find(|item| item.key == key).map_or_else(
                            || "Lesson target · Step".into(),
                            |item| format!("Lesson target · {}", item.label),
                        )
                    }))
                    .child(control("Choose lesson target").on_click(move || {
                        toggle.set(!toggle.get());
                    }));
                let choices = subjects.clone();
                let selected = subject.clone();
                detail = detail.child(switch(
                    move || show.get(),
                    move |open, _| {
                        column()
                            .gap(4.)
                            .children(choices.iter().filter(|_| open).map(|item| {
                                let key = item.key.clone();
                                let selected = selected.clone();
                                let close = close.clone();
                                control(item.label.clone()).on_click(move || {
                                    selected.set(key.clone());
                                    close.set(false);
                                })
                            }))
                    },
                ));
            }
            body = body.child(
                detail.child(input("What should improve?", note)).child(
                    control("Record lesson")
                        .id(format!("workflow-lesson-{sequence}"))
                        .on_click(move || {
                            app.workflow_run_command(Command::Lesson {
                                run: id,
                                sequence,
                                subject: subject.get(),
                                text: submit_note.get(),
                            });
                        }),
                ),
            );
        }
        #[cfg(feature = "live")]
        if let Some(definition) = &state.definition {
            return row()
                .gap(12.)
                .w_full()
                .child(
                    section()
                        .w(430.)
                        .child(crate::lifecycle_editor::run_diagram(definition, &run.node)),
                )
                .child(body.grow().min_w(0.));
        }
        body
    }

    fn action_form(
        &self,
        request: ActionRequest,
        fields: BTreeMap<String, ValueType>,
        app: &App,
    ) -> View {
        let mut form = section().child(text("Record action result"));
        let mut inputs = Vec::new();
        for (name, ty) in fields {
            let value = self.runtime.signal(String::new());
            form = form.child(input(
                &format!("{name} ({})", type_label(ty)),
                value.clone(),
            ));
            inputs.push((name, ty, value));
        }
        let evidence = self.runtime.signal(String::new());
        let evidence_to_send = evidence.clone();
        let app = app.clone();
        let status = self.notice.clone();
        form.child(input("Evidence reference", evidence)).child(
            control("Complete action")
                .id("workflow-complete")
                .on_click(move || match collect(&inputs, false) {
                    Ok(values) => app.workflow_run_command(Command::Complete {
                        request: request.clone(),
                        values,
                        evidence: evidence_to_send.get(),
                    }),
                    Err(error) => {
                        status.set(error);
                    }
                }),
        )
    }

    fn resume_form(&self, run: Run, fields: BTreeMap<String, ValueType>, app: &App) -> View {
        let mut form = section();
        let mut inputs = Vec::new();
        for (name, ty) in fields {
            let value = self.runtime.signal(String::new());
            form = form.child(input(
                &format!("{name} ({})", type_label(ty)),
                value.clone(),
            ));
            inputs.push((name, ty, value));
        }
        let task = run.task;
        let app = app.clone();
        let status = self.notice.clone();
        form.child(
            control("Run or check gates")
                .id("workflow-resume")
                .on_click(move || match collect(&inputs, true) {
                    Ok(values) => {
                        let observations = values
                            .into_iter()
                            .map(|(field, value)| {
                                let observation = Observation::Ready {
                                    value,
                                    evidence: format!("operator:{task}#{field}"),
                                };
                                (field, observation)
                            })
                            .collect();
                        app.workflow_run_command(Command::Resume {
                            run: run.clone(),
                            observations,
                        });
                    }
                    Err(error) => {
                        status.set(error);
                    }
                }),
        )
    }
}

fn context_button(app: &App, run: Uuid, visit: u64) -> View {
    let app = app.clone();
    control("View recorded context")
        .on_click(move || app.workflow_run_command(Command::InspectContext { run, visit }))
}

fn section() -> View {
    column()
        .w_full()
        .p(12.)
        .gap(8.)
        .rounded(7.)
        .bg(rgb(PANEL))
        .border(1.)
        .border_color(rgb(LINE))
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

fn collect(
    fields: &[(String, ValueType, Signal<String>)],
    optional: bool,
) -> Result<BTreeMap<String, Value>, String> {
    fields
        .iter()
        .filter_map(|(name, ty, value)| {
            let raw = value.get();
            if optional && raw.trim().is_empty() {
                return None;
            }
            let value = match ty {
                ValueType::Text => Some(Value::Text(raw)),
                ValueType::Boolean => raw.trim().parse().ok().map(Value::Boolean),
                ValueType::Integer => raw.trim().parse().ok().map(Value::Integer),
            };
            Some(
                value
                    .map(|value| (name.clone(), value))
                    .ok_or_else(|| format!("{name} must be {}.", type_label(*ty))),
            )
        })
        .collect()
}

fn type_label(kind: ValueType) -> &'static str {
    match kind {
        ValueType::Text => "text",
        ValueType::Boolean => "true or false",
        ValueType::Integer => "a whole number",
    }
}

fn describe(decision: &tod_core::lifecycle::workflow::Decision) -> String {
    use tod_core::lifecycle::workflow::Decision;
    match decision {
        Decision::Take { route, to } => format!("{route} → {to}"),
        Decision::OperatorMove { to, reason } => reason.as_ref().map_or_else(
            || format!("Operator moved to {to}"),
            |reason| format!("Operator moved to {to} · {reason}"),
        ),
        Decision::Complete => "Completed".into(),
        Decision::Blocked => "No route matches".into(),
        Decision::Waiting => "Waiting for observations".into(),
        Decision::Error => "Evaluation error".into(),
        Decision::Conflict { routes } => format!("Multiple routes match · {}", routes.join(", ")),
    }
}

fn display(value: Value) -> String {
    match value {
        Value::Text(value) => value,
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
    }
}
