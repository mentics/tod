//! The text query language the Linear generator's filter is written in.
//!
//! A query is a boolean expression over conditions, e.g.
//!
//! ```text
//! team IS ANY OF (TOD, OPS) AND (assignee IS EMPTY OR priority <= high)
//! ```
//!
//! A condition is `<field> <operator> [<value> | (<value>, ...)]`. `AND`
//! binds tighter than `OR`; parentheses group. Keywords are
//! case-insensitive; a value that is not a plain word is written in double
//! quotes. [`compile`] turns a query into Linear's `IssueFilter` JSON,
//! [`decompile`] turns such JSON back into a query where it can, and
//! [`complete`] says what may come next at a cursor, which is what the
//! editor's dropdown shows.

use crate::IntrospectionCache;
use serde_json::{json, Map, Value};
use std::ops::Range;

/// The generator config key the query text is stored under.
pub const QUERY_KEY: &str = "filter_query";

// ---------------------------------------------------------------------------
// Fields

/// How a field's values are compared, which decides its operators and how
/// a condition on it becomes JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// A related entity matched on one of its string properties: the
    /// comparator sits at `path` under the filter key.
    Relation { path: &'static [&'static str], nullable: bool },
    /// The label collection: `some` / `every` label matched by name.
    Labels,
    Text { nullable: bool },
    Date { nullable: bool },
    Number { nullable: bool },
    /// A number the user writes by name (`urgent`, `high`, ...).
    Priority,
}

/// Where a field's value suggestions come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Values {
    None,
    /// `IntrospectionCache::relation_options[key]`.
    Cache(&'static str),
    Static(&'static [&'static str]),
}

/// A field a condition can be written on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryField {
    /// The name written in the query.
    pub name: String,
    /// The `IssueFilter` key the condition is stored under.
    pub filter_key: String,
    pub kind: FieldKind,
    values: Values,
    pub description: &'static str,
}

/// Priorities from most to least urgent, with Linear's number for each.
/// Linear counts the other way (urgent is 1, low is 4, none is 0), so a
/// comparison in a query is on this order, not on the number.
const PRIORITIES: [(&str, i64); 5] =
    [("urgent", 1), ("high", 2), ("medium", 3), ("low", 4), ("noPriority", 0)];
const PRIORITY_NAMES: [&str; 5] = ["urgent", "high", "medium", "low", "noPriority"];
const STATE_TYPES: [&str; 6] = ["triage", "backlog", "unstarted", "started", "completed", "canceled"];
const RELATIVE_DATES: [&str; 6] = ["-P1D", "-P1W", "-P2W", "-P1M", "-P3M", "-P1Y"];

const fn relation(path: &'static [&'static str], nullable: bool) -> FieldKind {
    FieldKind::Relation { path, nullable }
}

/// The fields the query language knows by name.
const KNOWN_FIELDS: [(&str, &str, FieldKind, Values, &str); 19] = [
    ("team", "team", relation(&["key"], false), Values::Cache("team"), "Team key"),
    ("state", "state", relation(&["name"], false), Values::Cache("state"), "Workflow state"),
    ("stateType", "state", relation(&["type"], false), Values::Static(&STATE_TYPES), "Kind of workflow state"),
    ("assignee", "assignee", relation(&["displayName"], true), Values::Cache("assignee"), "Assigned user"),
    ("creator", "creator", relation(&["displayName"], true), Values::Cache("assignee"), "User who created it"),
    ("project", "project", relation(&["name"], true), Values::Cache("project"), "Project"),
    ("cycle", "cycle", relation(&["name"], true), Values::None, "Cycle"),
    ("labels", "labels", FieldKind::Labels, Values::Cache("labels"), "Labels"),
    ("priority", "priority", FieldKind::Priority, Values::Static(&PRIORITY_NAMES), "Priority"),
    ("estimate", "estimate", FieldKind::Number { nullable: true }, Values::None, "Estimate"),
    ("number", "number", FieldKind::Number { nullable: false }, Values::None, "Issue number"),
    ("title", "title", FieldKind::Text { nullable: false }, Values::None, "Title"),
    ("description", "description", FieldKind::Text { nullable: true }, Values::None, "Description"),
    ("createdAt", "createdAt", FieldKind::Date { nullable: false }, Values::Static(&RELATIVE_DATES), "Created"),
    ("updatedAt", "updatedAt", FieldKind::Date { nullable: false }, Values::Static(&RELATIVE_DATES), "Last updated"),
    ("dueDate", "dueDate", FieldKind::Date { nullable: true }, Values::Static(&RELATIVE_DATES), "Due date"),
    ("startedAt", "startedAt", FieldKind::Date { nullable: true }, Values::Static(&RELATIVE_DATES), "Started"),
    ("completedAt", "completedAt", FieldKind::Date { nullable: true }, Values::Static(&RELATIVE_DATES), "Completed"),
    ("canceledAt", "canceledAt", FieldKind::Date { nullable: true }, Values::Static(&RELATIVE_DATES), "Canceled"),
];

/// Every field a query may use: the known ones, plus any other
/// `IssueFilter` field the introspection cache lists whose comparator type
/// the language understands.
pub fn fields(cache: Option<&IntrospectionCache>) -> Vec<QueryField> {
    let mut fields: Vec<QueryField> = KNOWN_FIELDS
        .iter()
        .map(|(name, key, kind, values, description)| QueryField {
            name: (*name).to_string(),
            filter_key: (*key).to_string(),
            kind: *kind,
            values: *values,
            description,
        })
        .collect();
    for field in cache.map(|c| c.filter_fields.as_slice()).unwrap_or_default() {
        if fields.iter().any(|f| f.filter_key == field.name) {
            continue;
        }
        let nullable = field.field_type.starts_with("Nullable");
        let kind = match field.field_type.trim_start_matches("Nullable") {
            "StringComparator" => FieldKind::Text { nullable },
            "DateComparator" | "TimelessDateComparator" => FieldKind::Date { nullable },
            "NumberComparator" => FieldKind::Number { nullable },
            _ => continue,
        };
        let values = match kind {
            FieldKind::Date { .. } => Values::Static(&RELATIVE_DATES),
            _ => Values::None,
        };
        fields.push(QueryField {
            name: field.name.clone(),
            filter_key: field.name.clone(),
            kind,
            values,
            description: "",
        });
    }
    fields
}

fn find_field<'a>(fields: &'a [QueryField], name: &str) -> Option<&'a QueryField> {
    fields.iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

