//! Operator entry points for saved lifecycle workflows.

use crate::{Invocation, args::Args};
use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, str::FromStr};
use tod_core::lifecycle::workflow::{
    Definition, Observations, Revision, Value,
    actions::ActionCatalog,
    builtin,
    catalog::{Catalog, Draft},
    execute, improvement, metrics, migration,
    runner::{Run, Runner},
};
use tod_store::{fleet::FleetStore, outline::repos::NodeRepo};
use uuid::Uuid;

const USAGE: &str = "tod-cli lifecycle workflow — author and run saved workflows

    list
    actions
    publish-action --file <ACTION.json> [--action <ID> --revision <N>]
    draft <WORKFLOW>
    create --title <TEXT> [--file <DEFINITION.json> | --template default|automated-default]
    save <WORKFLOW> --generation <N> --title <TEXT> --file <DEFINITION.json>
    publish <WORKFLOW> --generation <N>
    assign <NODE> --workflow <ID> --revision <N> [--max-visits <N>]
    preview-adoption <NODE> --workflow <ID> --revision <N>
    adopt --preview <PREVIEW.json> [--max-visits <N>]
    preview-detach <RUN>
    detach --preview <PREVIEW.json>
    status <NODE>
    history <NODE>
    metrics <NODE>
    resume <NODE> [--observations <OBSERVATIONS.json>]
    complete <NODE> --request <ID> --outputs <VALUES.json> [--evidence <TEXT>]…
    cancel <NODE>
    preview-migration <RUN> --revision <N>
    migrate --preview <PREVIEW.json>
    trace <RUN>
    context <RUN> --visit <N>
    replay <RUN> --file <CANDIDATE.json>
    lesson-subjects <RUN> --sequence <N>
    lesson <RUN> --sequence <N> [--subject <KEY>] --text <TEXT>

Create without a file starts a draft from the Review and Fix example.
Node names can be UUIDs or slugs. Selectively supply operator observations
with resume. The workflow reads built-in and node-output fields itself.
";

