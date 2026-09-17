//! `--agent mock` conversation agent. It reads one directive per line of the
//! user's message and carries them out through [`InterviewClient`], the path
//! `tod-cli` gives a real agent, so a mock run records the same actions.
//!
//! Directives:
//!
//! ```text
//! add obligation <node-slug>: <text>
//! add plan <node-slug>: <text>
//! add node <parent-slug>: <title>
//! rename <id>: <text>
//! delete <id>
//! move <id> under <node-slug>
//! flag <id>: <reason>
//! ask <text>
//! think <text>
//! ```
//!
//! `<id>` is a node slug or UUID, or an obligation or plan-step id (full or
//! 8-character prefix). The reply is empty, except that `ask` echoes its text
//! and a line the mock cannot carry out gets a one-line note, each note its
//! own markdown paragraph.
//!
//! Like a real agent, the mock also reports the reply's parts
//! ([`MockReply::parts`]): when there is work, a note that it is working, a
//! thought for each `think`, and a tool call for each carried-out directive;
//! then the reply.

use crate::interview::client::InterviewClient;
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;
use tod_agent::{MockInterviewTurn, MockReply, ReplyPart};
use tod_store::conversation::{Entity, actor_conversation};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_ENV, InterviewCommand, InterviewRepo, PHASE_REQUIREMENTS};
use tod_store::outline::repos::{NodeRepo, OutlineRepo};
use tod_store::outline::{CreatePosition, KIND_REQUIREMENT, OutlineMutation};
use uuid::Uuid;

/// Where the user's message starts in a prompt that also carries a delta
/// (see `driver::join`).
const MESSAGE_HEADING: &str = "# Message\n\n";

/// How the mock reaches the data: through [`InterviewClient`] in the app, or
/// an already-open store in tests.
pub trait Access {
    fn actor(&self) -> &str;
    fn interview(&self, command: InterviewCommand) -> Result<Value>;
    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R>;
}

impl Access for InterviewClient {
    fn actor(&self) -> &str {
        InterviewClient::actor(self)
    }

    fn interview(&self, command: InterviewCommand) -> Result<Value> {
        InterviewClient::interview(self, command)
    }

    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        InterviewClient::read(self, f)
    }
}

/// An open store acting as `actor`.
pub struct Direct<'a> {
    pub fleet: &'a FleetStore,
    pub actor: String,
}

impl Access for Direct<'_> {
    fn actor(&self) -> &str {
        &self.actor
    }

    fn interview(&self, command: InterviewCommand) -> Result<Value> {
        self.fleet
            .interview(&self.actor, command)
            .map_err(|err| anyhow::anyhow!("{err:#}"))
    }

    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        self.fleet.read(f)
    }
}

/// Play one conversation turn for the registered mock handler.
pub fn handle_turn(data_root: &Path, turn: &MockInterviewTurn) -> Result<MockReply> {
    let actor = turn
        .env
        .iter()
        .find(|(key, _)| key == ACTOR_ENV)
        .map(|(_, value)| value.clone())
        .context("mock conversation turn has no actor")?;
    let client = InterviewClient::new(data_root, actor);
    reply(&client, &turn.blocks)
}

/// Carry out the directives in the user's message (the last prompt block)
/// and return the agent's reply.
pub fn reply(client: &impl Access, blocks: &[String]) -> Result<MockReply> {
    let conversation = actor_conversation(client.actor())
        .context("mock conversation actor is not `conversation:<uuid>`")?;
    let last = blocks.last().map(String::as_str).unwrap_or_default();
    let message = match last.rfind(MESSAGE_HEADING) {
        Some(at) => &last[at + MESSAGE_HEADING.len()..],
        None => last,
    };
    let mut notes = Vec::new();
    let mut parts = Vec::new();
    let lines = message.lines().map(str::trim).filter(|l| !l.is_empty());
    for (ix, line) in lines.enumerate() {
        if let Some(thought) = line.strip_prefix("think ") {
            parts.push(ReplyPart::Thought {
                text: thought.trim().to_string(),
            });
            continue;
        }
        let tool = |status: &str| ReplyPart::Tool {
            id: format!("mock-{ix}"),
            title: format!("tod-cli: {line}"),
            status: status.into(),
        };
        match directive(client, conversation, line) {
            Ok(Some(text)) => notes.push(text),
            Ok(None) => parts.push(tool("completed")),
            Err(err) => {
                parts.push(tool("failed"));
                notes.push(format!("Mock: could not do `{line}`: {err:#}"));
            }
        }
    }
    // The transcript renders replies as markdown, where single newlines run
    // together: give each note its own paragraph.
    let text = notes.join("\n\n");
    if !parts.is_empty() {
        let narration = ReplyPart::Text {
            text: "Working through the message.".into(),
        };
        parts.insert(0, narration);
    }
    if !text.is_empty() {
        parts.push(ReplyPart::Text { text: text.clone() });
    }
    Ok(MockReply {
        text,
        parts: Some(parts),
    })
}