impl QueryField {
    fn nullable(&self) -> bool {
        match self.kind {
            FieldKind::Relation { nullable, .. }
            | FieldKind::Text { nullable }
            | FieldKind::Date { nullable }
            | FieldKind::Number { nullable } => nullable,
            FieldKind::Labels | FieldKind::Priority => false,
        }
    }

    /// The operators a condition on this field may use, in the order the
    /// dropdown offers them.
    pub fn operators(&self) -> Vec<Op> {
        use Op::*;
        let mut ops = match self.kind {
            FieldKind::Relation { .. } | FieldKind::Labels => vec![Is, IsNot, AnyOf, NoneOf],
            FieldKind::Text { .. } => vec![Contains, NotContains, Is, IsNot, StartsWith],
            FieldKind::Date { .. } => vec![After, Before, Is, Gte, Lte],
            FieldKind::Number { .. } | FieldKind::Priority => {
                vec![Is, IsNot, AnyOf, NoneOf, Lt, Lte, Gt, Gte]
            }
        };
        if self.nullable() {
            ops.extend([IsEmpty, IsNotEmpty]);
        }
        ops
    }

    /// The values to suggest for this field.
    pub fn value_options(&self, cache: Option<&IntrospectionCache>) -> Vec<String> {
        match self.values {
            Values::None => Vec::new(),
            Values::Static(values) => values.iter().map(|v| (*v).to_string()).collect(),
            Values::Cache(key) => cache
                .and_then(|c| c.relation_options.get(key))
                .cloned()
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Operators

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Is,
    IsNot,
    AnyOf,
    NoneOf,
    Contains,
    NotContains,
    StartsWith,
    Before,
    After,
    Lt,
    Lte,
    Gt,
    Gte,
    IsEmpty,
    IsNotEmpty,
}

/// What an operator takes after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    None,
    One,
    List,
}

/// Every way an operator may be written, canonical spelling first.
const SPELLINGS: [(Op, &[&str]); 21] = [
    (Op::Is, &["IS"]),
    (Op::Is, &["="]),
    (Op::IsNot, &["IS", "NOT"]),
    (Op::IsNot, &["!="]),
    (Op::AnyOf, &["IS", "ANY", "OF"]),
    (Op::AnyOf, &["IN"]),
    (Op::NoneOf, &["IS", "NONE", "OF"]),
    (Op::NoneOf, &["NOT", "IN"]),
    (Op::Contains, &["CONTAINS"]),
    (Op::NotContains, &["DOES", "NOT", "CONTAIN"]),
    (Op::StartsWith, &["STARTS", "WITH"]),
    (Op::Before, &["BEFORE"]),
    (Op::After, &["AFTER"]),
    (Op::Lt, &["<"]),
    (Op::Lte, &["<="]),
    (Op::Gt, &[">"]),
    (Op::Gte, &[">="]),
    (Op::IsEmpty, &["IS", "EMPTY"]),
    (Op::IsNotEmpty, &["IS", "NOT", "EMPTY"]),
    (Op::IsEmpty, &["IS", "NULL"]),
    (Op::IsNotEmpty, &["IS", "NOT", "NULL"]),
];

impl Op {
    /// The canonical spelling, e.g. `IS ANY OF`.
    pub fn label(self) -> String {
        SPELLINGS
            .iter()
            .find(|(op, _)| *op == self)
            .map(|(_, words)| words.join(" "))
            .unwrap_or_default()
    }

    pub fn arity(self) -> Arity {
        match self {
            Op::AnyOf | Op::NoneOf => Arity::List,
            Op::IsEmpty | Op::IsNotEmpty => Arity::None,
            _ => Arity::One,
        }
    }

    /// The comparator key this operator is stored as, for fields compared
    /// directly (text, date, number, and a relation's property).
    fn comparator(self) -> &'static str {
        match self {
            Op::Is => "eq",
            Op::IsNot => "neq",
            Op::AnyOf => "in",
            Op::NoneOf => "nin",
            Op::Contains => "containsIgnoreCase",
            Op::NotContains => "notContainsIgnoreCase",
            Op::StartsWith => "startsWithIgnoreCase",
            Op::Before | Op::Lt => "lt",
            Op::After | Op::Gt => "gt",
            Op::Lte => "lte",
            Op::Gte => "gte",
            Op::IsEmpty | Op::IsNotEmpty => "null",
        }
    }
}

/// Words that cannot be a bare value because they read as syntax.
const RESERVED: [&str; 16] = [
    "AND", "OR", "IS", "NOT", "ANY", "NONE", "OF", "IN", "EMPTY", "NULL", "CONTAINS", "DOES",
    "CONTAIN", "STARTS", "BEFORE", "AFTER",
];

// ---------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Comma,
    /// A bare word or an operator symbol.
    Word(String),
    /// A quoted string; `closed` is false while the closing quote is missing.
    Str { text: String, closed: bool },
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    span: Range<usize>,
}

impl Token {
    fn word(&self) -> Option<&str> {
        match &self.tok {
            Tok::Word(w) => Some(w),
            _ => None,
        }
    }

    fn is_keyword(&self, keyword: &str) -> bool {
        self.word().is_some_and(|w| w.eq_ignore_ascii_case(keyword))
    }

    /// The token as a value: a quoted string, or a bare word.
    fn value(&self) -> Option<&str> {
        match &self.tok {
            Tok::Word(w) => Some(w),
            Tok::Str { text, .. } => Some(text),
            _ => None,
        }
    }
}

fn is_word_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '(' | ')' | ',' | '"' | '=' | '!' | '<' | '>')
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let simple = match c {
            '(' => Some(Tok::LParen),
            ')' => Some(Tok::RParen),
            ',' => Some(Tok::Comma),
            _ => None,
        };
        if let Some(tok) = simple {
            tokens.push(Token { tok, span: start..start + 1 });
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        if c == '"' {
            let mut value = String::new();
            let mut end = text.len();
            let mut closed = false;
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        closed = true;
                        break;
                    }
                    c => value.push(c),
                }
            }
            tokens.push(Token { tok: Tok::Str { text: value, closed }, span: start..end });
            continue;
        }
        if matches!(c, '=' | '!' | '<' | '>') {
            let mut end = start + 1;
            if let Some(&(i, '=')) = chars.peek() {
                chars.next();
                end = i + 1;
            }
            tokens.push(Token { tok: Tok::Word(text[start..end].to_string()), span: start..end });
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(i, c)) = chars.peek() {
            if !is_word_char(c) {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        tokens.push(Token { tok: Tok::Word(text[start..end].to_string()), span: start..end });
    }
    tokens
}