pub(super) fn run(inv: Invocation) -> Result<String> {
    let [_, command, args @ ..] = inv.rest.as_slice() else {
        return Ok(USAGE.trim_end().into());
    };
    if command == "help"
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        return Ok(USAGE.trim_end().into());
    }
    crate::operator::refuse_agent_session(
        "saved workflow commands",
        &crate::operator::process_env,
    )?;
    let args = Args::parse(args)?;
    let fleet = tod_core::direct_store::open(&inv.data_root, false)?;
    let catalog = Catalog::new(&fleet);
    let runner = Runner::new(&fleet);
    match command.as_str() {
        "list" => json(&catalog.list()?),
        "actions" => json(&ActionCatalog::new(&fleet).list()?),
        "publish-action" => json(&ActionCatalog::new(&fleet).publish(
            args.get("--action"),
            args.get("--revision").map(parse).transpose()?,
            read_json(args.require("--file")?)?,
        )?),
        "draft" => draft(
            catalog
                .draft(args.target("workflow ID")?)?
                .context("Workflow draft not found.")?,
        ),
        "create" => {
            let definition = match (args.get("--file"), args.get("--template")) {
                (Some(file), None) => read_json(file)?,
                (None, Some("default")) => builtin::default_template(),
                (None, Some("automated-default")) => builtin::automated_default(),
                (None, None) => builtin::review_workflow(),
                _ => bail!("Choose a definition file or the default template."),
            };
            draft(catalog.create(args.require("--title")?, definition)?)
        }
        "save" => draft(catalog.save(
            args.target("workflow ID")?,
            parse(args.require("--generation")?)?,
            args.require("--title")?,
            read_json(args.require("--file")?)?,
        )?),
        "publish" => {
            let published = catalog.publish(
                args.target("workflow ID")?,
                parse(args.require("--generation")?)?,
            )?;
            json(&serde_json::json!({"title": published.title, "definition": published.definition}))
        }
        "assign" => {
            let task = task(&fleet, args.target("task")?)?;
            let revision = Revision {
                id: args.require("--workflow")?.into(),
                number: parse(args.require("--revision")?)?,
            };
            json(&runner.assign(task, &revision, visit_limit(&args)?)?)
        }
        "preview-adoption" => {
            let task = task(&fleet, args.target("task")?)?;
            let revision = Revision {
                id: args.require("--workflow")?.into(),
                number: parse(args.require("--revision")?)?,
            };
            json(&migration::preview_adoption(&fleet, task, &revision)?)
        }
        "adopt" => json(&migration::adopt(
            &fleet,
            &read_json(args.require("--preview")?)?,
            visit_limit(&args)?,
        )?),
        "preview-detach" => json(&migration::preview_detach(
            &fleet,
            parse(args.target("run ID")?)?,
        )?),
        "detach" => {
            let preview: migration::DetachPreview = read_json(args.require("--preview")?)?;
            migration::detach(&fleet, &preview)?;
            json(&runner.get(preview.source.id)?)
        }
        "status" => {
            let run = current(&fleet, &runner, &args)?;
            let visit = runner.current_visit(&run)?;
            json(&serde_json::json!({"run": run, "visit": visit}))
        }
        "history" => json(&runner.history(task(&fleet, args.target("task")?)?)?),
        "metrics" => json(&metrics::for_task(
            &fleet,
            task(&fleet, args.target("task")?)?,
        )?),
        "resume" => {
            let run = current(&fleet, &runner, &args)?;
            let observations: Observations = args
                .get("--observations")
                .map(read_json)
                .transpose()?
                .unwrap_or_default();
            json(&execute::resume(&fleet, run, &observations)?)
        }
        "complete" => {
            let run = current(&fleet, &runner, &args)?;
            let request = runner
                .current_visit(&run)?
                .dispatch
                .context("No action has been dispatched for this visit.")?;
            ensure!(
                request.id == args.require("--request")?,
                "The action request belongs to another visit."
            );
            let values: BTreeMap<String, Value> = read_json(args.require("--outputs")?)?;
            let evidence = args
                .get_all("--evidence")
                .into_iter()
                .map(str::to_owned)
                .collect();
            execute::record_manual(&fleet, &request, values, evidence)?;
            let run = runner.get(run.id)?.context("Workflow run not found.")?;
            json(&execute::resume(&fleet, run, &Observations::new())?)
        }
        "cancel" => {
            let run = current(&fleet, &runner, &args)?;
            runner.cancel(&run)?;
            json(&runner.get(run.id)?)
        }
        "preview-migration" => {
            let id = parse(args.target("run ID")?)?;
            let run = runner.get(id)?.context("Workflow run not found.")?;
            let target = Revision {
                id: run.revision.id,
                number: parse(args.require("--revision")?)?,
            };
            json(&migration::preview(&fleet, id, &target)?)
        }
        "migrate" => json(&migration::apply(
            &fleet,
            &read_json(args.require("--preview")?)?,
        )?),
        "trace" => json(&runner.traces(parse(args.target("run ID")?)?)?),
        "context" => json(&improvement::captured_context(
            &fleet,
            parse(args.target("run ID")?)?,
            parse(args.require("--visit")?)?,
        )?),
        "replay" => {
            let candidate: Definition = read_json(args.require("--file")?)?;
            json(&improvement::compare(
                &fleet,
                parse(args.target("run ID")?)?,
                &candidate,
            )?)
        }
        "lesson-subjects" => {
            let run = runner
                .get(parse(args.target("run ID")?)?)?
                .context("Workflow run not found.")?;
            let sequence: u64 = parse(args.require("--sequence")?)?;
            let entry = runner
                .traces(run.id)?
                .into_iter()
                .find(|entry| entry.sequence == sequence)
                .context("Workflow trace not found.")?;
            let definition = execute::pinned_definition(&fleet, &run)?;
            json(&improvement::subjects(&fleet, &definition, &entry.trace)?)
        }
        "lesson" => json(&improvement::record_subject_lesson(
            &fleet,
            parse(args.target("run ID")?)?,
            parse(args.require("--sequence")?)?,
            args.get("--subject").unwrap_or("node"),
            args.require("--text")?,
        )?),
        other => bail!("Unknown workflow command {other}.\n{USAGE}"),
    }
}

fn visit_limit(args: &Args) -> Result<u32> {
    Ok(args
        .get("--max-visits")
        .map(parse)
        .transpose()?
        .unwrap_or(100))
}

fn current(fleet: &FleetStore, runner: &Runner<'_>, args: &Args) -> Result<Run> {
    runner
        .current(task(fleet, args.target("task")?)?)?
        .context("This task has no workflow run.")
}

fn task(fleet: &FleetStore, name: &str) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(name) {
        return Ok(id);
    }
    fleet
        .read(|conn| NodeRepo::new(conn).get_by_slug(name))?
        .map(|node| node.id)
        .with_context(|| format!("Task {name} was not found."))
}

fn read_json<T: DeserializeOwned>(path: &str) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("Read {path}."))?;
    serde_json::from_slice(&bytes).with_context(|| format!("Parse {path}."))
}

fn parse<T: FromStr>(raw: &str) -> Result<T> {
    raw.parse()
        .map_err(|_| anyhow::anyhow!("Invalid value {raw}."))
}

fn draft(draft: Draft) -> Result<String> {
    json(
        &serde_json::json!({"title": draft.title, "generation": draft.generation, "definition": draft.definition}),
    )
}

fn json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}
