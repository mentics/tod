//! Built-in data sources and action descriptors for saved workflows.
//! The runner interprets graph structure. Providers resolve names against the
//! task, while action adapters decide how a visit does its work.

use super::retry::{GithubOperation, GithubReadFailure, GithubReads};
use super::{
    Comparison, Condition, Definition, FORMAT_VERSION, Field, Node, NodeKind, Observation,
    Observations, Operand, Revision, Route, Source, Value, ValueType,
};
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use tod_store::{
    fleet::FleetStore,
    github::NodePrRepo,
    outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo},
    review::ReviewRepo,
    verification::VerdictRepo,
};
use uuid::Uuid;

pub const MANUAL: &str = "manual";
pub const TASK_SNAPSHOT: &str = "task_snapshot";
pub const WORKSPACE_SETUP: &str = "workspace_setup";
pub const WORKSPACE_CAPTURE: &str = "workspace_capture";
pub const TASK: &str = "task";
pub const GITHUB: &str = "github";
pub const WORKSPACE: &str = "workspace";
pub const TASK_CHECKS: &str = "task_checks";

pub const ACTIONS: &[(&str, &str)] = &[
    (MANUAL, "Record an action result"),
    (TASK_SNAPSHOT, "Read task data"),
    (WORKSPACE_SETUP, "Prepare implementation workspace"),
    (WORKSPACE_CAPTURE, "Capture clean checkout revision"),
];

const TASK_FIELDS: &[(&str, ValueType)] = &[
    ("title", ValueType::Text),
    ("slug", ValueType::Text),
    ("obligation_count", ValueType::Integer),
    ("unverified_obligations", ValueType::Integer),
    ("failed_obligations", ValueType::Integer),
    ("plan_step_count", ValueType::Integer),
    ("open_review_findings", ValueType::Integer),
];

const GITHUB_FIELDS: &[(&str, ValueType)] = &[
    ("linked_pr_count", ValueType::Integer),
    ("has_valid_pr_links", ValueType::Boolean),
    ("all_merged", ValueType::Boolean),
    ("merge_confirmation", ValueType::Boolean),
    ("all_released", ValueType::Boolean),
    ("head_commit", ValueType::Text),
];

const WORKSPACE_FIELDS: &[(&str, ValueType)] = &[("clean_revision", ValueType::Text)];
const TASK_CHECK_FIELDS: &[(&str, ValueType)] = &[
    ("has_own_requirements", ValueType::Boolean),
    ("requirements_covered_by_plan", ValueType::Boolean),
    ("due_steps_implemented", ValueType::Boolean),
];

pub type ProviderSpec = (
    &'static str,
    &'static str,
    &'static [(&'static str, ValueType)],
);

pub const PROVIDERS: &[ProviderSpec] = &[
    (TASK, "Current task", TASK_FIELDS),
    (GITHUB, "Linked GitHub pull requests", GITHUB_FIELDS),
    (WORKSPACE, "Task checkout", WORKSPACE_FIELDS),
    (
        TASK_CHECKS,
        "Task requirements and plan checks",
        TASK_CHECK_FIELDS,
    ),
    (MANUAL, "Value supplied by an operator", &[]),
];

/// Resolve only the fields the current routes could read. User-supplied facts
/// apply to manual bindings. Built-in and node-output evidence comes from
/// TOD even when the operator supplies a value under the same key.
pub(crate) fn observations_with_github_reads(
    fleet: &FleetStore,
    task: Uuid,
    definition: &Definition,
    node: &str,
    manual: &Observations,
) -> Result<(Observations, GithubReads)> {
    let mut fields = std::collections::BTreeSet::new();
    let Some(current) = definition.nodes.get(node) else {
        bail!("Unknown workflow node {node}.")
    };
    for condition in current
        .routes
        .iter()
        .filter_map(|route| route.when.as_ref())
    {
        collect_fields(condition, &mut fields);
    }
    let mut current = BTreeMap::new();
    let mut task_values = None;
    let mut github_values = None;
    let mut github_reads = GithubReads::default();
    let mut workspace_revision = None;
    for key in fields {
        let Some(Field {
            source: Source::Provider { binding, field },
            ..
        }) = definition.fields.get(key)
        else {
            continue;
        };
        let value = match binding.as_str() {
            MANUAL => manual
                .get(key)
                .cloned()
                .unwrap_or_else(|| waiting("No operator observation for this field.")),
            TASK => {
                if task_values.is_none() {
                    task_values = Some(task_snapshot(fleet, task)?);
                }
                match task_values.as_ref().and_then(|values| values.get(field)) {
                    Some(value) => Observation::Ready {
                        value: value.clone(),
                        evidence: format!("task:{task}#{field}"),
                    },
                    None => error(format!(
                        "The current task provider has no field called {field}."
                    )),
                }
            }
            GITHUB => {
                if github_values.is_none() {
                    github_values = Some(github_snapshot(fleet, task, definition, node)?);
                }
                let (values, reads) = github_values.as_ref().expect("loaded GitHub observations");
                github_reads.succeeded |= reads.succeeded;
                if let Some(failure) = reads.failures.get(field) {
                    github_reads.failures.insert(key.into(), failure.clone());
                }
                values.get(field).cloned().unwrap_or_else(|| {
                    error(format!("The GitHub provider has no field called {field}."))
                })
            }
            WORKSPACE if field == "clean_revision" => workspace_revision
                .get_or_insert_with(|| checkout_revision(fleet, task))
                .clone(),
            WORKSPACE => error(format!(
                "The task checkout provider has no field called {field}."
            )),
            TASK_CHECKS => task_check(fleet, task, field)?,
            other => error(format!("Provider {other} is unavailable.")),
        };
        current.insert(key.to_string(), value);
    }
    Ok((current, github_reads))
}

