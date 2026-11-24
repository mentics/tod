//! Versioned workflow definitions and traceable evaluation of outgoing routes.
//! Callers supply observations. Evaluation can be replayed without contacting
//! a provider, dispatching an action, or changing a task.

pub mod agent;
pub mod builtin;
pub mod execute;
pub mod improvement;
pub mod metrics;

pub mod runner;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod actions;
pub mod catalog;
pub mod migration;
pub mod native;
pub mod retry;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub id: String,
    pub number: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub format: u32,
    pub revision: Revision,
    pub entry: String,
    pub nodes: BTreeMap<String, Node>,
    #[serde(default)]
    pub fields: BTreeMap<String, Field>,
    /// Optional projection used by actions that still consume native task
    /// lifecycle stays. Several workflow nodes may share one task phase.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub task_phases: BTreeMap<String, String>,
    /// The authored start node for adopting an existing task in a projected
    /// lifecycle phase other than the normal entry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adoption_points: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub label: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueType>,
    #[serde(default)]
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Action { action: Revision },
    Decision,
    Finish,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub to: String,
    /// An omitted condition is the otherwise route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub value_type: ValueType,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    NodeOutput { node: String, field: String },
    Provider { binding: String, field: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Boolean,
    Integer,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Boolean(bool),
    Integer(i64),
    Text(String),
}

