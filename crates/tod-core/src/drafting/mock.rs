//! `--agent mock` drafter. Acts through [`InterviewClient`], the same path
//! `tod-cli` gives a real drafter, so a mock run exercises the same writes,
//! attribution, provenance, and guards.

use crate::interview::client::InterviewClient;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use tod_store::drafting::*;
#[cfg(test)]
use tod_store::fleet::FleetStore;
use tod_store::interview::*;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{EXTRA_CONTENT_GOAL, KIND_REQUIREMENT, OutlineMutation};
use uuid::Uuid;

/// How the mock reaches the data: through [`InterviewClient`] in the app, or
/// an already-open store (acting as a given actor) in tests.
pub(crate) trait Access {
    fn interview(&self, command: InterviewCommand) -> Result<Value>;
    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R>;
}

impl Access for InterviewClient {
    fn interview(&self, command: InterviewCommand) -> Result<Value> {
        InterviewClient::interview(self, command)
    }

    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        InterviewClient::read(self, f)
    }
}

/// An open store acting as `actor`.
#[cfg(test)]
pub(crate) struct Direct<'a> {
    pub fleet: &'a FleetStore,
    pub actor: String,
}

#[cfg(test)]
impl Access for Direct<'_> {
    fn interview(&self, command: InterviewCommand) -> Result<Value> {
        self.fleet
            .interview(&self.actor, command)
            .map_err(|err| anyhow::anyhow!("{err:#}"))
    }

    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        self.fleet.read(f)
    }
}

/// Split a dump into sentence-sized pieces; a question keeps its `?`.
fn pieces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            '.' | '!' | '\n' => {
                push_piece(&mut out, &current);
                current.clear();
            }
            '?' => {
                current.push('?');
                push_piece(&mut out, &current);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    push_piece(&mut out, &current);
    out
}

fn push_piece(out: &mut Vec<String>, piece: &str) {
    let piece = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    if !piece.is_empty() {
        out.push(piece);
    }
}

fn sentence(piece: &str) -> String {
    let trimmed = piece.trim().trim_end_matches(['.', '?']);
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn add_requirement(
    client: &impl Access,
    row: &AgentSessionRow,
    body: String,
    attention: &str,
    why: &str,
) -> Result<()> {
    let id = Uuid::new_v4();
    client.interview(InterviewCommand::Outline {
        mutation: OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: row.node_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body,
            phase: row.phase.clone(),
        },
        target: None,
    })?;
    client.interview(InterviewCommand::SetAttention {
        obligation_id: id,
        attention: attention.into(),
        why: Some(why.into()),
    })?;
    Ok(())
}

