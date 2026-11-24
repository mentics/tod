use super::*;

fn revision(id: &str, number: u64) -> Revision {
    Revision {
        id: id.into(),
        number,
    }
}

fn node(label: &str, kind: NodeKind, routes: Vec<Route>) -> Node {
    Node {
        label: label.into(),
        kind,
        outputs: BTreeMap::new(),
        routes,
    }
}

fn route(id: &str, to: &str, when: Option<Condition>) -> Route {
    Route {
        id: id.into(),
        to: to.into(),
        when,
    }
}

fn field(id: &str) -> Operand {
    Operand::Field(id.into())
}

fn compare(left: Operand, op: Comparison, right: Operand) -> Condition {
    Condition::Compare { left, op, right }
}

fn equals(left: Operand, right: Operand) -> Condition {
    compare(left, Comparison::Equal, right)
}

fn ready(value: Value, evidence: &str) -> Observation {
    Observation::Ready {
        value,
        evidence: evidence.into(),
    }
}

fn fixture() -> Definition {
    let mut review = node(
        "Inspect the change",
        NodeKind::Action {
            action: revision("inspect", 3),
        },
        vec![route("classify", "decision", None)],
    );
    review.outputs = BTreeMap::from([
        ("commit".into(), ValueType::Text),
        ("findings".into(), ValueType::Integer),
    ]);
    let decision = node(
        "Decide what is next",
        NodeKind::Decision,
        vec![
            route(
                "accept",
                "done",
                Some(Condition::All(vec![
                    equals(field("reviewed_commit"), field("current_commit")),
                    equals(field("findings"), Operand::Literal(Value::Integer(0))),
                ])),
            ),
            route(
                "repair",
                "repair",
                Some(compare(
                    field("findings"),
                    Comparison::Greater,
                    Operand::Literal(Value::Integer(0)),
                )),
            ),
        ],
    );
    let repair = node(
        "Resolve the findings",
        NodeKind::Action {
            action: revision("repair", 2),
        },
        vec![route("inspect_again", "review", None)],
    );
    let provider = Source::Provider {
        binding: "current_change".into(),
        field: "commit".into(),
    };
    let review_output = |field: &str| Source::NodeOutput {
        node: "review".into(),
        field: field.into(),
    };
    Definition {
        task_phases: BTreeMap::new(),
        adoption_points: BTreeMap::new(),
        format: FORMAT_VERSION,
        revision: revision("change-review", 7),
        entry: "review".into(),
        nodes: BTreeMap::from([
            ("review".into(), review),
            ("decision".into(), decision),
            ("repair".into(), repair),
            ("done".into(), node("Complete", NodeKind::Finish, vec![])),
        ]),
        fields: BTreeMap::from([
            (
                "current_commit".into(),
                Field {
                    value_type: ValueType::Text,
                    source: provider,
                },
            ),
            (
                "reviewed_commit".into(),
                Field {
                    value_type: ValueType::Text,
                    source: review_output("commit"),
                },
            ),
            (
                "findings".into(),
                Field {
                    value_type: ValueType::Integer,
                    source: review_output("findings"),
                },
            ),
        ]),
    }
}

fn snapshot(commit: &str, reviewed: &str, findings: i64) -> Observations {
    BTreeMap::from([
        (
            "current_commit".into(),
            ready(Value::Text(commit.into()), "task-change"),
        ),
        (
            "reviewed_commit".into(),
            ready(Value::Text(reviewed.into()), "review/visit"),
        ),
        (
            "findings".into(),
            ready(Value::Integer(findings), "review/findings"),
        ),
    ])
}

#[test]
fn a_serialized_graph_can_loop_and_explain_each_visit() {
    let original = fixture();
    let definition: Definition =
        serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
    assert_eq!(original, definition);
    let workflow = definition.validate().unwrap();
    let first = snapshot("aaa", "aaa", 2);
    let stale = snapshot("bbb", "aaa", 0);
    let fresh = snapshot("bbb", "bbb", 0);
    let mut current = workflow.entry().to_owned();

    for (visit, to, observations) in [
        (1, "decision", &first),
        (2, "repair", &first),
        (3, "review", &first),
        (4, "decision", &fresh),
    ] {
        let trace = workflow.decide(&current, visit, observations).unwrap();
        let Decision::Take {
            to: destination, ..
        } = trace.decision
        else {
            panic!("{trace:?}")
        };
        assert_eq!(destination, to);
        current = destination;
    }
    assert_eq!(
        workflow.decide(&current, 5, &stale).unwrap().decision,
        Decision::Blocked
    );
    let trace = workflow.decide(&current, 5, &fresh).unwrap();
    assert_eq!(
        trace.decision,
        Decision::Take {
            route: "accept".into(),
            to: "done".into()
        }
    );
    assert_eq!(trace.revision, revision("change-review", 7));
    assert_eq!(trace.observations, fresh);
    assert_eq!(
        trace.routes[0].condition.as_ref().unwrap().children.len(),
        2
    );
    assert_eq!(
        workflow.decide(&trace.node, trace.visit, &trace.observations),
        Some(trace.clone())
    );
    let restored: Trace = serde_json::from_str(&serde_json::to_string(&trace).unwrap()).unwrap();
    assert_eq!(restored, trace);
    assert_eq!(
        workflow.decide("done", 6, &fresh).unwrap().decision,
        Decision::Complete
    );
}

