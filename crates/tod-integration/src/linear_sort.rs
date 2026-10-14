//! The order a Linear generator's tickets are shown in, written as text:
//!
//! ```text
//! priority, updatedAt desc, ticket
//! ```
//!
//! A sort is a comma-separated list of terms, `<field> [asc|desc]`, applied
//! in turn (a later term breaks ties in an earlier one). A field is anything
//! the ticket's row keeps: its id, title, labels, and the synced metadata
//! (priority, state, assignee, dates, estimate). Tickets missing a value
//! always sort after the ones that have it.
//!
//! The sort runs over rows already in the outline, so changing it (or
//! flipping its direction from the row's sort button) never calls Linear.
//! The one use Linear has for it is the fetch itself: Linear can order
//! only by `createdAt` or `updatedAt` (newest first), and a result cap cuts
//! the list where it ends, so when the first term is one of those the fetch
//! asks for it ([`order_by`]) and the cap keeps the right tickets.

use crate::linear_query::{Completion, Suggestion, SuggestionKind};
use serde_json::Value;
use std::cmp::Ordering;
use std::ops::Range;

/// The generator config key the sort text is stored under.
pub const SORT_KEY: &str = "sort_query";

/// How a field's values are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `TOD-9` before `TOD-10`: the team prefix, then the number.
    Ticket,
    Text,
    /// Urgent first; no priority last.
    Priority,
    /// Workflow order: triage, backlog, unstarted, started, completed, canceled.
    StateType,
    Number,
    /// ISO 8601 text, which orders correctly as text.
    Date,
}

/// Where a field's value is read from on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    ExternalId,
    Title,
    Labels,
    Meta(&'static str),
}

/// A field a sort term can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortField {
    pub name: &'static str,
    aliases: &'static [&'static str],
    kind: Kind,
    source: Source,
    /// Linear's `orderBy` value for this field, when it has one.
    order_by: Option<&'static str>,
    pub description: &'static str,
}

const fn field(
    name: &'static str,
    aliases: &'static [&'static str],
    kind: Kind,
    source: Source,
    order_by: Option<&'static str>,
    description: &'static str,
) -> SortField {
    SortField { name, aliases, kind, source, order_by, description }
}

/// Every field a sort may name.
pub const FIELDS: [SortField; 12] = [
    field("ticket", &["id", "identifier"], Kind::Ticket, Source::ExternalId, None, "Ticket id (TOD-9 before TOD-10)"),
    field("number", &[], Kind::Number, Source::ExternalId, None, "Issue number, whatever the team"),
    field("title", &[], Kind::Text, Source::Title, None, "Title"),
    field("priority", &[], Kind::Priority, Source::Meta("priority"), None, "Priority (urgent first)"),
    field("state", &[], Kind::Text, Source::Meta("state"), None, "Workflow state name"),
    field("stateType", &[], Kind::StateType, Source::Meta("state_type"), None, "Kind of state (backlog, started, ...)"),
    field("assignee", &[], Kind::Text, Source::Meta("assignee"), None, "Assigned user"),
    field("labels", &[], Kind::Text, Source::Labels, None, "Labels"),
    field("createdAt", &["created"], Kind::Date, Source::Meta("createdAt"), Some("createdAt"), "Created"),
    field("updatedAt", &["updated"], Kind::Date, Source::Meta("updatedAt"), Some("updatedAt"), "Last updated"),
    field("dueDate", &["due"], Kind::Date, Source::Meta("dueDate"), None, "Due date"),
    field("estimate", &[], Kind::Number, Source::Meta("estimate"), None, "Estimate"),
];

fn find_field(name: &str) -> Option<&'static SortField> {
    FIELDS.iter().find(|f| {
        f.name.eq_ignore_ascii_case(name) || f.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    })
}

/// One `<field> [asc|desc]` term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortTerm {
    pub field: &'static SortField,
    pub descending: bool,
}

/// Why a sort does not parse, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortError {
    pub message: String,
    /// Byte range in the sort text.
    pub span: Range<usize>,
}

impl std::fmt::Display for SortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at column {})", self.message, self.span.start + 1)
    }
}

fn direction(word: &str) -> Option<bool> {
    match word.to_ascii_lowercase().as_str() {
        "asc" | "ascending" => Some(false),
        "desc" | "descending" => Some(true),
        _ => None,
    }
}