pub(crate) fn drafter(client: &impl Access, row: &AgentSessionRow, text: &str) -> Result<String> {
    let node = row.node_id;
    let capture = row.phase == PHASE_REQUIREMENTS;
    let mut added = 0usize;
    let mut asked = 0usize;
    let mut rewritten = 0usize;

    let dump_seqs: Vec<i64> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("### d-")?.trim().parse().ok())
        .collect();
    for seq in dump_seqs {
        let Some(dump) = client.read(|conn| DraftingRepo::new(conn).get_dump(seq))? else {
            continue;
        };
        let parts = pieces(&dump.body);
        if capture {
            let goal = client.read(|conn| NodeRepo::new(conn).get_extra_content(node, EXTRA_CONTENT_GOAL))?;
            if goal.is_none_or(|g| g.trim().is_empty()) {
                if let Some(first) = parts.first() {
                    client.interview(InterviewCommand::Outline {
                        mutation: OutlineMutation::SetExtraContent {
                            node_id: node,
                            content_type: EXTRA_CONTENT_GOAL.into(),
                            body: sentence(first),
                        },
                        target: None,
                    })?;
                }
            }
        }
        for (i, piece) in parts.iter().enumerate() {
            if piece.ends_with('?') {
                let open = client.read(|conn| Ok(DraftingRepo::new(conn).list_choices(node, &[CHOICE_OPEN])?.len()))?;
                if open < CHOICE_CAP {
                    let base = sentence(piece);
                    let base = base.trim_end_matches('.');
                    let option = |label: &str, suffix: &str| ChoiceOption {
                        label: label.into(),
                        obligations: vec![ChoiceObligation {
                            kind: KIND_REQUIREMENT.into(),
                            body: format!("{base}: {suffix}."),
                            section: None,
                        }],
                    };
                    client.interview(InterviewCommand::AddChoice {
                        node_id: node,
                        context: Some("Mock: reasonable people would split on this.".into()),
                        question: piece.clone(),
                        options: vec![option("Yes", "yes"), option("No", "no")],
                    })?;
                    asked += 1;
                    continue;
                }
            }
            let (attention, why) = match i % 3 {
                0 => (ATTENTION_LOW, "Restates what you said"),
                1 => (ATTENTION_MEDIUM, "Mock: a reasonable default where people differ"),
                _ => (ATTENTION_HIGH, "Mock: a taste call"),
            };
            add_requirement(client, row, sentence(piece), attention, why)?;
            added += 1;
        }
    }

    // "You pick": take the first option as the drafter's own call.
    for line in text.lines().filter(|l| l.contains("You pick")) {
        let Some(seq) = line
            .trim()
            .strip_prefix("- c-")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        let Some(choice) = client.read(|conn| DraftingRepo::new(conn).get_choice(node, seq))? else {
            continue;
        };
        if let Some(ob) = choice.options.first().and_then(|o| o.obligations.first()) {
            add_requirement(client, row, ob.body.clone(), ATTENTION_HIGH, "Mock: picked the first option for you")?;
            added += 1;
        }
    }

    if text.contains("## Rewrite pre-v3 obligations") {
        let old: Vec<MarkedObligation> = client.read(|conn| {
            Ok(DraftingRepo::new(conn)
                .marked_obligations(node)?
                .into_iter()
                .filter(|m| m.mark.is_pre_v3())
                .collect())
        })?;
        for m in old {
            let reworded = sentence(&m.obligation.body);
            if reworded != m.obligation.body {
                client.interview(InterviewCommand::Outline {
                    mutation: OutlineMutation::UpdateObligationBody {
                        obligation_id: m.obligation.id,
                        body: reworded,
                    },
                    target: None,
                })?;
            }
            client.interview(InterviewCommand::SetAttention {
                obligation_id: m.obligation.id,
                attention: ATTENTION_MEDIUM.into(),
                why: Some("Mock rewrite: reworded, nothing merged".into()),
            })?;
            rewritten += 1;
        }
    }

    let open = client.read(|conn| DraftingRepo::new(conn).list_choices(node, &[CHOICE_OPEN]))?;
    if !capture {
        let (outcome, detail) = if open.is_empty() {
            ("pass", "Mock: a competent implementer would build this correctly".to_string())
        } else {
            (
                "fail",
                format!(
                    "Waiting on {}",
                    open.iter().map(|c| c.label()).collect::<Vec<_>>().join(", ")
                ),
            )
        };
        client.interview(InterviewCommand::SetBuildable {
            node_id: node,
            outcome: outcome.into(),
            detail: Some(detail),
        })?;
    }

    let title = client
        .read(|conn| NodeRepo::new(conn).get(node))?
        .map(|n| n.title)
        .unwrap_or_default();
    let mut summary = Vec::new();
    if added > 0 {
        summary.push(format!("{title}  + {added} requirement{}", if added == 1 { "" } else { "s" }));
    }
    if rewritten > 0 {
        summary.push(format!("{title}  {rewritten} pre-v3 obligation(s) rewritten"));
    }
    if asked > 0 || !open.is_empty() {
        summary.push(format!("{} choice(s) waiting on {title}", open.len()));
    }
    if capture && added > 0 {
        summary.push("Not mentioned yet: what happens when something fails".into());
        summary.push("Not mentioned yet: who can use it".into());
    }
    if !capture {
        summary.push(if open.is_empty() { "Buildable".into() } else { "Not buildable yet".into() });
    }
    if summary.is_empty() {
        summary.push("No changes.".into());
    }
    Ok(summary.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_split_into_sentences_and_questions() {
        assert_eq!(
            pieces("Notes have a preview. Should they sync?\nKeep it fast!"),
            ["Notes have a preview", "Should they sync?", "Keep it fast"]
        );
        assert_eq!(sentence("notes sync"), "Notes sync.");
    }
}
