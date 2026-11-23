use super::model::{Chip, Column, Field, Item, Operator};
use std::{collections::BTreeSet, ops::Range};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Field,
    Operator,
    Value,
    Column,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Field => "Field",
            Self::Operator => "Operator",
            Self::Value => "Value",
            Self::Column => "Column",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Choice {
    Field(Field),
    Operator(Operator),
    Value(String),
    Column(Column),
}

impl Choice {
    pub fn label(&self) -> &str {
        match self {
            Self::Field(Field::Sort) => "Sort by",
            Self::Field(Field::Group) => "Group by",
            Self::Field(Field::Nest) => "Nest by",
            Self::Field(field) => field.label(),
            Self::Operator(operator) => operator.label(),
            Self::Value(value) => value,
            Self::Column(column) => column.label(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    pub stage: Stage,
    pub term: String,
    pub choices: Vec<Choice>,
}

impl Completion {
    pub fn preferred(&self) -> Option<&Choice> {
        self.choices
            .iter()
            .find(|choice| choice.label().eq_ignore_ascii_case(&self.term))
            .or_else(|| (self.choices.len() == 1).then(|| &self.choices[0]))
    }
}

pub enum Edit {
    Continue { text: String, caret: usize },
    Commit(Chip),
}

pub struct ColumnIssue {
    pub selection: (usize, usize),
    pub message: String,
}

struct Word<'a> {
    text: &'a str,
    range: Range<usize>,
}

fn phrase(words: &[Word<'_>], label: &str) -> Option<Range<usize>> {
    let mut end = 0;
    for (index, part) in label.split_whitespace().enumerate() {
        let word = words.get(index)?;
        if !word.text.eq_ignore_ascii_case(part) {
            return None;
        }
        end = word.range.end;
    }
    Some(words.first()?.range.start..end)
}

fn operator_at(words: &[Word<'_>]) -> Option<(Operator, Range<usize>)> {
    Operator::ALL
        .into_iter()
        .find_map(|operator| Some((operator, phrase(words, operator.label())?)))
}

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub struct Draft<'a> {
    raw: &'a str,
    field: Option<Field>,
    field_span: Range<usize>,
    operator: Option<Operator>,
    operator_span: Option<Range<usize>>,
}

impl<'a> Draft<'a> {
    pub fn parse(raw: &'a str) -> Self {
        let mut offset = 0;
        let words: Vec<_> = raw
            .split_whitespace()
            .map(|text| {
                let start = offset + raw[offset..].find(text).unwrap();
                offset = start + text.len();
                Word {
                    text,
                    range: start..offset,
                }
            })
            .collect();

        let field = Field::ALL.into_iter().find_map(|field| {
            let span = phrase(&words, field.label()).or_else(|| phrase(&words, field.alias()?))?;
            Some((field, span))
        });
        if let Some((field, field_span)) = field {
            let remaining =
                &words[words.partition_point(|word| word.range.end <= field_span.end)..];
            let operator = operator_at(remaining);
            let operator_span = operator
                .as_ref()
                .map(|(_, span)| span.clone())
                .or_else(|| remaining.first().map(|word| word.range.clone()));
            return Self {
                raw,
                field: Some(field),
                field_span,
                operator: operator.map(|(operator, _)| operator),
                operator_span,
            };
        }

        // When the field is partially edited, keep the original operator and value.
        let operator = (1..words.len()).find_map(|index| operator_at(&words[index..]));
        let end = operator.as_ref().map_or(raw.trim_end().len(), |(_, span)| {
            raw[..span.start].trim_end().len()
        });
        Self {
            raw,
            field: None,
            field_span: 0..end,
            operator: operator.as_ref().map(|(operator, _)| *operator),
            operator_span: operator.map(|(_, span)| span),
        }
    }

    pub fn complete(
        &self,
        selection: (usize, usize),
        existing: bool,
        items: &[Item],
    ) -> Completion {
        let (anchor, caret) = selection;
        let prefix_end = anchor.min(caret);
        let field_end = self.field_span.end;
        if self.field.is_none() || caret <= field_end {
            let term = normalized(&self.raw[..prefix_end.min(field_end)]);
            let choices = if existing && term.is_empty() {
                Vec::new()
            } else {
                Field::ALL
                    .into_iter()
                    .filter(|field| {
                        normalized(Choice::Field(*field).label()).starts_with(&term)
                            || field
                                .alias()
                                .is_some_and(|alias| normalized(alias).starts_with(&term))
                    })
                    .map(Choice::Field)
                    .collect()
            };
            return Completion {
                stage: Stage::Field,
                term,
                choices,
            };
        }

        let field = self.field.unwrap();
        if field == Field::Show {
            let span = self.column_span(caret);
            let term = normalized(&self.raw[span.start..prefix_end.max(span.start)]);
            let choices = self
                .column_matches(span, &term)
                .into_iter()
                .map(Choice::Column)
                .collect();
            return Completion {
                stage: Stage::Column,
                term,
                choices,
            };
        }
        let operator_end = self
            .operator_span
            .as_ref()
            .map_or(self.raw.len(), |span| span.end);
        if self.operator.is_none() || caret <= operator_end {
            let term = normalized(&self.raw[field_end..prefix_end.clamp(field_end, operator_end)]);
            let choices = field
                .operators()
                .iter()
                .copied()
                .filter(|operator| operator.label().starts_with(&term))
                .map(Choice::Operator)
                .collect();
            return Completion {
                stage: Stage::Operator,
                term,
                choices,
            };
        }

        let term = normalized(&self.raw[operator_end..prefix_end.max(operator_end)]);
        let mut choices: Vec<_> = values(field, items)
            .into_iter()
            .filter(|value| {
                let value = normalized(value);
                term.split_whitespace().all(|part| value.contains(part))
            })
            .map(|value| Choice::Value(value.to_owned()))
            .collect();
        let operator_prefix = normalized(&self.raw[field_end..prefix_end.max(field_end)]);
        choices.extend(
            field
                .operators()
                .iter()
                .copied()
                .filter(|operator| {
                    !term.is_empty()
                        && Some(*operator) != self.operator
                        && operator.label().starts_with(&operator_prefix)
                })
                .map(Choice::Operator),
        );
        Completion {
            stage: Stage::Value,
            term,
            choices,
        }
    }

    pub fn chip(&self) -> Option<Chip> {
        if self.field == Some(Field::Show) {
            return self.show_columns()?.ok();
        }
        let value = &self.raw[self.operator_span.as_ref()?.end..];
        Chip::from_parts(self.field?, self.operator?, value)
    }

    pub fn is_show(&self) -> bool {
        self.field == Some(Field::Show)
    }

    pub fn show_columns(&self) -> Option<Result<Chip, ColumnIssue>> {
        let start = self.field_span.end;
        if !self.is_show() || start == self.raw.len() {
            return None;
        }
        let values = &self.raw[start..];
        if values.trim().eq_ignore_ascii_case("title") {
            return Some(Ok(Chip::Show(Vec::new())));
        }

        let mut columns = Vec::new();
        let mut offset = start;
        for (index, part) in values.split(',').enumerate() {
            let span = offset..offset + part.len();
            offset = span.end + 1;
            let label = part.trim();
            let matches = self.column_matches(span.clone(), &normalized(label));
            let column = Column::parse(label).or_else(|| {
                if label.is_empty() {
                    None
                } else {
                    single_column(&matches)
                }
            });
            if let Some(column) = column
                && !columns.contains(&column)
            {
                columns.push(column);
                continue;
            }
            let issue = match (column, label.is_empty(), matches.len()) {
                (Some(column), _, _) => format!("{} is already shown.", column.label()),
                (None, true, _) => "Choose a field for this column.".into(),
                (None, false, 0) => format!("“{label}” matches no available field."),
                (None, false, _) => format!("“{label}” matches multiple fields. Choose one."),
            };
            let end = span.start + part.trim_end().len();
            // Keep the prefix for ambiguous suggestions. Select unknown or duplicate text for replacement.
            let selection = if label.is_empty() {
                (span.end, span.end)
            } else if column.is_none() && matches.len() > 1 {
                (end, end)
            } else {
                (span.end - part.trim_start().len(), end)
            };
            return Some(Err(ColumnIssue {
                selection,
                message: format!("Column {}. {issue}", index + 1),
            }));
        }
        Some(Ok(Chip::Show(columns)))
    }

    pub fn column_comma(&self, caret: usize, highlighted: Option<Column>) -> Option<Edit> {
        if !self.is_show() || caret < self.field_span.end {
            return None;
        }
        let span = self.column_span(caret);
        let label = self.raw[span.clone()].trim();
        if label.is_empty() && highlighted.is_none() {
            let text = if self.raw.len() == self.field_span.end {
                "Show ".into()
            } else {
                self.raw.to_owned()
            };
            let caret = if self.raw.len() == self.field_span.end {
                text.len()
            } else {
                caret
            };
            return Some(Edit::Continue { text, caret });
        }
        let column = highlighted
            .or_else(|| Column::parse(label))
            .or_else(|| single_column(&self.column_matches(span.clone(), &normalized(label))));
        let label = column.map(Column::label).unwrap_or(label);
        let prefix = format!("{} {label}, ", self.raw[..span.start].trim_end());
        let caret = prefix.len();
        let tail = self.raw[span.end..]
            .strip_prefix(',')
            .unwrap_or_default()
            .trim_start();
        Some(Edit::Continue {
            text: prefix + tail,
            caret,
        })
    }

    fn column_matches(&self, span: Range<usize>, term: &str) -> Vec<Column> {
        let elsewhere = self.raw[self.field_span.end..span.start]
            .split(',')
            .chain(self.raw[span.end..].split(','))
            .filter_map(Column::parse)
            .collect::<Vec<_>>();
        Column::ALL
            .into_iter()
            .filter(|column| {
                let label = normalized(column.label());
                !elsewhere.contains(column)
                    && (label.starts_with(term)
                        || label.split_whitespace().any(|word| word.starts_with(term)))
            })
            .collect()
    }

    fn column_span(&self, caret: usize) -> Range<usize> {
        let start = self.field_span.end;
        let caret = caret.max(start);
        let field_start = self.raw[start..caret]
            .rfind(',')
            .map_or(start, |i| start + i + 1);
        let field_end = self.raw[caret..]
            .find(',')
            .map_or(self.raw.len(), |i| caret + i);
        field_start..field_end
    }

    pub fn choose(&self, choice: &Choice, stage: Stage, caret: usize) -> Option<Edit> {
        let (prefix, suffix) = match choice {
            Choice::Column(column) => {
                let span = self.column_span(caret);
                (
                    format!("{} {}", self.raw[..span.start].trim_end(), column.label()),
                    span.end,
                )
            }
            Choice::Value(value) => {
                return Some(Edit::Commit(Chip::from_parts(
                    self.field?,
                    self.operator?,
                    value,
                )?));
            }
            Choice::Field(field) if field.is_arrangement() => (
                format!("{} ", choice.label()),
                self.operator_span
                    .as_ref()
                    .map_or(self.field_span.end, |span| span.end),
            ),
            Choice::Field(field) => (format!("{} ", field.label()), self.field_span.end),
            Choice::Operator(operator) => (
                format!(
                    "{} {} ",
                    self.raw[..self.field_span.end].trim(),
                    operator.label()
                ),
                if stage == Stage::Value {
                    self.raw.len()
                } else {
                    self.operator_span
                        .as_ref()
                        .map_or(self.field_span.end, |span| span.end)
                },
            ),
        };
        let caret = prefix.len();
        Some(Edit::Continue {
            text: prefix + self.raw[suffix..].trim_start(),
            caret,
        })
    }
}

fn single_column(columns: &[Column]) -> Option<Column> {
    match columns {
        [column] => Some(*column),
        _ => None,
    }
}

fn values(field: Field, items: &[Item]) -> Vec<&str> {
    match field {
        Field::Tag => items
            .iter()
            .flat_map(|item| item.tags.iter().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        Field::Kind | Field::Status | Field::Phase => items
            .iter()
            .map(|item| match field {
                Field::Kind => item.kind,
                Field::Status => item.status,
                _ => item.phase.as_ref(),
            })
            .filter(|value| !value.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        Field::Sort => vec![
            "Name ascending",
            "Name descending",
            "Agent cost ascending",
            "Agent cost descending",
        ],
        Field::Group => vec!["Item type", "Status"],
        Field::Nest => vec!["Part of"],
        Field::Cost | Field::Text | Field::Show => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_active_part_uses_only_the_prefix_before_the_selection() {
        let raw = "Tag is Specification";
        let items = crate::collection::model::items();
        let draft = Draft::parse(raw);
        let labels = |selection| {
            let result = draft.complete(selection, true, &items);
            (
                result.stage,
                result
                    .choices
                    .iter()
                    .map(|choice| choice.label().to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            labels((7, raw.len())),
            (Stage::Value, vec!["Design".into(), "Specification".into()])
        );
        assert_eq!(labels((9, 9)), (Stage::Value, vec!["Specification".into()]));
        assert_eq!(
            labels((5, 5)),
            (Stage::Operator, vec!["is".into(), "is not".into()])
        );
        assert_eq!(
            labels((1, 1)),
            (Stage::Field, vec!["Tag".into(), "Text".into()])
        );
        assert_eq!(labels((0, 0)), (Stage::Field, vec![]));
        assert!(matches!(
            draft.choose(&Choice::Operator(Operator::IsNot), Stage::Operator, 5),
            Some(Edit::Continue { text, caret: 11 }) if text == "Tag is not Specification"
        ));
    }

    #[test]
    fn arrangement_aliases_and_full_clauses_share_the_editor() {
        let items = crate::collection::model::items();
        let field = Draft::parse("order").complete((5, 5), false, &items);
        assert_eq!(field.choices, [Choice::Field(Field::Sort)]);
        let raw = "Order by Agent";
        let value = Draft::parse(raw).complete((raw.len(), raw.len()), false, &items);
        assert_eq!(value.choices.len(), 2);
        assert!(Draft::parse("Order by Agent cost desc").chip().is_some());
        assert!(Draft::parse("Tag is not Specification").chip().is_some());
    }

    #[test]
    fn show_replaces_only_the_column_at_the_caret_and_saves_the_order() {
        let raw = "Show Item type, Ag, Lifecycle phase";
        let draft = Draft::parse(raw);
        let caret = "Show Item type, Ag".len();
        let options = draft.complete((caret, caret), true, &[]);
        assert_eq!(options.stage, Stage::Column);
        assert_eq!(options.choices, [Choice::Column(Column::Cost)]);
        let Edit::Continue { text, .. } = draft
            .choose(&options.choices[0], options.stage, caret)
            .unwrap()
        else {
            panic!("selecting a column should keep the editor open");
        };
        assert_eq!(text, "Show Item type, Agent cost, Lifecycle phase");

        let mut config = crate::collection::model::Config::default();
        let index = config.apply(None, Draft::parse(&text).chip().unwrap());
        assert_eq!(index, 1);
        let saved = serde_json::to_vec(&config).unwrap();
        let reopened: crate::collection::model::Config = serde_json::from_slice(&saved).unwrap();
        assert_eq!(
            reopened.columns(),
            [Column::Kind, Column::Cost, Column::Phase]
        );
        assert!(Draft::parse("Show Status, Status").chip().is_none());
    }

    #[test]
    fn show_comma_completes_unique_matches_and_preserves_later_columns() {
        for (raw, prefix, expected, next) in [
            ("show", "show", "Show ", "Show "),
            (
                "Show co",
                "Show co",
                "Show Agent cost, ",
                "Show Agent cost, ",
            ),
            ("Show t", "Show t", "Show t, ", "Show t, "),
            (
                "Show co, Tags",
                "Show co",
                "Show Agent cost, Tags",
                "Show Agent cost, ",
            ),
        ] {
            assert!(matches!(
                Draft::parse(raw).column_comma(prefix.len(), None),
                Some(Edit::Continue { text, caret }) if text == expected && caret == next.len()
            ));
        }
        let raw = "Show it, t, s";
        assert!(matches!(Draft::parse(raw).show_columns(),
            Some(Err(issue)) if issue.selection == (10, 10) && issue.message.contains("multiple fields")
        ));
        assert!(matches!(
            Draft::parse("Show it, ph, co").show_columns(),
            Some(Ok(Chip::Show(columns))) if columns == [Column::Kind, Column::Phase, Column::Cost]
        ));
        assert!(matches!(
            Draft::parse("Show Item type, Lifecycle phase, Status, Tags, ").show_columns(),
            Some(Err(issue)) if issue.message.contains("Choose a field")
        ));
    }
}