/// The words of `text` between `start..end`, each with its byte range.
fn words(text: &str, start: usize, end: usize) -> Vec<(Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut word_start = None;
    for (i, c) in text[start..end].char_indices() {
        let at = start + i;
        match (c.is_whitespace(), word_start) {
            (false, None) => word_start = Some(at),
            (true, Some(s)) => {
                out.push((s..at, &text[s..at]));
                word_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = word_start {
        out.push((s..end, &text[s..end]));
    }
    out
}

/// Parse a sort. Empty text is no sort (`Ok` with no terms).
pub fn parse(text: &str) -> Result<Vec<SortTerm>, SortError> {
    let mut terms: Vec<SortTerm> = Vec::new();
    let mut start = 0;
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    for (i, c) in text.char_indices() {
        if c == ',' {
            pieces.push((start, i));
            start = i + 1;
        }
    }
    pieces.push((start, text.len()));
    let last = pieces.len() - 1;
    for (index, (from, to)) in pieces.into_iter().enumerate() {
        let w = words(text, from, to);
        if w.is_empty() {
            // Nothing at all, or a comma still waiting for its next term.
            if index == last {
                continue;
            }
            return Err(SortError {
                message: "expected a field before the comma".into(),
                span: from..to.max(from),
            });
        }
        let (name_span, name) = &w[0];
        let Some(field) = find_field(name) else {
            return Err(SortError {
                message: format!(
                    "unknown sort field `{name}`; one of {}",
                    FIELDS.iter().map(|f| f.name).collect::<Vec<_>>().join(", ")
                ),
                span: name_span.clone(),
            });
        };
        if terms.iter().any(|t| t.field == field) {
            return Err(SortError {
                message: format!("`{}` is already sorted on", field.name),
                span: name_span.clone(),
            });
        }
        let descending = match w.get(1) {
            None => false,
            Some((span, word)) => direction(word).ok_or_else(|| SortError {
                message: format!("expected asc or desc, not `{word}`"),
                span: span.clone(),
            })?,
        };
        if let Some((span, word)) = w.get(2) {
            return Err(SortError {
                message: format!("unexpected `{word}`; separate terms with commas"),
                span: span.clone(),
            });
        }
        terms.push(SortTerm { field, descending });
    }
    Ok(terms)
}

/// Linear's `orderBy` for a fetch sorted this way: the first term's, when
/// Linear can order by it.
pub fn order_by(terms: &[SortTerm]) -> Option<&'static str> {
    terms.first().and_then(|t| t.field.order_by)
}

/// The sort text of a generator config, parsed. A config with none, or with
/// text that does not parse, has no sort.
pub fn terms_from_config(config: &Value) -> Vec<SortTerm> {
    config
        .get(SORT_KEY)
        .and_then(Value::as_str)
        .and_then(|text| parse(text).ok())
        .unwrap_or_default()
}

/// The data of one row a sort compares.
#[derive(Debug, Clone, Copy)]
pub struct SortRow<'a> {
    /// The ticket's id in Linear (`TOD-142`).
    pub external_id: Option<&'a str>,
    pub title: &'a str,
    pub tags: &'a [String],
    /// The row's synced metadata (`EXTRA_CONTENT_METADATA`).
    pub metadata: Option<&'a Value>,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
enum Sortable {
    Text(String),
    Number(f64),
    Ticket(String, u64),
}

fn ticket_parts(id: &str) -> (String, u64) {
    match id.rsplit_once('-') {
        Some((prefix, number)) => match number.parse() {
            Ok(n) => (prefix.to_ascii_lowercase(), n),
            Err(_) => (id.to_ascii_lowercase(), 0),
        },
        None => (id.to_ascii_lowercase(), 0),
    }
}

/// Order two ticket ids the way [`Kind::Ticket`] does: `TOD-9` before `TOD-10`.
pub fn compare_ticket_ids(a: &str, b: &str) -> Ordering {
    ticket_parts(a).cmp(&ticket_parts(b))
}

fn priority_rank(name: &str) -> Option<f64> {
    match name.to_ascii_lowercase().as_str() {
        "urgent" => Some(1.0),
        "high" => Some(2.0),
        "medium" => Some(3.0),
        "low" => Some(4.0),
        "no priority" | "nopriority" => Some(5.0),
        _ => None,
    }
}

fn state_type_rank(kind: &str) -> Option<f64> {
    ["triage", "backlog", "unstarted", "started", "completed", "canceled"]
        .iter()
        .position(|k| k.eq_ignore_ascii_case(kind))
        .map(|p| p as f64)
}

fn value_of(field: &SortField, row: &SortRow) -> Option<Sortable> {
    let meta = |key: &str| row.metadata.and_then(|m| m.get(key)).filter(|v| !v.is_null());
    let meta_text = |key: &str| meta(key).and_then(Value::as_str).filter(|s| !s.is_empty());
    match (field.kind, field.source) {
        (Kind::Ticket, _) => row.external_id.map(|id| {
            let (prefix, number) = ticket_parts(id);
            Sortable::Ticket(prefix, number)
        }),
        (Kind::Number, Source::ExternalId) => {
            row.external_id.map(|id| Sortable::Number(ticket_parts(id).1 as f64))
        }
        (Kind::Number, Source::Meta(key)) => meta(key).and_then(Value::as_f64).map(Sortable::Number),
        (Kind::Priority, Source::Meta(key)) => {
            meta_text(key).and_then(priority_rank).map(Sortable::Number)
        }
        (Kind::StateType, Source::Meta(key)) => {
            meta_text(key).and_then(state_type_rank).map(Sortable::Number)
        }
        (_, Source::Title) => Some(Sortable::Text(row.title.to_lowercase())),
        (_, Source::Labels) => {
            let mut tags: Vec<String> = row.tags.iter().map(|t| t.to_lowercase()).collect();
            tags.sort();
            (!tags.is_empty()).then(|| Sortable::Text(tags.join(",")))
        }
        (_, Source::Meta(key)) => meta_text(key).map(|s| Sortable::Text(s.to_lowercase())),
        (_, Source::ExternalId) => row.external_id.map(|id| Sortable::Text(id.to_lowercase())),
    }
}

/// Compare two rows by `terms`. With `flipped`, every term runs the other
/// way (the row's sort button); rows missing a value still go last.
pub fn compare(terms: &[SortTerm], flipped: bool, a: &SortRow, b: &SortRow) -> Ordering {
    for term in terms {
        let ord = match (value_of(term.field, a), value_of(term.field, b)) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(x), Some(y)) => {
                let ord = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
                if term.descending != flipped { ord.reverse() } else { ord }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// `terms` as text, as [`parse`] reads it.
pub fn render(terms: &[SortTerm]) -> String {
    terms
        .iter()
        .map(|t| format!("{} {}", t.field.name, if t.descending { "desc" } else { "asc" }))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Completion

fn suggestion(label: &str, detail: &str, kind: SuggestionKind, insert: String) -> Suggestion {
    Suggestion { label: label.into(), detail: detail.into(), kind, insert, replace: None }
}

/// The suggestions for the sort `text` with the cursor at byte `cursor`.
pub fn complete(text: &str, cursor: usize) -> Completion {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let before = &text[..cursor];
    let term_start = before.rfind(',').map(|i| i + 1).unwrap_or(0);
    let mut typed = words(text, term_start, cursor);
    // A word the cursor still touches is being typed; the ones before it are done.
    let partial = if before.ends_with(char::is_whitespace) || cursor == term_start {
        cursor..cursor
    } else {
        typed.pop().map(|(range, _)| range).unwrap_or(cursor..cursor)
    };
    let prefix = &text[partial.clone()];
    let matches = |candidate: &str| {
        candidate.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
    };

    let mut items = Vec::new();
    match typed.len() {
        0 => {
            let used: Vec<&str> = parse_used(&text[..term_start]);
            for f in FIELDS.iter().filter(|f| matches(f.name) && !used.contains(&f.name)) {
                items.push(suggestion(f.name, f.description, SuggestionKind::Field, format!("{} ", f.name)));
            }
        }
        1 if find_field(typed[0].1).is_some() => {
            for (word, detail) in [("asc", "Lowest first"), ("desc", "Highest first")] {
                if matches(word) {
                    items.push(suggestion(word, detail, SuggestionKind::Keyword, word.into()));
                }
            }
            if prefix.is_empty() {
                items.push(suggestion(",", "Then by another field", SuggestionKind::Punctuation, ", ".into()));
            }
        }
        2 if direction(typed[1].1).is_some() && prefix.is_empty() => {
            items.push(suggestion(",", "Then by another field", SuggestionKind::Punctuation, ", ".into()));
        }
        _ => {}
    }
    let preselect = !items.is_empty() && !prefix.is_empty();
    Completion { range: partial, items, preselect, hint: None }
}

/// The fields already used by the terms in `before` (whatever parses).
fn parse_used(before: &str) -> Vec<&'static str> {
    before
        .split(',')
        .filter_map(|piece| piece.split_whitespace().next())
        .filter_map(find_field)
        .map(|f| f.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row<'a>(id: &'a str, title: &'a str, meta: &'a Value) -> SortRow<'a> {
        SortRow { external_id: Some(id), title, tags: &[], metadata: Some(meta) }
    }

    fn sorted(sort: &str, flipped: bool, rows: &[(&str, Value)]) -> Vec<String> {
        let terms = parse(sort).unwrap();
        let mut rows: Vec<&(&str, Value)> = rows.iter().collect();
        rows.sort_by(|a, b| compare(&terms, flipped, &row(a.0, "", &a.1), &row(b.0, "", &b.1)));
        rows.into_iter().map(|r| r.0.to_string()).collect()
    }

    #[test]
    fn parses_terms_with_directions_and_aliases() {
        let terms = parse("priority, Updated DESC , id asc").unwrap();
        let named: Vec<_> = terms.iter().map(|t| (t.field.name, t.descending)).collect();
        assert_eq!(named, [("priority", false), ("updatedAt", true), ("ticket", false)]);
        assert_eq!(render(&terms), "priority asc, updatedAt desc, ticket asc");
        assert!(parse("  ").unwrap().is_empty());
    }

    #[test]
    fn rejects_unknown_repeated_and_malformed_terms() {
        assert!(parse("nope").unwrap_err().message.contains("unknown sort field"));
        assert!(parse("title, title desc").unwrap_err().message.contains("already"));
        assert!(parse("title sideways").unwrap_err().message.contains("asc or desc"));
        assert!(parse("title desc extra").unwrap_err().message.contains("unexpected"));
        assert!(parse("title,, priority").is_err());
        assert_eq!(parse("title,").unwrap().len(), 1);
    }

    #[test]
    fn ticket_ids_sort_by_number_not_text() {
        let rows = [("TOD-10", json!({})), ("TOD-9", json!({})), ("OPS-100", json!({}))];
        assert_eq!(sorted("ticket", false, &rows), ["OPS-100", "TOD-9", "TOD-10"]);
        assert_eq!(sorted("ticket desc", false, &rows), ["TOD-10", "TOD-9", "OPS-100"]);
        assert_eq!(sorted("number", false, &rows), ["TOD-9", "TOD-10", "OPS-100"]);
    }

    #[test]
    fn priority_runs_urgent_first_and_missing_values_go_last_either_way() {
        let rows = [
            ("A-1", json!({"priority": "Low"})),
            ("A-2", json!({"priority": null})),
            ("A-3", json!({"priority": "Urgent"})),
            ("A-4", json!({"priority": "No priority"})),
        ];
        assert_eq!(sorted("priority", false, &rows), ["A-3", "A-1", "A-4", "A-2"]);
        assert_eq!(sorted("priority desc", false, &rows), ["A-4", "A-1", "A-3", "A-2"]);
        // Flipping the button reverses the terms, not the position of gaps.
        assert_eq!(sorted("priority", true, &rows), ["A-4", "A-1", "A-3", "A-2"]);
    }

    #[test]
    fn later_terms_break_ties() {
        let rows = [
            ("A-1", json!({"priority": "High", "updatedAt": "2026-01-01T00:00:00Z"})),
            ("A-2", json!({"priority": "High", "updatedAt": "2026-03-01T00:00:00Z"})),
            ("A-3", json!({"priority": "Urgent", "updatedAt": "2026-02-01T00:00:00Z"})),
        ];
        assert_eq!(sorted("priority, updatedAt desc", false, &rows), ["A-3", "A-2", "A-1"]);
        assert_eq!(sorted("priority, updatedAt desc", true, &rows), ["A-1", "A-2", "A-3"]);
    }

    #[test]
    fn order_by_is_the_first_terms_when_linear_can_order_by_it() {
        assert_eq!(order_by(&parse("updatedAt desc, ticket").unwrap()), Some("updatedAt"));
        assert_eq!(order_by(&parse("createdAt").unwrap()), Some("createdAt"));
        assert_eq!(order_by(&parse("ticket, updatedAt").unwrap()), None);
        assert_eq!(order_by(&[]), None);
    }

    #[test]
    fn completes_fields_directions_and_commas() {
        let labels = |text: &str| -> Vec<String> {
            complete(text, text.len()).items.into_iter().map(|i| i.label).collect()
        };
        assert!(labels("").contains(&"ticket".to_string()));
        assert_eq!(labels("pri"), ["priority"]);
        assert_eq!(labels("priority "), ["asc", "desc", ","]);
        assert_eq!(labels("priority d"), ["desc"]);
        assert_eq!(labels("priority desc "), [","]);
        assert!(!labels("priority, ").contains(&"priority".to_string()));
        assert!(labels("priority, ").contains(&"title".to_string()));
    }

    #[test]
    fn a_suggestion_replaces_the_word_being_typed() {
        let text = "priority, upd";
        let completion = complete(text, text.len());
        let (applied, cursor) = completion.apply(text, 0).unwrap();
        assert_eq!(applied, "priority, updatedAt ");
        assert_eq!(cursor, applied.len());
        let completion = complete("priority ", 9);
        let (applied, _) = completion.apply("priority ", 2).unwrap();
        assert_eq!(applied, "priority, ");
    }
}