impl Value {
    fn value_type(&self) -> ValueType {
        match self {
            Self::Boolean(_) => ValueType::Boolean,
            Self::Integer(_) => ValueType::Integer,
            Self::Text(_) => ValueType::Text,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operand {
    Field(String),
    Literal(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Comparison {
    fn supports(self, left: ValueType, right: ValueType) -> bool {
        left == right
            && (left == ValueType::Integer || matches!(self, Self::Equal | Self::NotEqual))
    }

    fn matches(self, left: &Value, right: &Value) -> bool {
        match (self, left, right) {
            (Self::Equal, _, _) => left == right,
            (Self::NotEqual, _, _) => left != right,
            (Self::Less, Value::Integer(left), Value::Integer(right)) => left < right,
            (Self::LessOrEqual, Value::Integer(left), Value::Integer(right)) => left <= right,
            (Self::Greater, Value::Integer(left), Value::Integer(right)) => left > right,
            (Self::GreaterOrEqual, Value::Integer(left), Value::Integer(right)) => left >= right,
            _ => unreachable!("comparison types have been checked"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Compare {
        left: Operand,
        op: Comparison,
        right: Operand,
    },
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Observation {
    Ready { value: Value, evidence: String },
    Waiting { reason: String },
    Error { reason: String },
}

pub type Observations = BTreeMap<String, Observation>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultState {
    Match,
    NoMatch,
    Waiting,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedCondition {
    pub state: ResultState,
    /// Children retain condition order so the editor can explain each clause.
    pub children: Vec<CheckedCondition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedRoute {
    pub route: String,
    /// The otherwise route has no condition.
    pub condition: Option<CheckedCondition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    Take { route: String, to: String },
    OperatorMove { to: String, reason: Option<String> },
    Complete,
    Blocked,
    Waiting,
    Error,
    Conflict { routes: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub revision: Revision,
    pub node: String,
    pub visit: u64,
    pub observations: Observations,
    pub routes: Vec<CheckedRoute>,
    pub decision: Decision,
    /// Transport recovery associated with this evaluation. Route replay uses
    /// the recorded observations and does not contact the integration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub github_retries: BTreeMap<retry::GithubOperation, retry::GithubRetry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub location: String,
    pub message: String,
}

/// A validated definition held by reference so callers evaluate exactly the
/// revision they validated.
#[derive(Debug)]
pub struct Workflow<'a>(&'a Definition);

impl Definition {
    pub fn validate(&self) -> Result<Workflow<'_>, Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();
        let mut report = |location: &str, message: &str| {
            diagnostics.push(Diagnostic {
                location: location.into(),
                message: message.into(),
            });
        };
        if self.format != FORMAT_VERSION {
            report("format", "Unsupported workflow format.");
        }
        if self.revision.id.trim().is_empty() || self.revision.number == 0 {
            report(
                "revision",
                "Give the workflow an ID and positive revision number.",
            );
        }
        if !self.nodes.contains_key(&self.entry) {
            report("entry", "The entry must name a node.");
        }
        if !self.task_phases.is_empty() {
            for id in self.nodes.keys() {
                if !self.task_phases.get(id).is_some_and(|phase| {
                    tod_store::settings::VALID_LIFECYCLE_STATES.contains(&phase.as_str())
                }) {
                    report(
                        &format!("task_phases.{id}"),
                        "Choose a task phase for this node.",
                    );
                }
            }
            for id in self
                .task_phases
                .keys()
                .filter(|id| !self.nodes.contains_key(*id))
            {
                report(
                    &format!("task_phases.{id}"),
                    "This task phase refers to a missing node.",
                );
            }
        }
        for (phase, node) in &self.adoption_points {
            if !self.nodes.contains_key(node) || self.task_phases.get(node) != Some(phase) {
                report(
                    &format!("adoption_points.{phase}"),
                    "Choose an existing node mapped to this lifecycle phase.",
                );
            }
        }
        for (id, field) in &self.fields {
            let location = format!("fields.{id}");
            if id.trim().is_empty() {
                report(&location, "Give the field an ID.");
            }
            match &field.source {
                Source::NodeOutput {
                    node,
                    field: output,
                } => {
                    if self
                        .nodes
                        .get(node)
                        .and_then(|node| node.outputs.get(output))
                        != Some(&field.value_type)
                    {
                        report(
                            &location,
                            "Bind an existing node output with the same type.",
                        );
                    }
                }
                Source::Provider { binding, field } => {
                    if binding.trim().is_empty() || field.trim().is_empty() {
                        report(&location, "Choose a provider binding and a field.");
                    }
                }
            }
        }

        let mut route_ids = BTreeSet::new();
        for (id, node) in &self.nodes {
            let location = format!("nodes.{id}");
            if id.trim().is_empty() || node.label.trim().is_empty() {
                report(&location, "Give the node an ID and label.");
            }
            if let NodeKind::Action { action } = &node.kind
                && (action.id.trim().is_empty() || action.number == 0)
            {
                report(&location, "Choose a versioned action.");
            }
            if let NodeKind::Action { action } = &node.kind
                && native::is_native(&action.id)
                && (action.number != 1
                    || !self
                        .task_phases
                        .get(id)
                        .is_some_and(|phase| native::phase_supported(&action.id, phase)))
            {
                report(
                    &location,
                    "Choose a compatible task phase and revision for the native action.",
                );
            }
            if let NodeKind::Action { action } = &node.kind {
                for &(name, ty) in native::required_outputs(&action.id) {
                    if node.outputs.get(name) != Some(&ty) {
                        report(
                            &location,
                            &format!("This action requires the {name} output ({ty:?})."),
                        );
                    }
                }
            }
            match (&node.kind, node.routes.is_empty()) {
                (NodeKind::Finish, false) => {
                    report(&location, "Finish nodes have no outgoing routes.")
                }
                (NodeKind::Action { .. } | NodeKind::Decision, true) => {
                    report(&location, "Connect this node to a successor.");
                }
                _ => {}
            }
            if node
                .routes
                .iter()
                .filter(|route| route.when.is_none())
                .count()
                > 1
            {
                report(&location, "Use at most one otherwise route.");
            }
            for (index, route) in node.routes.iter().enumerate() {
                let location = format!("{location}.routes.{index}");
                if route.id.trim().is_empty() || !route_ids.insert(&route.id) {
                    report(&location, "Give the route a unique ID.");
                }
                if !self.nodes.contains_key(&route.to) {
                    report(&location, "Choose an existing destination node.");
                }
                if let Some(condition) = &route.when {
                    self.validate_condition(condition, &location, &mut report);
                }
            }
        }

        let mut reachable = BTreeSet::new();
        let mut pending = vec![self.entry.as_str()];
        while let Some(id) = pending.pop() {
            if let Some(node) = self.nodes.get(id)
                && reachable.insert(id)
            {
                pending.extend(node.routes.iter().map(|route| route.to.as_str()));
            }
        }
        for id in self
            .nodes
            .keys()
            .filter(|id| !reachable.contains(id.as_str()))
        {
            report(
                &format!("nodes.{id}"),
                "Connect this node to the entry path.",
            );
        }
        if !reachable
            .iter()
            .any(|id| self.nodes[*id].kind == NodeKind::Finish)
        {
            report("nodes", "Connect a finish node.");
        }
        if diagnostics.is_empty() {
            Ok(Workflow(self))
        } else {
            Err(diagnostics)
        }
    }

    fn validate_condition(
        &self,
        condition: &Condition,
        location: &str,
        report: &mut impl FnMut(&str, &str),
    ) {
        match condition {
            Condition::Compare { left, op, right } => {
                let operand_type = |operand: &Operand| match operand {
                    Operand::Literal(value) => Some(value.value_type()),
                    Operand::Field(id) => self.fields.get(id).map(|field| field.value_type),
                };
                match (operand_type(left), operand_type(right)) {
                    (Some(left), Some(right)) if op.supports(left, right) => {}
                    (Some(_), Some(_)) => {
                        report(location, "This comparison requires compatible value types.")
                    }
                    _ => report(location, "Choose an existing field."),
                }
            }
            Condition::Not(child) => self.validate_condition(child, location, report),
            Condition::All(children) | Condition::Any(children) => {
                if children.is_empty() {
                    report(location, "Add a clause to the group.");
                }
                for (index, child) in children.iter().enumerate() {
                    self.validate_condition(child, &format!("{location}.{index}"), report);
                }
            }
        }
    }
}

impl Workflow<'_> {
    pub fn entry(&self) -> &str {
        &self.0.entry
    }

    /// Evaluate at the supplied visit. Returns None for an unknown node.
    pub fn decide(&self, node: &str, visit: u64, observations: &Observations) -> Option<Trace> {
        let definition = self.0;
        let at = definition.nodes.get(node)?;
        let mut used = BTreeMap::new();
        let mut fallback = None;
        let mut matching = Vec::new();
        let mut waiting = false;
        let mut error = false;
        let routes = at
            .routes
            .iter()
            .map(|route| {
                let condition = route
                    .when
                    .as_ref()
                    .map(|condition| check(condition, definition, observations, &mut used));
                match condition.as_ref().map(|check| check.state) {
                    Some(ResultState::Match) => matching.push(route),
                    Some(ResultState::Waiting) => waiting = true,
                    Some(ResultState::Error) => error = true,
                    Some(ResultState::NoMatch) => {}
                    None => fallback = Some(route),
                }
                CheckedRoute {
                    route: route.id.clone(),
                    condition,
                }
            })
            .collect();
        let decision = match matching.as_slice() {
            [_, _, ..] => Decision::Conflict {
                routes: matching.iter().map(|route| route.id.clone()).collect(),
            },
            _ if at.kind == NodeKind::Finish => Decision::Complete,
            _ if error => Decision::Error,
            _ if waiting => Decision::Waiting,
            _ => match matching.first().copied().or(fallback) {
                Some(route) => Decision::Take {
                    route: route.id.clone(),
                    to: route.to.clone(),
                },
                None => Decision::Blocked,
            },
        };
        Some(Trace {
            revision: definition.revision.clone(),
            node: node.into(),
            visit,
            observations: used,
            routes,
            decision,
            github_retries: BTreeMap::new(),
        })
    }
}

fn check(
    condition: &Condition,
    definition: &Definition,
    snapshot: &Observations,
    used: &mut Observations,
) -> CheckedCondition {
    use ResultState::*;
    let (state, children) = match condition {
        Condition::Compare { left, op, right } => {
            let left = read(left, definition, snapshot, used);
            let right = read(right, definition, snapshot, used);
            let state = match (left, right) {
                (Ok(left), Ok(right)) if op.matches(&left, &right) => Match,
                (Ok(_), Ok(_)) => NoMatch,
                (Err(Error), _) | (_, Err(Error)) => Error,
                _ => Waiting,
            };
            (state, Vec::new())
        }
        Condition::Not(child) => {
            let child = check(child, definition, snapshot, used);
            let state = match child.state {
                Match => NoMatch,
                NoMatch => Match,
                other => other,
            };
            (state, vec![child])
        }
        Condition::All(conditions) | Condition::Any(conditions) => {
            let children: Vec<_> = conditions
                .iter()
                .map(|child| check(child, definition, snapshot, used))
                .collect();
            let contains = |state| children.iter().any(|child| child.state == state);
            let (settling, otherwise) = if matches!(condition, Condition::All(_)) {
                (NoMatch, Match)
            } else {
                (Match, NoMatch)
            };
            let state = if contains(settling) {
                settling
            } else if contains(Error) {
                Error
            } else if contains(Waiting) {
                Waiting
            } else {
                otherwise
            };
            (state, children)
        }
    };
    CheckedCondition { state, children }
}

fn read(
    operand: &Operand,
    definition: &Definition,
    snapshot: &Observations,
    used: &mut Observations,
) -> Result<Value, ResultState> {
    let id = match operand {
        Operand::Field(id) => id,
        Operand::Literal(value) => return Ok(value.clone()),
    };
    let observation = used
        .entry(id.clone())
        .or_insert_with(|| match snapshot.get(id) {
            Some(Observation::Ready { value, evidence })
                if value.value_type() != definition.fields[id].value_type =>
            {
                Observation::Error {
                    reason: format!(
                        "Value type disagrees with the binding schema. Evidence: {evidence}"
                    ),
                }
            }
            Some(observation) => observation.clone(),
            None => Observation::Waiting {
                reason: "No observation for this field.".into(),
            },
        });
    match observation {
        Observation::Ready { value, .. } => Ok(value.clone()),
        Observation::Waiting { .. } => Err(ResultState::Waiting),
        Observation::Error { .. } => Err(ResultState::Error),
    }
}

#[cfg(test)]
mod tests;