/// Carry out one line. `Ok(Some(text))` is something to say.
fn directive(client: &impl Access, conversation: Uuid, line: &str) -> Result<Option<String>> {
    let outline = |mutation| {
        client.interview(InterviewCommand::Outline {
            mutation,
            target: None,
        })
    };
    if let Some(text) = line.strip_prefix("ask ") {
        return Ok(Some(text.trim().to_string()));
    }
    if let Some(rest) = line.strip_prefix("add obligation ") {
        let (slug, text) = split_colon(rest)?;
        let node_id = node_by_slug(client, slug)?;
        outline(OutlineMutation::CreateObligation {
            obligation_id: Some(Uuid::new_v4()),
            node_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: text.into(),
            phase: PHASE_REQUIREMENTS.into(),
        })?;
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("add plan ") {
        let (slug, text) = split_colon(rest)?;
        let node_id = node_by_slug(client, slug)?;
        outline(OutlineMutation::CreatePlanStep {
            step_id: Some(Uuid::new_v4()),
            node_id,
            after_id: None,
            before: false,
            body: text.into(),
        })?;
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("add node ") {
        let (slug, title) = split_colon(rest)?;
        let parent = node_by_slug(client, slug)?;
        let list_id = client
            .read(|conn| OutlineRepo::new(conn).get_entry(parent))?
            .context("the parent is not in the outline")?
            .list_id;
        outline(OutlineMutation::CreateNode {
            node_id: Some(Uuid::new_v4()),
            list_id,
            parent_id: Some(parent),
            anchor_id: None,
            position: CreatePosition::Child,
            title: title.into(),
        })?;
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("rename ") {
        let (raw, text) = split_colon(rest)?;
        let mutation = match item(client, raw)? {
            (Entity::Node, node_id) => OutlineMutation::UpdateNodeTitle {
                node_id,
                title: text.into(),
            },
            (Entity::Obligation, obligation_id) => OutlineMutation::UpdateObligationBody {
                obligation_id,
                body: text.into(),
            },
            (Entity::PlanStep, step_id) => OutlineMutation::UpdatePlanStepBody {
                step_id,
                body: text.into(),
            },
        };
        outline(mutation)?;
        return Ok(None);
    }
    if let Some(raw) = line.strip_prefix("delete ") {
        let mutation = match item(client, raw.trim())? {
            (Entity::Node, node_id) => OutlineMutation::DeleteNode { node_id },
            (Entity::Obligation, obligation_id) => {
                OutlineMutation::DeleteObligation { obligation_id }
            }
            (Entity::PlanStep, step_id) => OutlineMutation::DeletePlanStep { step_id },
        };
        outline(mutation)?;
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("move ") {
        let (raw, slug) = rest
            .split_once(" under ")
            .context("expected `move <id> under <node-slug>`")?;
        let target = node_by_slug(client, slug.trim())?;
        let mutation = match item(client, raw.trim())? {
            (Entity::Node, node_id) => {
                let ordinal = client.read(|conn| {
                    let outline = OutlineRepo::new(conn);
                    let list_id = outline
                        .get_entry(target)?
                        .context("the target is not in the outline")?
                        .list_id;
                    outline.next_ordinal(list_id, Some(target))
                })?;
                OutlineMutation::ReparentNode {
                    node_id,
                    parent_id: Some(target),
                    ordinal,
                }
            }
            (Entity::Obligation, obligation_id) => OutlineMutation::MoveObligation {
                obligation_id,
                target_node_id: target,
            },
            (Entity::PlanStep, _) => bail!("plan steps cannot move between nodes"),
        };
        outline(mutation)?;
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("flag ") {
        let (raw, reason) = split_colon(rest)?;
        let (entity, entity_id) = item(client, raw)?;
        client.interview(InterviewCommand::FlagConversationItem {
            conversation_id: conversation,
            entity,
            entity_id,
            reason: reason.into(),
        })?;
        return Ok(None);
    }
    Ok(Some(format!("Mock: I don't understand `{line}`.")))
}

fn split_colon(rest: &str) -> Result<(&str, &str)> {
    let (head, text) = rest
        .split_once(':')
        .context("expected `<target>: <text>`")?;
    let (head, text) = (head.trim(), text.trim());
    if head.is_empty() || text.is_empty() {
        bail!("expected `<target>: <text>`");
    }
    Ok((head, text))
}

fn node_by_slug(client: &impl Access, raw: &str) -> Result<Uuid> {
    client.read(|conn| {
        let nodes = NodeRepo::new(conn);
        if let Ok(id) = Uuid::parse_str(raw) {
            if nodes.get(id)?.is_some() {
                return Ok(id);
            }
        }
        nodes
            .get_by_slug(raw)?
            .map(|n| n.id)
            .with_context(|| format!("no node `{raw}`"))
    })
}

/// Resolve `raw` to a node, obligation, or plan step, in that order.
fn item(client: &impl Access, raw: &str) -> Result<(Entity, Uuid)> {
    if let Ok(id) = node_by_slug(client, raw) {
        return Ok((Entity::Node, id));
    }
    client.read(|conn| {
        let repo = InterviewRepo::new(conn);
        if let Ok(id) = repo.resolve_obligation_id(raw) {
            return Ok((Entity::Obligation, id));
        }
        if let Ok(id) = repo.resolve_plan_step_id(raw) {
            return Ok((Entity::PlanStep, id));
        }
        bail!("no node, obligation, or plan step `{raw}`")
    })
}