/// The spellings that begin with `words` (compared case-insensitively),
/// restricted to `allowed`.
fn spellings_starting_with<'a>(
    words: &'a [&str],
    allowed: &'a [Op],
) -> impl Iterator<Item = (Op, &'static [&'static str])> + 'a {
    SPELLINGS.iter().copied().filter(move |(op, spelling)| {
        allowed.contains(op)
            && spelling.len() >= words.len()
            && spelling.iter().zip(words).all(|(s, w)| s.eq_ignore_ascii_case(w))
    })
}

fn exact_spelling(words: &[&str], allowed: &[Op]) -> Option<Op> {
    spellings_starting_with(words, allowed)
        .find(|(_, spelling)| spelling.len() == words.len())
        .map(|(op, _)| op)
}

// ---------------------------------------------------------------------------
// Parsing

/// A parsed query.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Cond { field: String, op: Op, values: Vec<String> },
}

impl Expr {
    /// How many conditions the expression holds.
    pub fn condition_count(&self) -> usize {
        match self {
            Expr::And(terms) | Expr::Or(terms) => terms.iter().map(Expr::condition_count).sum(),
            Expr::Cond { .. } => 1,
        }
    }
}

/// Why a query does not parse, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    /// Byte range in the query text.
    pub span: Range<usize>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at column {})", self.message, self.span.start + 1)
    }
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    len: usize,
    fields: &'a [QueryField],
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn error(&self, message: impl Into<String>) -> QueryError {
        let span = self.peek().map(|t| t.span.clone()).unwrap_or(self.len..self.len);
        QueryError { message: message.into(), span }
    }

    fn expr(&mut self) -> Result<Expr, QueryError> {
        let mut terms = vec![self.and()?];
        while self.peek().is_some_and(|t| t.is_keyword("OR")) {
            self.pos += 1;
            terms.push(self.and()?);
        }
        Ok(flatten(terms, Expr::Or))
    }

    fn and(&mut self) -> Result<Expr, QueryError> {
        let mut terms = vec![self.primary()?];
        while self.peek().is_some_and(|t| t.is_keyword("AND")) {
            self.pos += 1;
            terms.push(self.primary()?);
        }
        Ok(flatten(terms, Expr::And))
    }

    fn primary(&mut self) -> Result<Expr, QueryError> {
        let Some(token) = self.peek().cloned() else {
            return Err(self.error("Expected a field"));
        };
        if token.tok == Tok::LParen {
            self.pos += 1;
            let inner = self.expr()?;
            if self.peek().map(|t| &t.tok) != Some(&Tok::RParen) {
                return Err(self.error("Expected `)`"));
            }
            self.pos += 1;
            return Ok(inner);
        }
        let Some(name) = token.word() else {
            return Err(self.error("Expected a field"));
        };
        let Some(field) = find_field(self.fields, name) else {
            return Err(self.error(format!("Unknown field `{name}`")));
        };
        self.pos += 1;
        let op = self.operator(field)?;
        let values = match op.arity() {
            Arity::None => Vec::new(),
            Arity::One => vec![self.value(field)?],
            Arity::List => self.list(field)?,
        };
        Ok(Expr::Cond { field: field.name.clone(), op, values })
    }

    /// The longest operator spelling at the cursor.
    fn operator(&mut self, field: &QueryField) -> Result<Op, QueryError> {
        let allowed = field.operators();
        let mut words: Vec<&str> = Vec::new();
        let mut best = None;
        for token in &self.tokens[self.pos..] {
            let Some(word) = token.word() else { break };
            words.push(word);
            if spellings_starting_with(&words, &allowed).next().is_none() {
                break;
            }
            if let Some(op) = exact_spelling(&words, &allowed) {
                best = Some((op, words.len()));
            }
        }
        let Some((op, consumed)) = best else {
            let names: Vec<String> = allowed.iter().map(|op| op.label()).collect();
            return Err(self.error(format!(
                "Expected an operator for `{}`: {}",
                field.name,
                names.join(", ")
            )));
        };
        self.pos += consumed;
        Ok(op)
    }

    fn value(&mut self, field: &QueryField) -> Result<String, QueryError> {
        let Some(token) = self.peek().cloned() else {
            return Err(self.error(format!("Expected a value for `{}`", field.name)));
        };
        let value = match &token.tok {
            Tok::Str { closed: false, .. } => return Err(self.error("Missing closing `\"`")),
            Tok::Str { text, .. } => text.clone(),
            Tok::Word(word) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(word)) => {
                word.clone()
            }
            _ => return Err(self.error(format!("Expected a value for `{}`", field.name))),
        };
        if let Err(message) = check_value(field, &value) {
            return Err(self.error(message));
        }
        self.pos += 1;
        Ok(value)
    }

    fn list(&mut self, field: &QueryField) -> Result<Vec<String>, QueryError> {
        if self.peek().map(|t| &t.tok) != Some(&Tok::LParen) {
            return Err(self.error("Expected `(` to start the list of values"));
        }
        self.pos += 1;
        let mut values = vec![self.value(field)?];
        loop {
            match self.peek().map(|t| &t.tok) {
                Some(Tok::Comma) => {
                    self.pos += 1;
                    values.push(self.value(field)?);
                }
                Some(Tok::RParen) => {
                    self.pos += 1;
                    return Ok(values);
                }
                _ => return Err(self.error("Expected `,` or `)`")),
            }
        }
    }
}

fn flatten(mut terms: Vec<Expr>, wrap: fn(Vec<Expr>) -> Expr) -> Expr {
    if terms.len() == 1 {
        return terms.pop().expect("one term");
    }
    let is_and = matches!(wrap(Vec::new()), Expr::And(_));
    let mut flat = Vec::new();
    for term in terms {
        match term {
            Expr::And(inner) if is_and => flat.extend(inner),
            Expr::Or(inner) if !is_and => flat.extend(inner),
            term => flat.push(term),
        }
    }
    wrap(flat)
}

fn check_value(field: &QueryField, value: &str) -> Result<(), String> {
    match field.kind {
        FieldKind::Number { .. } if value.parse::<f64>().is_err() => {
            Err(format!("`{}` takes a number, not `{value}`", field.name))
        }
        FieldKind::Priority if priority_number(value).is_none() => Err(format!(
            "`priority` is one of {}, or 0-4",
            PRIORITY_NAMES.join(", ")
        )),
        _ => Ok(()),
    }
}

fn priority_number(value: &str) -> Option<i64> {
    PRIORITIES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(value))
        .map(|(_, n)| *n)
        .or_else(|| value.parse::<i64>().ok().filter(|n| (0..=4).contains(n)))
}