#[test]
fn a_matching_route_waits_for_an_unresolved_competitor() {
    let mut definition = fixture();
    definition
        .nodes
        .get_mut("decision")
        .unwrap()
        .routes
        .push(route(
            "inspect_special",
            "review",
            Some(equals(
                field("special"),
                Operand::Literal(Value::Boolean(true)),
            )),
        ));
    definition.fields.insert(
        "special".into(),
        Field {
            value_type: ValueType::Boolean,
            source: Source::Provider {
                binding: "task".into(),
                field: "special".into(),
            },
        },
    );
    let workflow = definition.validate().unwrap();
    let mut inputs = snapshot("aaa", "aaa", 0);
    let waiting = workflow.decide("decision", 1, &inputs).unwrap();
    assert_eq!(waiting.decision, Decision::Waiting);
    assert!(matches!(
        waiting.observations["special"],
        Observation::Waiting { .. }
    ));
    inputs.insert("special".into(), ready(Value::Boolean(true), "task"));
    assert_eq!(
        workflow.decide("decision", 1, &inputs).unwrap().decision,
        Decision::Conflict {
            routes: vec!["accept".into(), "inspect_special".into()]
        },
    );
}

#[test]
fn otherwise_requires_a_false_result_and_groups_follow_four_state_logic() {
    let mut definition = fixture();
    definition.nodes.get_mut("decision").unwrap().routes = vec![
        route(
            "accept",
            "done",
            Some(Condition::All(vec![
                equals(field("reviewed_commit"), field("current_commit")),
                equals(field("findings"), Operand::Literal(Value::Integer(0))),
            ])),
        ),
        route("repair", "repair", None),
    ];
    let workflow = definition.validate().unwrap();
    let mut inputs = snapshot("aaa", "aaa", 0);
    inputs.insert(
        "current_commit".into(),
        Observation::Error {
            reason: "offline".into(),
        },
    );
    assert_eq!(
        workflow.decide("decision", 6, &inputs).unwrap().decision,
        Decision::Error
    );
    inputs.remove("current_commit");
    assert_eq!(
        workflow.decide("decision", 6, &inputs).unwrap().decision,
        Decision::Waiting
    );
    inputs.insert("findings".into(), ready(Value::Integer(1), "review"));
    assert_eq!(
        workflow.decide("decision", 6, &inputs).unwrap().decision,
        Decision::Take {
            route: "repair".into(),
            to: "repair".into()
        },
    );
}

#[test]
fn any_can_settle_with_a_true_clause_and_not_preserves_waiting() {
    let mut definition = fixture();
    definition.nodes.get_mut("decision").unwrap().routes = vec![
        route(
            "accept",
            "done",
            Some(Condition::Any(vec![
                Condition::Not(Box::new(equals(
                    field("reviewed_commit"),
                    field("current_commit"),
                ))),
                equals(field("findings"), Operand::Literal(Value::Integer(0))),
            ])),
        ),
        route("repair", "repair", None),
    ];
    let workflow = definition.validate().unwrap();
    let mut inputs = snapshot("aaa", "aaa", 0);
    inputs.remove("current_commit");
    let trace = workflow.decide("decision", 3, &inputs).unwrap();
    assert_eq!(
        trace.routes[0].condition.as_ref().unwrap().children[0].state,
        ResultState::Waiting
    );
    assert_eq!(
        trace.decision,
        Decision::Take {
            route: "accept".into(),
            to: "done".into()
        }
    );
}

#[test]
fn invalid_bindings_and_routes_are_reported_at_their_editor_locations() {
    let mut definition = fixture();
    definition
        .fields
        .get_mut("reviewed_commit")
        .unwrap()
        .value_type = ValueType::Integer;
    definition.nodes.get_mut("repair").unwrap().routes[0].to = "missing".into();
    let diagnostics = definition.validate().unwrap_err();
    assert!(
        diagnostics
            .iter()
            .any(|item| item.location == "fields.reviewed_commit")
    );
    assert!(
        diagnostics
            .iter()
            .any(|item| item.location == "nodes.decision.routes.0.0")
    );
    assert!(
        diagnostics
            .iter()
            .any(|item| item.location == "nodes.repair.routes.0")
    );
}

#[test]
fn a_mistyped_observation_produces_an_error_and_retains_its_evidence_reference() {
    let definition = fixture();
    let workflow = definition.validate().unwrap();
    let mut inputs = snapshot("aaa", "aaa", 0);
    inputs.insert(
        "findings".into(),
        ready(Value::Boolean(false), "malformed-evidence"),
    );
    let trace = workflow.decide("decision", 9, &inputs).unwrap();
    assert_eq!(trace.decision, Decision::Error);
    assert!(
        matches!(&trace.observations["findings"], Observation::Error { reason } if reason.contains("malformed-evidence"))
    );
}
