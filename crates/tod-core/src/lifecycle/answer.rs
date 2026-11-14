//! Answering a request, as the task panel and `tod-cli decisions answer` do:
//! the answer is recorded (a move the picked option stands for made first),
//! and the message the asking conversation is told is worked out here, so the
//! app delivers it and the CLI, which has no conversation to hand it to, does not.

use anyhow::Context;
use tod_store::decisions::{Decision, DecisionRepo};
use tod_store::fleet::FleetStore;
use tod_store::interview::ACTOR_USER;
use uuid::Uuid;

/// "option 2 (\"per invoice\")", "\"do it Tuesday\"", or "option 1" when
/// there is no free text and the option's own label is empty.
pub fn describe_answer(decision: &Decision, option: Option<i64>, text: Option<&str>) -> String {
    let picked = option.and_then(|o| {
        usize::try_from(o)
            .ok()
            .and_then(|ix| decision.options.get(ix.checked_sub(1)?))
    });
    match (picked, text) {
        (Some(label), Some(text)) if !text.is_empty() => format!("\"{label}\" ({text})"),
        (Some(label), _) => format!("\"{label}\""),
        (None, Some(text)) if !text.is_empty() => format!("\"{text}\""),
        _ => "(no answer recorded)".to_string(),
    }
}

/// Records the user's answer to `decision_id` and says what to tell the
/// asking conversation: its id and the message, or `None` when nothing
/// is to be delivered (a permission, or a decision with no
/// conversation). Blocks on the store: run it off the UI thread.
pub fn record_decision_answer(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<usize>,
    text: Option<String>,
) -> anyhow::Result<Option<(Uuid, String)>> {
    record_decision_answer_as(fleet, decision_id, option, text, ACTOR_USER)
}

/// [`record_decision_answer`] recorded as `actor` (`user`, or `shepherd:<id>`).
pub fn record_decision_answer_as(
    fleet: &FleetStore,
    decision_id: Uuid,
    option: Option<usize>,
    text: Option<String>,
    actor: &str,
) -> anyhow::Result<Option<(Uuid, String)>> {
    let before = fleet
        .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))?
        .with_context(|| format!("decision {decision_id} not found"))?;
    let option_index = option.map(|o| o as i64);

    // An option that sends the node back moves it first (and fails the
    // answer, recording nothing, when it cannot): `crate::lifecycle::send_back`.
    let answered = super::send_back::answer_decision_as(fleet, decision_id, option_index, text.clone(), actor)?;

    let after = fleet
        .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))?
        .with_context(|| format!("decision {decision_id} vanished after answering"))?;

    // A permission is a standing grant, not a message: the runner that
    // stopped for it reopens the step once nothing is waiting, and the
    // agent's next ask is answered from the saved answer
    // (`crate::permission`).
    if after.decision.protocol.as_deref() == Some(crate::permission::PROTOCOL) {
        return Ok(None);
    }
    let Some(conversation_id) = after.decision.conversation_id else {
        return Ok(None);
    };

    let chosen = describe_answer(&after.decision, option_index, text.as_deref());
    // A credential request: the agent is told whether it is now set,
    // never anything of its value (`crate::environment_request`).
    let credential = crate::environment_request::is_request(&after.decision)
        .then(|| crate::environment_request::parse_question(&after.decision.question))
        .flatten()
        .map(|(name, _)| {
            crate::environment_request::answer_message(
                &name,
                crate::environment_request::was_provided(&after.decision, option_index),
            )
        });
    let moved = answered.moved_to.as_ref().map(|to| {
        format!(
            "\n\nThe app has already moved the task back to `{to}` and reopened its \
             implementation steps, as the user chose. Do nothing more in this session."
        )
    });
    let message = match credential {
        Some(message) => message,
        None => match before.answers.last() {
        Some(prev) => {
            let previous =
                describe_answer(&after.decision, prev.option, prev.text.as_deref());
            format!(
                "Decision answered: {}\n\nThe user answered {chosen}.\n\nThe user changed \
                 their answer from {previous} to {chosen}: review what you did based on \
                 {previous} and adjust.",
                after.decision.question
            )
        }
        None => format!(
            "Decision answered: {}\n\nThe user answered {chosen}.",
            after.decision.question
        ),
        },
    };

    // The plan steps this answer opened again, so the agent takes them up
    // with the answer rather than waiting for a retry.
    let released = if after.decision.blocks.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nThis answer also opened the plan steps it held up ([{}]) again: they are ready, carry on \
             with them using this answer. Do not ask this again.",
            after
                .decision
                .blocks
                .iter()
                .map(|id| tod_store::interview::short_id(*id))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(Some((conversation_id, format!("{message}{released}{}", moved.unwrap_or_default()))))
}