/// Where a priority sits in the urgency order: 0 for urgent, 4 for none.
fn priority_rank(value: &str) -> Option<usize> {
    let number = priority_number(value)?;
    PRIORITIES.iter().position(|(_, n)| *n == number)
}

/// The Linear numbers of the priorities a comparison against `value`
/// selects, e.g. `>= high` is urgent and high.
fn priority_range(op: Op, value: &str) -> Vec<i64> {
    let Some(rank) = priority_rank(value) else {
        return Vec::new();
    };
    PRIORITIES
        .iter()
        .enumerate()
        .filter(|(i, _)| match op {
            Op::Gt => *i < rank,
            Op::Gte => *i <= rank,
            Op::Lt => *i > rank,
            Op::Lte => *i >= rank,
            _ => false,
        })
        .map(|(_, (_, n))| *n)
        .collect()
}

/// Parse a query. An empty (or all-whitespace) query is `Ok(None)`.
pub fn parse(text: &str, fields: &[QueryField]) -> Result<Option<Expr>, QueryError> {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut parser = Parser { tokens, pos: 0, len: text.len(), fields };
    let expr = parser.expr()?;
    if parser.peek().is_some() {
        let message = if parser.peek().map(|t| &t.tok) == Some(&Tok::RParen) {
            "Unmatched `)`"
        } else {
            "Expected `AND` or `OR`"
        };
        return Err(parser.error(message));
    }
    Ok(Some(expr))
}

// ---------------------------------------------------------------------------
// Query → IssueFilter JSON

/// The `IssueFilter` JSON for a query; `{}` for an empty one.
pub fn compile(text: &str, fields: &[QueryField]) -> Result<Value, QueryError> {
    Ok(match parse(text, fields)? {
        None => json!({}),
        Some(expr) => expr_to_json(&expr, fields),
    })
}

fn expr_to_json(expr: &Expr, fields: &[QueryField]) -> Value {
    match expr {
        Expr::And(terms) => json!({ "and": terms.iter().map(|t| expr_to_json(t, fields)).collect::<Vec<_>>() }),
        Expr::Or(terms) => json!({ "or": terms.iter().map(|t| expr_to_json(t, fields)).collect::<Vec<_>>() }),
        Expr::Cond { field, op, values } => {
            let field = find_field(fields, field).expect("parsed fields exist");
            cond_to_json(field, *op, values)
        }
    }
}

fn cond_to_json(field: &QueryField, op: Op, values: &[String]) -> Value {
    let scalar = |value: &str| -> Value {
        match field.kind {
            FieldKind::Priority => json!(priority_number(value).unwrap_or_default()),
            FieldKind::Number { .. } => value
                .parse::<i64>()
                .map(Value::from)
                .or_else(|_| value.parse::<f64>().map(Value::from))
                .unwrap_or(Value::Null),
            _ => Value::String(value.to_string()),
        }
    };
    let operand = match op.arity() {
        Arity::None => Value::Bool(op == Op::IsEmpty),
        Arity::One => scalar(&values[0]),
        Arity::List => Value::Array(values.iter().map(|v| scalar(v)).collect()),
    };
    let body = match field.kind {
        // Linear's numbers run the other way, so `>= high` is the set of
        // priorities at least that urgent, not a number comparison.
        FieldKind::Priority if matches!(op, Op::Lt | Op::Lte | Op::Gt | Op::Gte) => {
            json!({ "in": priority_range(op, &values[0]) })
        }
        FieldKind::Labels => {
            // "Has label X" is some label named X; "has not" is every label
            // not named X, which an issue without labels also satisfies.
            let (quantifier, comparator) = match op {
                Op::Is => ("some", "eq"),
                Op::AnyOf => ("some", "in"),
                Op::IsNot => ("every", "neq"),
                _ => ("every", "nin"),
            };
            json!({ quantifier: { "name": { comparator: operand } } })
        }
        FieldKind::Relation { .. } if op.arity() == Arity::None => {
            json!({ "null": operand })
        }
        FieldKind::Relation { path, .. } => nest(path, json!({ op.comparator(): operand })),
        _ => json!({ op.comparator(): operand }),
    };
    json!({ field.filter_key.clone(): body })
}

fn nest(path: &[&str], mut value: Value) -> Value {
    for key in path.iter().rev() {
        value = json!({ *key: value });
    }
    value
}

// ---------------------------------------------------------------------------
// IssueFilter JSON → query

/// A query equivalent to an `IssueFilter` JSON object, or `None` when it
/// uses something the language cannot write.
pub fn decompile(filter: &Value, fields: &[QueryField]) -> Option<String> {
    let obj = filter.as_object()?;
    if obj.is_empty() {
        return Some(String::new());
    }
    Some(render(&json_to_expr(obj, fields)?))
}

/// [`decompile`] for a single `IssueFilter` key and its value.
pub fn decompile_entry(key: &str, value: &Value, fields: &[QueryField]) -> Option<String> {
    let mut obj = Map::new();
    obj.insert(key.to_string(), value.clone());
    decompile(&Value::Object(obj), fields)
}

fn json_to_expr(obj: &Map<String, Value>, fields: &[QueryField]) -> Option<Expr> {
    let mut terms = Vec::new();
    for (key, value) in obj {
        let term = match key.as_str() {
            "and" | "or" => {
                let inner = value
                    .as_array()?
                    .iter()
                    .map(|v| json_to_expr(v.as_object()?, fields))
                    .collect::<Option<Vec<_>>>()?;
                if inner.is_empty() {
                    return None;
                }
                flatten(inner, if key == "and" { Expr::And } else { Expr::Or })
            }
            _ => fields.iter().find_map(|field| {
                (field.filter_key == *key).then(|| json_to_cond(field, value)).flatten()
            })?,
        };
        terms.push(term);
    }
    Some(flatten(terms, Expr::And))
}

fn single(value: &Value) -> Option<(&str, &Value)> {
    let obj = value.as_object()?;
    (obj.len() == 1).then(|| obj.iter().next().map(|(k, v)| (k.as_str(), v)))?
}