fn task_check(fleet: &FleetStore, task: Uuid, field: &str) -> Result<Observation> {
    use crate::gate::derived::{
        has_requirements_outcome, plan_implemented_outcome, requirements_traceable_outcome,
    };
    use tod_store::outline::repos::gate::OUTCOME_PASS;
    let check = match field {
        "has_own_requirements" => has_requirements_outcome,
        "requirements_covered_by_plan" => requirements_traceable_outcome,
        "due_steps_implemented" => plan_implemented_outcome,
        _ => return Ok(error(format!("Task checks has no field called {field}."))),
    };
    let result = fleet.read(|conn| check(conn, task))?;
    Ok(Observation::Ready {
        value: Value::Boolean(result.outcome == OUTCOME_PASS),
        evidence: result.detail,
    })
}

/// Consult the task's existing checkout, using the same clean superproject and
/// submodule identity as the lifecycle checks. Looking at a gate never makes a
/// worktree or sandbox, or accepts an operator-supplied replacement value.
pub(crate) fn checkout_revision(fleet: &FleetStore, task: Uuid) -> Observation {
    use tod_store::fleet::provision::{clean_heads_key, launch_cwd_if_made};
    match launch_cwd_if_made(fleet, &task.to_string()) {
        Ok(Some(directory)) => match clean_heads_key(&directory) {
            Ok(Some(revision)) => Observation::Ready {
                evidence: format!("workspace:{task}@{revision}"),
                value: Value::Text(revision),
            },
            Ok(None) => waiting("The task checkout does not have a clean committed revision."),
            Err(cause) => error(format!(
                "Could not read the task checkout revision: {cause:#}"
            )),
        },
        Ok(None) => waiting("The task checkout has not been provisioned."),
        Err(cause) => error(format!("Could not find the task checkout: {cause:#}")),
    }
}

pub(crate) fn workspace_setup(
    fleet: &FleetStore,
    task: Uuid,
) -> Result<crate::gate::DerivedOutcome> {
    use crate::gate::derived::{generate_missing_branch, implementation_setup_outcome};
    let settings = tod_store::TodSettings::load(&tod_store::TodPaths::at(fleet.paths().root()))?;
    generate_missing_branch(fleet, task, &settings.branch_template)?;
    fleet.read(|conn| implementation_setup_outcome(conn, task))
}

fn collect_fields<'a>(condition: &'a Condition, fields: &mut std::collections::BTreeSet<&'a str>) {
    match condition {
        Condition::Compare { left, right, .. } => {
            for operand in [left, right] {
                if let Operand::Field(name) = operand {
                    fields.insert(name);
                }
            }
        }
        Condition::All(children) | Condition::Any(children) => {
            for child in children {
                collect_fields(child, fields);
            }
        }
        Condition::Not(child) => collect_fields(child, fields),
    }
}

pub fn task_snapshot(fleet: &FleetStore, task: Uuid) -> Result<BTreeMap<String, Value>> {
    fleet.read(|conn| {
        let Some(node) = NodeRepo::new(conn).get(task)? else {
            bail!("Task {task} was deleted.")
        };
        let obligations = ObligationRepo::new(conn).list_for_node(task)?;
        let verdicts = VerdictRepo::new(conn).standings(task)?;
        let steps = PlanStepRepo::new(conn).list_for_node(task)?;
        let findings = ReviewRepo::new(conn).list_for_node(task)?;
        Ok(BTreeMap::from([
            ("title".into(), Value::Text(node.title)),
            ("slug".into(), Value::Text(node.slug)),
            ("obligation_count".into(), count(obligations.len())),
            (
                "unverified_obligations".into(),
                count(verdicts.iter().filter(|v| v.is_unchecked()).count()),
            ),
            (
                "failed_obligations".into(),
                count(verdicts.iter().filter(|v| v.is_failed()).count()),
            ),
            ("plan_step_count".into(), count(steps.len())),
            (
                "open_review_findings".into(),
                count(findings.iter().filter(|f| f.is_open()).count()),
            ),
        ]))
    })
}

fn count(len: usize) -> Value {
    Value::Integer(i64::try_from(len).unwrap_or(i64::MAX))
}

fn waiting(reason: impl Into<String>) -> Observation {
    Observation::Waiting {
        reason: reason.into(),
    }
}

fn error(reason: impl Into<String>) -> Observation {
    Observation::Error {
        reason: reason.into(),
    }
}

pub(super) const MALFORMED_PR_LINKS: &str =
    "One or more linked pull requests could not be read as GitHub URLs.";
pub(super) const MISSING_PR_LINKS: &str = "Link a GitHub pull request to this task.";