fn json_to_cond(field: &QueryField, value: &Value) -> Option<Expr> {
    let direct = !matches!(field.kind, FieldKind::Relation { .. } | FieldKind::Labels);
    if let Some(obj) = value.as_object().filter(|obj| direct && obj.len() > 1) {
        let terms = obj
            .iter()
            .map(|(key, inner)| json_to_cond(field, &json!({ key.clone(): inner.clone() })))
            .collect::<Option<Vec<_>>>()?;
        return Some(flatten(terms, Expr::And));
    }
    let cond = |op: Op, values: Vec<String>| {
        Some(Expr::Cond { field: field.name.clone(), op, values })
    };
    let (mut key, mut inner) = single(value)?;
    if key == "null" && field.nullable() {
        return cond(if inner.as_bool()? { Op::IsEmpty } else { Op::IsNotEmpty }, Vec::new());
    }
    match field.kind {
        FieldKind::Labels => {
            let (name, comparator) = single(inner)?;
            if name != "name" {
                return None;
            }
            let (comparator, operand) = single(comparator)?;
            let op = match (key, comparator) {
                ("some", "eq") => Op::Is,
                ("some", "in") => Op::AnyOf,
                ("every", "neq") => Op::IsNot,
                ("every", "nin") => Op::NoneOf,
                _ => return None,
            };
            return cond(op, operand_values(field, op, operand)?);
        }
        FieldKind::Relation { path, .. } => {
            for step in path {
                if key != *step {
                    return None;
                }
                (key, inner) = single(inner)?;
            }
        }
        // A stored number comparison on priority is on Linear's numbers,
        // which is not what the query's `<`/`>` mean; it is carried instead.
        FieldKind::Priority if matches!(key, "lt" | "lte" | "gt" | "gte") => return None,
        _ => {}
    }
    let op = field.operators().into_iter().find(|op| op.comparator() == key && op.arity() != Arity::None)?;
    cond(op, operand_values(field, op, inner)?)
}