fn github_snapshot(
    fleet: &FleetStore,
    task: Uuid,
    definition: &Definition,
    node: &str,
) -> Result<(Observations, GithubReads)> {
    let mut metadata = GithubReads::default();
    let links = fleet.read(|conn| NodePrRepo::new(conn).read(task))?;
    let source = format!("task:{task}#linked-pull-requests");
    let malformed = (!links.unrecognized.is_empty()).then_some(MALFORMED_PR_LINKS);
    let mut values = BTreeMap::from([(
        "linked_pr_count".into(),
        Observation::Ready {
            value: count(links.prs.len()),
            evidence: source.clone(),
        },
    )]);
    // At Merged, legacy can run the release protocol without a usable set
    // of PR links. This does not supply evidence for a merge or a release.
    values.insert(
        "has_valid_pr_links".into(),
        Observation::Ready {
            value: Value::Boolean(malformed.is_none() && !links.prs.is_empty()),
            evidence: malformed
                .map_or_else(|| source.clone(), |reason| format!("{source}: {reason}")),
        },
    );
    let unavailable = if let Some(reason) = malformed {
        Some(error(reason))
    } else if links.prs.is_empty() {
        Some(waiting(MISSING_PR_LINKS))
    } else {
        None
    };
    if let Some(unavailable) = unavailable {
        values.insert("all_merged".into(), unavailable.clone());
        values.insert("merge_confirmation".into(), unavailable.clone());
        values.insert("all_released".into(), unavailable.clone());
        values.insert("head_commit".into(), unavailable);
        return Ok((values, metadata));
    }

    let mut requested = std::collections::BTreeSet::new();
    for condition in definition.nodes[node]
        .routes
        .iter()
        .filter_map(|route| route.when.as_ref())
    {
        collect_fields(condition, &mut requested);
    }
    let requests = |key| {
        requested.iter().any(|name| {
            matches!(&definition.fields[*name].source,
            Source::Provider { binding, field } if binding == GITHUB && field == key)
        })
    };
    let snapshot_needed = requests("head_commit") || requests("all_merged");
    let merge_needed = requests("merge_confirmation");
    let release_needed = requests("all_released");
    if !snapshot_needed && !merge_needed && !release_needed {
        return Ok((values, metadata));
    }
    let Some(feed) = crate::pr_readiness::feed_for(fleet.paths().root()) else {
        for field in [
            "head_commit",
            "all_merged",
            "merge_confirmation",
            "all_released",
        ] {
            values.insert(
                field.into(),
                waiting("Configure GitHub access to read pull request status."),
            );
        }
        return Ok((values, metadata));
    };
    if merge_needed {
        values.insert(
            "merge_confirmation".into(),
            shipping_observation(
                fleet,
                feed.as_ref(),
                &links.prs,
                "merge_confirmation",
                &mut metadata,
            ),
        );
    }
    if release_needed {
        values.insert(
            "all_released".into(),
            shipping_observation(
                fleet,
                feed.as_ref(),
                &links.prs,
                "all_released",
                &mut metadata,
            ),
        );
    }
    if !snapshot_needed {
        return Ok((values, metadata));
    }

    let read = links
        .prs
        .iter()
        .map(|pr| {
            feed.snapshot(pr)
                .map_err(|cause| format!("Could not read {}: {cause}", pr.url))
        })
        .collect::<std::result::Result<Vec<_>, _>>();
    match read {
        Ok(snapshots) => {
            metadata.succeeded = true;
            let evidence = snapshots
                .iter()
                .zip(&links.prs)
                .map(|(snapshot, pr)| {
                    format!(
                        "{}@{}",
                        pr.url,
                        snapshot.status.head_sha.as_deref().unwrap_or("unknown")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let head = match snapshots.as_slice() {
                [snapshot] => snapshot.status.head_sha.as_ref().map_or_else(
                    || waiting("GitHub has not supplied a head commit."),
                    |head| Observation::Ready {
                        value: Value::Text(head.clone()),
                        evidence: evidence.clone(),
                    },
                ),
                _ => waiting("The head commit field requires exactly one linked pull request."),
            };
            values.insert("head_commit".into(), head);
            values.insert(
                "all_merged".into(),
                Observation::Ready {
                    value: Value::Boolean(snapshots.iter().all(|snapshot| snapshot.status.merged)),
                    evidence,
                },
            );
        }
        Err(message) => {
            for field in ["head_commit", "all_merged"] {
                values.insert(
                    field.into(),
                    github_failure(
                        &mut metadata,
                        field,
                        GithubOperation::PullRequests,
                        message.clone(),
                    ),
                );
            }
        }
    }
    Ok((values, metadata))
}

fn shipping_observation(
    fleet: &FleetStore,
    feed: &dyn crate::pr_readiness::PrFeed,
    prs: &[tod_store::github::NodePr],
    field: &str,
    metadata: &mut GithubReads,
) -> Observation {
    use crate::release_watch::{self, Inclusion};
    let settings = if field == "all_released" {
        match tod_store::TodSettings::load(&tod_store::TodPaths::at(fleet.paths().root())) {
            Ok(settings) => Some(settings.lifecycle),
            Err(cause) => return error(format!("Could not read release policy: {cause:#}")),
        }
    } else {
        None
    };
    let mut evidence = Vec::new();
    let mut pending = None;
    let mut standing = Vec::new();
    for pr in prs {
        let label = crate::pr_readiness::pr_label(pr);
        let merge = match feed.merge(pr) {
            Ok(merge) => {
                metadata.succeeded = true;
                if !merge.merged {
                    standing.push(format!("{label} not merged"));
                    pending.get_or_insert_with(|| format!("{} to be merged", pr.url));
                    continue;
                }
                merge
            }
            Err(cause) => {
                return github_failure(
                    metadata,
                    field,
                    if settings.is_some() {
                        GithubOperation::Releases
                    } else {
                        GithubOperation::PullRequests
                    },
                    format!("{label} not readable: {cause}"),
                );
            }
        };
        let Some(settings) = &settings else {
            standing.push(format!("{label} merged"));
            evidence.push(match merge.merged_at {
                Some(at) => format!("{} merged {at}", pr.url),
                None => pr.url.clone(),
            });
            continue;
        };
        match release_watch::inclusion(feed, pr, &merge, settings.profile_for(&pr.owner, &pr.repo))
        {
            Ok(Inclusion::MergedWithoutRelease) => {
                standing.push(format!("{label} merged (this repository does not release)"));
                evidence.push(pr.url.clone());
            }
            Ok(Inclusion::Published(release)) => {
                standing.push(format!("{label} released in {}", release.tag));
                evidence.push(release.url);
            }
            Ok(Inclusion::Pending) => {
                standing.push(format!("{label} merged, no release names it yet"));
                pending.get_or_insert_with(|| format!("a release whose notes name {}", pr.url));
            }
            Err(cause) => {
                return github_failure(
                    metadata,
                    field,
                    GithubOperation::Releases,
                    format!("releases of {label} not readable: {cause}"),
                );
            }
        }
    }
    if let Some(what) = pending {
        return waiting(if standing.len() > 1 {
            format!("{what} ({})", standing.join("; "))
        } else {
            what
        });
    }
    Observation::Ready {
        value: Value::Boolean(true),
        evidence: evidence.join(", "),
    }
}

fn github_failure(
    metadata: &mut GithubReads,
    field: &str,
    operation: GithubOperation,
    message: String,
) -> Observation {
    metadata.failures.insert(
        field.into(),
        GithubReadFailure {
            operation,
            message: message.clone(),
        },
    );
    error(message)
}

/// An editable example that works with the generic runner. A person or an
/// external adapter records each action's typed result before the graph moves.
pub fn review_workflow() -> Definition {
    let manual = || NodeKind::Action {
        action: Revision {
            id: MANUAL.into(),
            number: 1,
        },
    };
    let route = |id: &str, to: &str, when| Route {
        id: id.into(),
        to: to.into(),
        when,
    };
    let node = |label: &str, kind, outputs, routes| Node {
        label: label.into(),
        kind,
        outputs,
        routes,
    };
    let field = |id: &str| Operand::Field(id.into());
    let compare = |left, op, right| Condition::Compare { left, op, right };
    let result = |field: &str, value_type| Field {
        value_type,
        source: Source::NodeOutput {
            node: "review".into(),
            field: field.into(),
        },
    };
    let zero = Operand::Literal(Value::Integer(0));
    Definition {
        task_phases: BTreeMap::new(),
        adoption_points: BTreeMap::new(),
        format: FORMAT_VERSION,
        revision: Revision {
            id: String::new(),
            number: 1,
        },
        entry: "review".into(),
        fields: BTreeMap::from([
            (
                "open_findings".into(),
                result("open_findings", ValueType::Integer),
            ),
            (
                "reviewed_commit".into(),
                result("reviewed_commit", ValueType::Text),
            ),
            (
                "current_commit".into(),
                Field {
                    value_type: ValueType::Text,
                    source: Source::Provider {
                        binding: MANUAL.into(),
                        field: "current_commit".into(),
                    },
                },
            ),
        ]),
        nodes: BTreeMap::from([
            (
                "review".into(),
                node(
                    "Run review",
                    manual(),
                    BTreeMap::from([
                        ("open_findings".into(), ValueType::Integer),
                        ("reviewed_commit".into(), ValueType::Text),
                    ]),
                    vec![route("classify", "decision", None)],
                ),
            ),
            (
                "decision".into(),
                node(
                    "Review result",
                    NodeKind::Decision,
                    BTreeMap::new(),
                    vec![
                        route(
                            "accept",
                            "done",
                            Some(Condition::All(vec![
                                compare(field("open_findings"), Comparison::Equal, zero.clone()),
                                compare(
                                    field("reviewed_commit"),
                                    Comparison::Equal,
                                    field("current_commit"),
                                ),
                            ])),
                        ),
                        route(
                            "rework",
                            "fix",
                            Some(compare(field("open_findings"), Comparison::Greater, zero)),
                        ),
                    ],
                ),
            ),
            (
                "fix".into(),
                node(
                    "Fix findings",
                    manual(),
                    BTreeMap::new(),
                    vec![route("review_again", "review", None)],
                ),
            ),
            (
                "done".into(),
                node("Finished", NodeKind::Finish, BTreeMap::new(), vec![]),
            ),
        ]),
    }
}

/// A runnable copy of Default's forward path. Completing a manual visit records
/// the operator's confirmation for that phase. The merge and release gates read
/// all linked pull requests. The operator records verification and redesign
/// outcomes. Existing assignments retain their legacy automation.
pub fn default_template() -> Definition {
    let mut definition = review_workflow();
    definition
        .fields
        .get_mut("current_commit")
        .expect("review input")
        .source = Source::Provider {
        binding: WORKSPACE.into(),
        field: "clean_revision".into(),
    };
    definition.entry = "proposed".into();
    let phases = &super::super::flow::PHASES;
    for phase in phases {
        if phase.id == "review" || phase.id == "done" {
            continue;
        }
        let automatic_gate = phase.id == "approved";
        let gate = match phase.id {
            "proposed" => Some("has_own_requirements"),
            "planning" => Some("requirements_covered_by_plan"),
            "active" => Some("due_steps_implemented"),
            "approved" => Some("all_linked_prs_merged"),
            "merged" => Some("all_linked_prs_released"),
            _ => None,
        };
        let next = super::super::flow::next_phase(phase.id).expect("nonterminal phase");
        definition.nodes.insert(
            phase.id.into(),
            Node {
                label: tod_store::settings::lifecycle_display_name(phase.id),
                kind: if automatic_gate {
                    NodeKind::Decision
                } else {
                    NodeKind::Action {
                        action: Revision {
                            id: if phase.id == "ready" {
                                WORKSPACE_SETUP
                            } else {
                                MANUAL
                            }
                            .into(),
                            number: 1,
                        },
                    }
                },
                outputs: BTreeMap::new(),
                routes: vec![Route {
                    id: format!("after_{}", phase.id),
                    to: next.id.into(),
                    when: gate.map(|field| Condition::Compare {
                        left: Operand::Field(field.into()),
                        op: Comparison::Equal,
                        right: Operand::Literal(Value::Boolean(true)),
                    }),
                }],
            },
        );
    }
    for &(field, value_type) in TASK_CHECK_FIELDS {
        definition.fields.insert(
            field.into(),
            Field {
                value_type,
                source: Source::Provider {
                    binding: TASK_CHECKS.into(),
                    field: field.into(),
                },
            },
        );
    }
    definition.fields.insert(
        "all_linked_prs_merged".into(),
        Field {
            value_type: ValueType::Boolean,
            source: Source::Provider {
                binding: GITHUB.into(),
                field: "merge_confirmation".into(),
            },
        },
    );
    definition.fields.insert(
        "all_linked_prs_released".into(),
        Field {
            value_type: ValueType::Boolean,
            source: Source::Provider {
                binding: GITHUB.into(),
                field: "all_released".into(),
            },
        },
    );
    let boolean_result = |name: &str, node: &str| {
        (
            name.to_owned(),
            Field {
                value_type: ValueType::Boolean,
                source: Source::NodeOutput {
                    node: node.into(),
                    field: name.into(),
                },
            },
        )
    };
    let test = |name: &str, value| Condition::Compare {
        left: Operand::Field(name.into()),
        op: Comparison::Equal,
        right: Operand::Literal(Value::Boolean(value)),
    };
    let verify = definition.nodes.get_mut("verifying").expect("verify phase");
    verify.outputs.insert("verified".into(), ValueType::Boolean);
    verify.routes.insert(
        0,
        Route {
            id: "verification_failed".into(),
            to: "active".into(),
            when: Some(test("verified", false)),
        },
    );
    definition.fields.extend([
        boolean_result("verified", "verifying"),
        boolean_result("needs_redesign", "review"),
    ]);
    definition
        .nodes
        .get_mut("review")
        .expect("review action")
        .outputs
        .insert("needs_redesign".into(), ValueType::Boolean);
    let decision = definition.nodes.get_mut("decision").expect("review gate");
    decision.routes[0].to = "pr".into();
    decision.routes.push(Route {
        id: "rereview_changed_commit".into(),
        to: "review".into(),
        when: Some(Condition::All(vec![
            Condition::Compare {
                left: Operand::Field("open_findings".into()),
                op: Comparison::Equal,
                right: Operand::Literal(Value::Integer(0)),
            },
            Condition::Compare {
                left: Operand::Field("reviewed_commit".into()),
                op: Comparison::NotEqual,
                right: Operand::Field("current_commit".into()),
            },
        ])),
    });
    for route in &mut decision.routes {
        let prior = route.when.take().expect("review condition");
        route.when = Some(Condition::All(vec![test("needs_redesign", false), prior]));
    }
    decision.routes.push(Route {
        id: "redesign".into(),
        to: "design".into(),
        when: Some(test("needs_redesign", true)),
    });
    definition
}

/// Default with managed task protocols. Routing stays in the graph, and the
/// task phase projection exists for protocols and integrations that consume
/// the native task-phase records.
pub fn automated_default() -> Definition {
    use super::native;
    let mut graph = default_template();
    let mut task_phases: BTreeMap<String, String> = graph
        .nodes
        .keys()
        .map(|id| (id.clone(), id.clone()))
        .collect();
    for id in ["decision", "fix"] {
        task_phases.insert(id.into(), "review".into());
    }
    let condition = |name: &str, value| Condition::Compare {
        left: Operand::Field(name.into()),
        op: Comparison::Equal,
        right: Operand::Literal(value),
    };
    let route = |id: &str, to: &str, when| Route {
        id: id.into(),
        to: to.into(),
        when,
    };
    let output_field = |node: &str, name: &str, value_type| Field {
        value_type,
        source: Source::NodeOutput {
            node: node.into(),
            field: name.into(),
        },
    };

    // Release evidence must be available before the Merged phase starts its
    // agent work. Both nodes share one Merged stay.
    let awaiting_release = graph.nodes.get_mut("merged").expect("merged phase").routes[0]
        .when
        .take();
    graph.fields.insert(
        "has_valid_pr_links".into(),
        Field {
            value_type: ValueType::Boolean,
            source: Source::Provider {
                binding: GITHUB.into(),
                field: "has_valid_pr_links".into(),
            },
        },
    );
    graph.nodes.get_mut("approved").expect("merge gate").routes[0].to = "release_evidence".into();
    graph.nodes.insert(
        "release_evidence".into(),
        Node {
            label: "Wait for release evidence".into(),
            kind: NodeKind::Decision,
            outputs: BTreeMap::new(),
            routes: vec![route(
                "release_observed",
                "merged",
                Some(Condition::Any(vec![
                    condition("has_valid_pr_links", Value::Boolean(false)),
                    awaiting_release.expect("release condition"),
                ])),
            )],
        },
    );
    task_phases.insert("release_evidence".into(), "merged".into());

    let capture = "capture_review";
    task_phases.insert(capture.into(), "review".into());
    for node in graph.nodes.values_mut() {
        for route in &mut node.routes {
            if route.to == "review" {
                route.to = capture.into();
            }
        }
    }
    graph.nodes.insert(
        capture.into(),
        Node {
            label: "Capture review input".into(),
            kind: NodeKind::Action {
                action: native::revision(native::REVIEW_INPUT),
            },
            outputs: BTreeMap::from([("revision".into(), ValueType::Text)]),
            routes: vec![route("review_captured_revision", "review", None)],
        },
    );
    graph.fields.insert(
        "reviewed_commit".into(),
        output_field(capture, "revision", ValueType::Text),
    );
    graph.fields.remove("needs_redesign");
    let review = graph.nodes.get_mut("review").expect("review");
    review.outputs.remove("needs_redesign");
    review.outputs.remove("reviewed_commit");
    review
        .outputs
        .insert("review_clear".into(), ValueType::Boolean);
    graph.fields.insert(
        "review_clear".into(),
        output_field("review", "review_clear", ValueType::Boolean),
    );
    let current = Operand::Field("current_commit".into());
    let reviewed = Operand::Field("reviewed_commit".into());
    let clean = condition("review_clear", Value::Boolean(true));
    let zero = Operand::Literal(Value::Integer(0));
    graph
        .nodes
        .get_mut("decision")
        .expect("review decision")
        .routes = vec![
        route(
            "accept",
            "pr",
            Some(Condition::All(vec![
                clean.clone(),
                condition("open_findings", Value::Integer(0)),
                Condition::Compare {
                    left: reviewed.clone(),
                    op: Comparison::Equal,
                    right: current.clone(),
                },
            ])),
        ),
        route(
            "rework",
            "fix",
            Some(Condition::Compare {
                left: Operand::Field("open_findings".into()),
                op: Comparison::Greater,
                right: zero,
            }),
        ),
        route(
            "rereview_changed_commit",
            capture,
            Some(Condition::All(vec![
                clean,
                condition("open_findings", Value::Integer(0)),
                Condition::Compare {
                    left: reviewed,
                    op: Comparison::NotEqual,
                    right: current,
                },
            ])),
        ),
        route(
            "rereview_incomplete_round",
            capture,
            Some(Condition::All(vec![
                condition("review_clear", Value::Boolean(false)),
                condition("open_findings", Value::Integer(0)),
            ])),
        ),
    ];

    for (id, action) in [
        ("proposed", native::PHASE),
        ("design", native::PHASE),
        ("planning", native::PHASE),
        ("active", native::IMPLEMENT),
        ("verifying", native::VERIFY),
        (capture, native::REVIEW_INPUT),
        ("review", native::REVIEW),
        ("fix", native::FIX),
        ("pr", native::OPEN_PR),
        ("pr_draft", native::TEND_PR),
        ("pr_review", native::TEND_PR),
        ("merged", native::PHASE),
        ("released", native::PHASE),
        ("learn", native::PHASE),
    ] {
        let node = graph.nodes.get_mut(id).expect("native node");
        node.kind = NodeKind::Action {
            action: native::revision(action),
        };
        let phase = &task_phases[id];
        let targets: Vec<&str> = ["proposed", "design", "planning", "active"]
            .into_iter()
            .take_while(|target| *target != phase)
            .collect();
        if targets.is_empty() {
            continue;
        }
        let field = format!("{id}_redirect");
        node.outputs.insert("redirect".into(), ValueType::Text);
        graph
            .fields
            .insert(field.clone(), output_field(id, "redirect", ValueType::Text));
        let normal = condition(&field, Value::Text(String::new()));
        for route in &mut node.routes {
            route.when = Some(match route.when.take() {
                Some(when) => Condition::All(vec![normal.clone(), when]),
                None if id == "verifying" => Condition::All(vec![
                    normal.clone(),
                    condition("verified", Value::Boolean(true)),
                ]),
                None => normal.clone(),
            });
        }
        for target in targets {
            node.routes.push(route(
                &format!("{id}_redirect_to_{target}"),
                target,
                Some(condition(&field, Value::Text(target.into()))),
            ));
        }
    }
    graph.adoption_points = phases_for_adoption()
        .map(|(phase, node)| (phase.to_owned(), node.to_owned()))
        .collect();
    graph.task_phases = task_phases;
    graph
}

fn phases_for_adoption() -> impl Iterator<Item = (&'static str, &'static str)> {
    super::super::flow::PHASES.iter().map(|phase| {
        (phase.id, match phase.id {
            "review" => "capture_review",
            "merged" => "release_evidence",
            other => other,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        autopilot::{Autopilot, Budget, Outcome},
        conversation::driver::{AgentAccess, ConversationConfig},
        interview::test_support::fixture,
        lifecycle::workflow::{
            catalog::Catalog,
            execute,
            runner::{RunStatus, Runner},
        },
        media::MediaPaths,
        pr_readiness::{PrFeed, set_feed_override},
    };
    use std::{
        path::{Path, PathBuf},
        process::Command,
        sync::Arc,
    };
    use tod_agent::{AgentLaunchOptions, AgentPlatform, AgentProvider};
    use tod_store::{
        fleet::{FilesPlace, places},
        github::{NodePr, PrMerge, PrSnapshot, PrStatus, Release},
        outline::{Capability, OutlineMutation},
    };

    struct Github {
        merged: bool,
        included_in_release: bool,
    }

    struct NoTurns;

    impl AgentAccess for NoTurns {
        fn with<R>(&mut self, _: impl FnOnce(&mut dyn AgentProvider) -> R) -> R {
            panic!("The workflow should wait without starting a provider.");
        }
    }

    impl PrFeed for Github {
        fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
            Ok(PrSnapshot {
                status: PrStatus {
                    merged: self.merged,
                    ..Default::default()
                },
                threads: Vec::new(),
                comments: Vec::new(),
                reviews: Vec::new(),
            })
        }

        fn merge(&self, _: &NodePr) -> Result<PrMerge, String> {
            Ok(PrMerge {
                title: "A task change".into(),
                merged: self.merged,
                merged_at: Some("2026-11-20T12:00:00Z".into()),
            })
        }

        fn releases(&self, _: &NodePr, _: &PrMerge) -> Result<Vec<Release>, String> {
            Ok(vec![Release {
                tag: "v2".into(),
                name: None,
                body: if self.included_in_release {
                    "A task change"
                } else {
                    "Other work"
                }
                .into(),
                url: "https://github.com/tod-test/workflow/releases/tag/v2".into(),
                published_at: Some("2026-11-20T13:00:00Z".into()),
            }])
        }
    }

    fn git(path: &Path, args: &[&str]) -> String {
        let result = Command::new("git")
            .args([
                "-c",
                "user.name=TOD Test",
                "-c",
                "user.email=tod@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().into()
    }

    #[test]
    fn default_graph_requires_a_clean_checkout_merge_and_release() {
        let fx = fixture();
        let repo = fx.root.join("checkout");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let first = git(&repo, &["rev-parse", "HEAD"]);
        fx.outline(OutlineMutation::EnableCapabilities {
            node_id: fx.node,
            capabilities: vec![Capability::Files, Capability::Ticket],
        });
        fx.fleet.writer().flush().unwrap();
        fx.outline(OutlineMutation::SetNodeTicket {
            node_id: fx.node,
            ticket: None,
            linked_prs: vec!["https://github.com/tod-test/workflow/pull/1".into()],
        });
        fx.fleet.writer().flush().unwrap();
        places::pick_matching(
            &fx.fleet,
            &fx.node.to_string(),
            &FilesPlace {
                repo: Some(repo.display().to_string()),
                use_worktree: false,
                ..Default::default()
            },
        )
        .unwrap();
        fx.fleet.writer().flush().unwrap();

        let catalog = Catalog::new(&fx.fleet);
        let draft = catalog.create("Default", default_template()).unwrap();
        let published = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap();
        let runner = Runner::new(&fx.fleet);
        let run = runner
            .assign(fx.node, &published.definition.revision, 100)
            .unwrap();
        let run_id = run.id;
        let resume = |run| execute::resume(&fx.fleet, run, &Observations::new()).unwrap();
        let advance = |progress: execute::Progress, node, values| {
            let request = progress.pending_action.unwrap();
            assert_eq!(request.node, node);
            execute::record_manual(&fx.fleet, &request, values, vec![]).unwrap();
            resume(runner.get(run_id).unwrap().unwrap())
        };
        let mut progress = resume(run);
        progress = advance(progress, "proposed", BTreeMap::new());
        assert_eq!(progress.run.status, RunStatus::Blocked);
        let obligation = fx.obligation("Record implementation evidence");
        fx.fleet.writer().flush().unwrap();
        progress = resume(progress.run);
        progress = advance(progress, "design", BTreeMap::new());
        progress = advance(progress, "planning", BTreeMap::new());
        assert_eq!(progress.run.status, RunStatus::Blocked);
        let step = Uuid::new_v4();
        fx.outline(OutlineMutation::CreatePlanStep {
            step_id: Some(step),
            node_id: fx.node,
            after_id: None,
            before: false,
            body: "Implement the required change".into(),
        });
        fx.fleet.writer().flush().unwrap();
        fx.outline(OutlineMutation::LinkPlanStepObligation {
            step_id: step,
            obligation_id: obligation,
        });
        fx.fleet.writer().flush().unwrap();
        progress = resume(progress.run);
        assert_eq!(progress.run.node, "ready");
        assert!(
            progress
                .stopped
                .as_deref()
                .is_some_and(|reason| reason.contains("Agent capability"))
        );
        fx.outline(OutlineMutation::EnableCapabilities {
            node_id: fx.node,
            capabilities: vec![Capability::Agent],
        });
        fx.fleet.writer().flush().unwrap();
        progress = resume(progress.run);
        let ready = runner
            .visits(run_id)
            .unwrap()
            .into_iter()
            .find(|visit| visit.node == "ready")
            .unwrap();
        assert!(ready.output.as_ref().unwrap().evidence[0].contains("Agent from"));
        progress = advance(progress, "active", BTreeMap::new());
        assert_eq!(progress.run.status, RunStatus::Blocked);
        fx.outline(OutlineMutation::UpdatePlanStepStatus {
            step_id: step,
            status: tod_store::outline::repos::plan_steps::STATUS_IMPLEMENTED.into(),
            note: None,
            reason: None,
        });
        fx.fleet.writer().flush().unwrap();
        progress = resume(progress.run);
        progress = advance(
            progress,
            "verifying",
            BTreeMap::from([("verified".into(), Value::Boolean(true))]),
        );

        let review = |revision| {
            BTreeMap::from([
                ("reviewed_commit".into(), Value::Text(revision)),
                ("open_findings".into(), Value::Integer(0)),
                ("needs_redesign".into(), Value::Boolean(false)),
            ])
        };
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "new head"]);
        let current = git(&repo, &["rev-parse", "HEAD"]);
        progress = advance(progress, "review", review(first));
        assert_eq!(progress.pending_action.as_ref().unwrap().node, "review");

        let dirty = repo.join("untracked");
        std::fs::write(&dirty, "dirty").unwrap();
        progress = advance(progress, "review", review(current.clone()));
        assert_eq!(progress.run.status, RunStatus::Waiting);
        assert_eq!(progress.run.node, "decision");
        let bogus = BTreeMap::from([(
            "current_commit".into(),
            Observation::Ready {
                value: Value::Text(current.clone()),
                evidence: "operator".into(),
            },
        )]);
        progress = execute::resume(&fx.fleet, progress.run, &bogus).unwrap();
        assert_eq!(progress.run.status, RunStatus::Waiting);
        std::fs::remove_file(dirty).unwrap();

        let github = |merged, included_in_release| {
            set_feed_override(Some(Arc::new(Github {
                merged,
                included_in_release,
            })))
        };
        github(false, false);
        progress = resume(progress.run);
        for phase in ["pr", "pr_draft", "pr_review"] {
            progress = advance(progress, phase, BTreeMap::new());
        }
        assert_eq!(progress.run.node, "approved");
        assert_eq!(progress.run.status, RunStatus::Waiting);
        assert!(progress.pending_action.is_none());
        let config = ConversationConfig {
            data_root: fx.root.clone(),
            media: MediaPaths::from_media_root(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tod/media"),
            )
            .unwrap(),
            launch: AgentLaunchOptions::for_platform(AgentPlatform::default()),
            settings_path: None,
            context: Default::default(),
        };
        let mut pilot = Autopilot::new(config, fx.node, Budget::default()).unwrap();
        let outcome = pilot.run(&fx.fleet, &mut NoTurns).unwrap();
        assert!(matches!(
            outcome,
            Outcome::Waiting { what, due_at_ms }
                if what.contains("to be merged") && due_at_ms > chrono::Utc::now().timestamp_millis()
        ));
        progress.run = runner.get(run_id).unwrap().unwrap();
        github(true, false);
        progress = resume(progress.run);
        progress = advance(progress, "merged", BTreeMap::new());
        assert_eq!(progress.run.status, RunStatus::Waiting);
        assert_eq!(progress.run.node, "merged");
        let outcome = pilot.run(&fx.fleet, &mut NoTurns).unwrap();
        assert!(matches!(
            outcome,
            Outcome::Waiting { what, due_at_ms }
                if what.contains("a release whose notes name") && due_at_ms > chrono::Utc::now().timestamp_millis()
        ));
        progress.run = runner.get(run_id).unwrap().unwrap();
        github(true, true);
        progress = resume(progress.run);
        set_feed_override(None);
        for phase in ["released", "learn"] {
            progress = advance(progress, phase, BTreeMap::new());
        }
        assert_eq!(progress.run.status, RunStatus::Complete);

        let trace = runner.traces(run_id).unwrap();
        assert!(trace.iter().any(|entry| matches!(
            entry.trace.observations.get("current_commit"),
            Some(Observation::Ready { value: Value::Text(value), evidence })
                if value == &current && evidence == &format!("workspace:{}@{current}", fx.node)
        )));
        assert!(trace.iter().any(|entry| matches!(
            entry.trace.observations.get("all_linked_prs_released"),
            Some(Observation::Ready { value: Value::Boolean(true), evidence })
                if evidence.ends_with("/releases/tag/v2")
        )));
    }
}