fn operand_values(field: &QueryField, op: Op, operand: &Value) -> Option<Vec<String>> {
    let scalar = |v: &Value| -> Option<String> {
        match (field.kind, v) {
            (FieldKind::Priority, v) => {
                let n = v.as_i64()?;
                PRIORITIES.iter().find(|(_, p)| *p == n).map(|(name, _)| (*name).to_string())
            }
            (FieldKind::Number { .. }, Value::Number(n)) => Some(n.to_string()),
            (FieldKind::Number { .. }, _) => None,
            (_, Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    match op.arity() {
        Arity::List => {
            let values: Vec<String> = operand.as_array()?.iter().map(scalar).collect::<Option<_>>()?;
            (!values.is_empty()).then_some(values)
        }
        _ => Some(vec![scalar(operand)?]),
    }
}

/// The query text for an expression.
pub fn render(expr: &Expr) -> String {
    fn go(expr: &Expr, parent_is_and: bool) -> String {
        match expr {
            Expr::And(terms) => terms.iter().map(|t| go(t, true)).collect::<Vec<_>>().join(" AND "),
            Expr::Or(terms) => {
                let text = terms.iter().map(|t| go(t, false)).collect::<Vec<_>>().join(" OR ");
                if parent_is_and { format!("({text})") } else { text }
            }
            Expr::Cond { field, op, values } => {
                let mut text = format!("{field} {}", op.label());
                match op.arity() {
                    Arity::None => {}
                    Arity::One => {
                        text.push(' ');
                        text.push_str(&format_value(&values[0]));
                    }
                    Arity::List => {
                        let list: Vec<String> = values.iter().map(|v| format_value(v)).collect();
                        text.push_str(&format!(" ({})", list.join(", ")));
                    }
                }
                text
            }
        }
    }
    go(expr, false)
}

/// A value as written in a query: bare when it can be, quoted otherwise.
pub fn format_value(value: &str) -> String {
    let bare = !value.is_empty()
        && value.chars().all(is_word_char)
        && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(value));
    if bare {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

// ---------------------------------------------------------------------------
// Completion

/// What a suggestion stands for, which the editor may style differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Field,
    Operator,
    Value,
    Keyword,
    Punctuation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    pub kind: SuggestionKind,
    /// The text that replaces [`Completion::range`] (or `replace` when set).
    pub insert: String,
    /// A range to replace instead of [`Completion::range`] — an operator
    /// replaces all the words of the operator typed so far.
    pub replace: Option<Range<usize>>,
}

/// The suggestions at a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    /// The text a suggestion replaces: the partial word before the cursor.
    pub range: Range<usize>,
    pub items: Vec<Suggestion>,
    /// Whether Enter should take the first item without the user picking it.
    /// False where the query may just as well end (after a condition), so
    /// Enter there still commits.
    pub preselect: bool,
    /// A hint for where there is nothing to pick, e.g. free text.
    pub hint: Option<String>,
}

impl Completion {
    /// The query with suggestion `index` applied, and the cursor after it.
    pub fn apply(&self, text: &str, index: usize) -> Option<(String, usize)> {
        let item = self.items.get(index)?;
        let range = item.replace.clone().unwrap_or(self.range.clone());
        let mut insert = item.insert.clone();
        let rest = &text[range.end.min(text.len())..];
        if insert.ends_with(' ') && rest.starts_with(char::is_whitespace) {
            insert.pop();
        }
        let mut head = &text[..range.start];
        if insert.starts_with([')', ',']) {
            // `)` and `,` sit against the value before them.
            head = head.trim_end();
        } else if head.chars().next_back().is_some_and(|c| !c.is_whitespace() && c != '(') {
            // A word needs a space from the token before it, e.g. `AND` after `)`.
            insert.insert(0, ' ');
        }
        let mut out = String::with_capacity(text.len() + insert.len());
        out.push_str(head);
        out.push_str(&insert);
        let cursor = out.len();
        out.push_str(rest);
        Some((out, cursor))
    }
}

/// Where the walk over the tokens before the cursor ended up.
#[derive(Debug)]
enum State<'a> {
    Operand,
    Operator { field: &'a QueryField, words: Vec<Token> },
    Value { field: &'a QueryField },
    ListOpen { field: &'a QueryField },
    ListValue { field: &'a QueryField, chosen: Vec<String>, after_value: bool },
    AfterCondition,
    Stuck,
}

/// The suggestions for the query `text` with the cursor at byte `cursor`.
pub fn complete(
    text: &str,
    cursor: usize,
    fields: &[QueryField],
    cache: Option<&IntrospectionCache>,
) -> Completion {
    let cursor = cursor.min(text.len());
    let completion = complete_at(text, cursor, fields, cache, true);
    // A value just picked (or typed out in full) is done: what may follow
    // it is more useful than itself again.
    let finished = completion.items.len() == 1
        && completion.items[0].kind == SuggestionKind::Value
        && completion.items[0].label.eq_ignore_ascii_case(&text[completion.range.clone()]);
    if finished {
        return complete_at(text, cursor, fields, cache, false);
    }
    completion
}

fn complete_at(
    text: &str,
    cursor: usize,
    fields: &[QueryField],
    cache: Option<&IntrospectionCache>,
    split_partial: bool,
) -> Completion {
    let mut tokens = tokenize(&text[..cursor]);
    // The token the cursor sits at the end of is the prefix being typed.
    let partial = match tokens.last() {
        Some(t)
            if split_partial
                && t.span.end == cursor
                && matches!(t.tok, Tok::Word(_) | Tok::Str { closed: false, .. }) =>
        {
            tokens.pop()
        }
        _ => None,
    };
    let (prefix, range) = match &partial {
        Some(t) => (t.value().unwrap_or_default().to_string(), t.span.clone()),
        None => (String::new(), cursor..cursor),
    };

    let mut depth = 0usize;
    let mut state = State::Operand;
    for token in &tokens {
        state = step(state, token, fields, &mut depth);
        if matches!(state, State::Stuck) {
            return Completion { range, ..Default::default() };
        }
    }

    let mut completion = Completion { range: range.clone(), preselect: true, ..Default::default() };
    let items = &mut completion.items;
    let matches = |label: &str| prefix_rank(label, &prefix).is_some();
    match state {
        State::Operand => {
            for field in fields {
                if matches(&field.name) {
                    items.push(suggestion(&field.name, field.description, SuggestionKind::Field, format!("{} ", field.name)));
                }
            }
            if prefix.is_empty() {
                items.push(suggestion("(", "Group conditions", SuggestionKind::Punctuation, "(".into()));
            }
        }
        State::Operator { field, words } => {
            let start = words.first().map(|t| t.span.start).unwrap_or(range.start);
            let typed: Vec<&str> = words.iter().filter_map(Token::word).collect();
            let allowed = field.operators();
            for op in &allowed {
                let spelling: Vec<String> = op.label().split(' ').map(str::to_string).collect();
                let fits = spelling.len() > typed.len()
                    && spelling.iter().zip(&typed).all(|(s, w)| s.eq_ignore_ascii_case(w))
                    && prefix_rank(&spelling[typed.len()], &prefix) == Some(0);
                if fits {
                    let insert = match op.arity() {
                        Arity::List => format!("{} (", op.label()),
                        _ => format!("{} ", op.label()),
                    };
                    let mut item = suggestion(&op.label(), op_detail(*op, field.kind), SuggestionKind::Operator, insert);
                    item.replace = Some(start..range.end);
                    items.push(item);
                }
            }
            // The operator typed so far may already be whole: `IS` takes
            // its values directly, and `IS EMPTY` ends the condition.
            if let Some(op) = exact_spelling(&typed, &allowed).filter(|_| !typed.is_empty()) {
                match op.arity() {
                    Arity::One => push_values(items, field, cache, &prefix, &[], "", " "),
                    Arity::None if items.is_empty() => {
                        completion.preselect = !prefix.is_empty();
                        push_after_condition(items, depth, &prefix);
                    }
                    Arity::List | Arity::None => {}
                }
            }
        }
        State::Value { field } => {
            push_values(items, field, cache, &prefix, &[], "", " ");
            if items.is_empty() {
                completion.hint = Some(value_hint(field));
            }
        }
        State::ListOpen { .. } => {
            if prefix.is_empty() {
                items.push(suggestion("(", "Start the list of values", SuggestionKind::Punctuation, "(".into()));
            }
        }
        State::ListValue { field, chosen, after_value } => {
            if after_value {
                // A word typed straight after a value starts the next one;
                // picking it supplies the comma.
                if prefix.is_empty() {
                    items.push(suggestion(")", "End the list", SuggestionKind::Punctuation, ") ".into()));
                }
                push_values(items, field, cache, &prefix, &chosen, ", ", " ");
            } else {
                push_values(items, field, cache, &prefix, &chosen, "", " ");
                if items.is_empty() {
                    completion.hint = Some(value_hint(field));
                }
            }
        }
        State::AfterCondition => {
            // The query may as well end here, so Enter commits it unless a
            // keyword has been started.
            completion.preselect = !prefix.is_empty();
            push_after_condition(items, depth, &prefix);
        }
        State::Stuck => {}
    }
    completion.items.sort_by_key(|item| prefix_rank(&item.label, &prefix).unwrap_or(2));
    completion
}

fn step<'a>(state: State<'a>, token: &Token, fields: &'a [QueryField], depth: &mut usize) -> State<'a> {
    match state {
        State::Operand => match &token.tok {
            Tok::LParen => {
                *depth += 1;
                State::Operand
            }
            Tok::Word(name) => match find_field(fields, name) {
                Some(field) => State::Operator { field, words: Vec::new() },
                None => State::Stuck,
            },
            _ => State::Stuck,
        },
        State::Operator { field, mut words } => {
            let allowed = field.operators();
            if let Some(word) = token.word() {
                let mut typed: Vec<&str> = words.iter().filter_map(Token::word).collect();
                typed.push(word);
                if spellings_starting_with(&typed, &allowed).next().is_some() {
                    words.push(token.clone());
                    return State::Operator { field, words };
                }
            }
            let typed: Vec<&str> = words.iter().filter_map(Token::word).collect();
            match exact_spelling(&typed, &allowed) {
                Some(op) => step(after_operator(field, op), token, fields, depth),
                None => State::Stuck,
            }
        }
        State::Value { .. } => match token.value() {
            Some(_) => State::AfterCondition,
            None => State::Stuck,
        },
        State::ListOpen { field } => match token.tok {
            Tok::LParen => State::ListValue { field, chosen: Vec::new(), after_value: false },
            _ => State::Stuck,
        },
        State::ListValue { field, mut chosen, after_value } => match (&token.tok, after_value) {
            (Tok::Comma, true) => State::ListValue { field, chosen, after_value: false },
            (Tok::RParen, true) => State::AfterCondition,
            (_, false) => match token.value() {
                Some(value) => {
                    chosen.push(value.to_string());
                    State::ListValue { field, chosen, after_value: true }
                }
                None => State::Stuck,
            },
            _ => State::Stuck,
        },
        State::AfterCondition => {
            if token.is_keyword("AND") || token.is_keyword("OR") {
                State::Operand
            } else if token.tok == Tok::RParen && *depth > 0 {
                *depth -= 1;
                State::AfterCondition
            } else {
                State::Stuck
            }
        }
        State::Stuck => State::Stuck,
    }
}

fn after_operator(field: &QueryField, op: Op) -> State<'_> {
    match op.arity() {
        Arity::None => State::AfterCondition,
        Arity::One => State::Value { field },
        Arity::List => State::ListOpen { field },
    }
}

/// What may follow a whole condition: `AND`, `OR`, and `)` inside a group.
fn push_after_condition(items: &mut Vec<Suggestion>, depth: usize, prefix: &str) {
    for keyword in ["AND", "OR"] {
        if prefix_rank(keyword, prefix).is_some() {
            let detail = if keyword == "AND" { "Both must match" } else { "Either may match" };
            items.push(suggestion(keyword, detail, SuggestionKind::Keyword, format!("{keyword} ")));
        }
    }
    if depth > 0 && prefix.is_empty() {
        items.push(suggestion(")", "End the group", SuggestionKind::Punctuation, ") ".into()));
    }
}

fn suggestion(label: &str, detail: &str, kind: SuggestionKind, insert: String) -> Suggestion {
    Suggestion { label: label.to_string(), detail: detail.to_string(), kind, insert, replace: None }
}

/// 0 when `label` starts with `prefix`, 1 when it contains it, `None`
/// otherwise (case-insensitive).
fn prefix_rank(label: &str, prefix: &str) -> Option<u8> {
    let label = label.to_lowercase();
    let prefix = prefix.to_lowercase();
    if label.starts_with(&prefix) {
        Some(0)
    } else if label.contains(&prefix) {
        Some(1)
    } else {
        None
    }
}

fn push_values(
    items: &mut Vec<Suggestion>,
    field: &QueryField,
    cache: Option<&IntrospectionCache>,
    prefix: &str,
    chosen: &[String],
    before: &str,
    after: &str,
) {
    for value in field.value_options(cache) {
        if chosen.contains(&value) || prefix_rank(&value, prefix).is_none() {
            continue;
        }
        let insert = format!("{before}{}{after}", format_value(&value));
        let detail = value_detail(&value).unwrap_or("");
        items.push(suggestion(&value, detail, SuggestionKind::Value, insert));
    }
}

/// A human reading of a suggested value, where the value itself is terse:
/// the relative dates are ISO durations (`-P1W`), which read as "1 week ago".
fn value_detail(value: &str) -> Option<&'static str> {
    Some(match value {
        "-P1D" => "1 day ago",
        "-P1W" => "1 week ago",
        "-P2W" => "2 weeks ago",
        "-P1M" => "1 month ago",
        "-P3M" => "3 months ago",
        "-P1Y" => "1 year ago",
        _ => return None,
    })
}

fn value_hint(field: &QueryField) -> String {
    match field.kind {
        FieldKind::Text { .. } => "Type the text, in quotes if it has spaces".into(),
        FieldKind::Number { .. } => "Type a number".into(),
        FieldKind::Date { .. } => "Type a date (2026-01-31) or a duration ago (-P2W)".into(),
        _ => format!("Type a {} value", field.name),
    }
}

fn op_detail(op: Op, kind: FieldKind) -> &'static str {
    match (op, kind) {
        (Op::Lt, FieldKind::Priority) => return "Less urgent than",
        (Op::Lte, FieldKind::Priority) => return "At most as urgent as",
        (Op::Gt, FieldKind::Priority) => return "More urgent than",
        (Op::Gte, FieldKind::Priority) => return "At least as urgent as",
        _ => {}
    }
    match op {
        Op::Is => "Equals",
        Op::IsNot => "Does not equal",
        Op::AnyOf => "Matches any in a list",
        Op::NoneOf => "Matches none in a list",
        Op::Contains => "Contains, ignoring case",
        Op::NotContains => "Does not contain, ignoring case",
        Op::StartsWith => "Starts with, ignoring case",
        Op::Before => "Earlier than",
        Op::After => "Later than",
        Op::Lt => "Less than",
        Op::Lte => "At most",
        Op::Gt => "Greater than",
        Op::Gte => "At least",
        Op::IsEmpty => "Has no value",
        Op::IsNotEmpty => "Has a value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cache() -> IntrospectionCache {
        IntrospectionCache {
            workspace_slug: String::new(),
            filter_fields: Vec::new(),
            enums: HashMap::new(),
            relation_options: HashMap::from([
                ("team".to_string(), vec!["TOD".to_string(), "OPS".to_string()]),
                ("labels".to_string(), vec!["bug".to_string(), "needs review".to_string()]),
            ]),
        }
    }

    fn compile_ok(text: &str) -> Value {
        compile(text, &fields(None)).unwrap()
    }

    #[test]
    fn a_single_condition_compiles_to_its_filter() {
        assert_eq!(compile_ok("team IS TOD"), json!({ "team": { "key": { "eq": "TOD" } } }));
        assert_eq!(
            compile_ok("team is any of (TOD, \"O P\")"),
            json!({ "team": { "key": { "in": ["TOD", "O P"] } } })
        );
        assert_eq!(compile_ok("assignee IS EMPTY"), json!({ "assignee": { "null": true } }));
        assert_eq!(compile_ok("title CONTAINS \"crash\""), json!({ "title": { "containsIgnoreCase": "crash" } }));
        // Linear numbers priorities the other way round: `<= high` is what
        // is at most as urgent as high.
        assert_eq!(compile_ok("priority <= high"), json!({ "priority": { "in": [2, 3, 4, 0] } }));
        assert_eq!(compile_ok("priority >= high"), json!({ "priority": { "in": [1, 2] } }));
        assert_eq!(compile_ok("priority > urgent"), json!({ "priority": { "in": [] } }));
        assert_eq!(compile_ok("labels IS NONE OF (bug)"), json!({ "labels": { "every": { "name": { "nin": ["bug"] } } } }));
        assert_eq!(compile_ok("stateType = started"), json!({ "state": { "type": { "eq": "started" } } }));
        assert_eq!(compile_ok(""), json!({}));
    }

    #[test]
    fn and_binds_tighter_than_or_and_parentheses_group() {
        let a = json!({ "team": { "key": { "eq": "A" } } });
        let b = json!({ "team": { "key": { "eq": "B" } } });
        let c = json!({ "team": { "key": { "eq": "C" } } });
        assert_eq!(
            compile_ok("team IS A OR team IS B AND team IS C"),
            json!({ "or": [a, { "and": [b, c] }] })
        );
        assert_eq!(
            compile_ok("(team IS A OR team IS B) AND team IS C"),
            json!({ "and": [{ "or": [a, b] }, c] })
        );
        assert_eq!(
            compile_ok("team IS A AND (team IS B AND team IS C)"),
            json!({ "and": [a, b, c] })
        );
    }

    #[test]
    fn errors_say_what_was_expected_and_where() {
        let fields = fields(None);
        let err = compile("team IS", &fields).unwrap_err();
        assert!(err.message.contains("value"), "{err:?}");
        assert_eq!(err.span, 7..7);
        let err = compile("colour IS red", &fields).unwrap_err();
        assert!(err.message.contains("Unknown field"));
        assert_eq!(err.span, 0..6);
        assert!(compile("team CONTAINS x", &fields).is_err());
        assert!(compile("(team IS A", &fields).is_err());
        assert!(compile("team IS A)", &fields).is_err());
        assert!(compile("priority IS soon", &fields).is_err());
        assert!(compile("team IS \"open", &fields).is_err());
    }

    #[test]
    fn decompile_round_trips_what_compile_writes() {
        let fields = fields(None);
        for text in [
            "team IS ANY OF (TOD, \"O P\")",
            "assignee IS NOT EMPTY",
            "labels IS bug AND priority IS NONE OF (low, noPriority)",
            "(team IS A OR team IS B) AND title CONTAINS \"a \\\"b\\\"\"",
            "createdAt AFTER -P2W",
            "stateType IS started",
        ] {
            let json = compile(text, &fields).unwrap();
            let back = decompile(&json, &fields).expect(text);
            assert_eq!(compile(&back, &fields).unwrap(), json, "{text} -> {back}");
        }
    }

    #[test]
    fn decompile_reads_the_old_form_s_filters_and_refuses_the_rest() {
        let fields = fields(None);
        assert_eq!(
            decompile_entry("team", &json!({ "key": { "in": ["TOD"] } }), &fields).as_deref(),
            Some("team IS ANY OF (TOD)")
        );
        assert_eq!(
            decompile_entry("labels", &json!({ "some": { "name": { "in": ["a b"] } } }), &fields).as_deref(),
            Some("labels IS ANY OF (\"a b\")")
        );
        assert_eq!(
            decompile_entry("dueDate", &json!({ "gte": "2026-01-01", "lte": "2026-02-01" }), &fields).as_deref(),
            Some("dueDate >= 2026-01-01 AND dueDate <= 2026-02-01")
        );
        assert_eq!(decompile_entry("title", &json!({ "contains": "x" }), &fields), None);
        assert_eq!(decompile_entry("priority", &json!({ "lte": 2 }), &fields), None);
        assert_eq!(decompile_entry("id", &json!({ "eq": "x" }), &fields), None);
    }

    fn labels(completion: &Completion) -> Vec<&str> {
        completion.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn completion_walks_field_operator_value() {
        let cache = cache();
        let fields = fields(Some(&cache));
        let c = complete("", 0, &fields, Some(&cache));
        assert!(labels(&c).contains(&"team"));
        assert!(c.preselect);

        let c = complete("te", 2, &fields, Some(&cache));
        assert_eq!(labels(&c)[0], "team");
        assert_eq!(c.apply("te", 0).unwrap(), ("team ".to_string(), 5));

        let c = complete("team ", 5, &fields, Some(&cache));
        assert_eq!(labels(&c), ["IS", "IS NOT", "IS ANY OF", "IS NONE OF"]);
        let (text, cursor) = c.apply("team ", 2).unwrap();
        assert_eq!(text, "team IS ANY OF (");

        let c = complete(&text, cursor, &fields, Some(&cache));
        assert_eq!(labels(&c), ["TOD", "OPS"]);
        let (text, cursor) = c.apply(&text, 0).unwrap();
        assert_eq!(text, "team IS ANY OF (TOD ");

        let c = complete(&text, cursor, &fields, Some(&cache));
        assert_eq!(labels(&c), [")", "OPS"]);
        let (text, cursor) = c.apply(&text, 1).unwrap();
        assert_eq!(text, "team IS ANY OF (TOD, OPS ");
        let c = complete(&text, cursor, &fields, Some(&cache));
        let (text, cursor) = c.apply(&text, 0).unwrap();
        assert_eq!(text, "team IS ANY OF (TOD, OPS) ");

        let c = complete(&text, cursor, &fields, Some(&cache));
        assert_eq!(labels(&c), ["AND", "OR"]);
        assert!(!c.preselect);

        // A keyword started by hand is taken by Enter.
        let c = complete("team IS TOD an", 14, &fields, Some(&cache));
        assert_eq!(labels(&c), ["AND"]);
        assert!(c.preselect);

        let c = complete("(team IS TOD)", 13, &fields, Some(&cache));
        assert_eq!(c.apply("(team IS TOD)", 0).unwrap().0, "(team IS TOD) AND ");
    }

    #[test]
    fn completion_takes_the_next_list_value_typed_straight_after_one() {
        let cache = cache();
        let fields = fields(Some(&cache));
        let text = "team IS ANY OF (TOD op";
        let c = complete(text, text.len(), &fields, Some(&cache));
        assert_eq!(labels(&c), ["OPS"]);
        assert_eq!(c.apply(text, 0).unwrap().0, "team IS ANY OF (TOD, OPS ");
    }

    #[test]
    fn completion_follows_an_operator_that_takes_no_value() {
        let fields = fields(None);
        let text = "(assignee IS EMPTY ";
        let c = complete(text, text.len(), &fields, None);
        assert_eq!(labels(&c), ["AND", "OR", ")"]);
        assert!(!c.preselect);
        assert_eq!(c.apply(text, 2).unwrap().0, "(assignee IS EMPTY) ");
        // `IS NOT` is whole too, but may still grow into `IS NOT EMPTY`.
        let text = "assignee IS NOT ";
        let c = complete(text, text.len(), &fields, None);
        assert_eq!(labels(&c)[0], "IS NOT EMPTY");
        assert!(c.preselect);
    }

    #[test]
    fn completion_extends_a_partly_typed_operator() {
        let fields = fields(None);
        let c = complete("assignee IS N", 13, &fields, None);
        assert_eq!(labels(&c), ["IS NOT", "IS NONE OF", "IS NOT EMPTY"]);
        assert_eq!(c.apply("assignee IS N", 0).unwrap().0, "assignee IS NOT ");
    }

    #[test]
    fn completion_quotes_values_that_need_it_and_offers_close_paren_in_groups() {
        let cache = cache();
        let fields = fields(Some(&cache));
        let c = complete("labels IS ", 10, &fields, Some(&cache));
        let index = labels(&c).iter().position(|l| *l == "needs review").unwrap();
        assert_eq!(c.apply("labels IS ", index).unwrap().0, "labels IS \"needs review\" ");

        let text = "(team IS TOD ";
        let c = complete(text, text.len(), &fields, Some(&cache));
        assert_eq!(labels(&c), ["AND", "OR", ")"]);
    }
}
