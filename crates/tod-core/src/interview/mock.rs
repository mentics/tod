//! Dispatch `--agent mock` conversation turns. They act through [`InterviewClient`],
//! exactly the path `tod-cli` gives real agents, so a mock run exercises the
//! same writes, attribution, and guards.

use crate::interview::client::InterviewClient;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tod_agent::{MockInterviewTurn, MockReply, SessionPurpose, set_mock_interview_handler};
use tod_store::interview::ACTOR_USER;
use uuid::Uuid;

pub fn install_mock_interview_handler(data_root: PathBuf) {
    set_mock_interview_handler(Arc::new(move |turn: &MockInterviewTurn| {
        handle_turn(&data_root, turn)
    }));
}

fn handle_turn(data_root: &Path, turn: &MockInterviewTurn) -> Result<MockReply> {
    // An implementation, verification, or review conversation carries its node and
    // conversation instead of a conversation actor: its writes are its own,
    // not a change set.
    use crate::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
    let env_uuid = |name: &str| {
        turn.env
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| Uuid::parse_str(value).ok())
    };
    if let (Some(node), Some(conversation)) =
        (env_uuid(IMPLEMENT_NODE_ENV), env_uuid(IMPLEMENT_CONVERSATION_ENV))
    {
        // A phase agent or evaluator also carries its conversation actor:
        // what it records is the conversation's.
        let actor = turn
            .env
            .iter()
            .find(|(key, _)| key == tod_store::interview::ACTOR_ENV)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| ACTOR_USER.to_string());
        let client = InterviewClient::new(data_root, actor);
        // An incoming-changes check carries the actions it was shown.
        if let Some((_, actions)) = turn
            .env
            .iter()
            .find(|(key, _)| key == crate::incoming::INCOMING_ACTIONS_ENV)
        {
            let actions = crate::conversation::incoming::parse_action_ids(Some(actions))?;
            return crate::conversation::incoming::mock_turn(
                &client,
                node,
                conversation,
                actions,
                &turn.blocks.join("\n\n"),
            )
            .map(MockReply::from);
        }
        let place = crate::conversation::mock::Place {
            cwd: Some(&turn.cwd),
            env: &turn.env,
        };
        return crate::conversation::mock::plan_turn(&client, node, conversation, place)
            .map(MockReply::from);
    }
    // A conversation's actor is `conversation:<uuid>`, not a session id.
    if turn.purpose == SessionPurpose::Conversation {
        return crate::conversation::mock::handle_turn(data_root, turn);
    }
    bail!("unsupported mock session purpose: {:?}", turn.purpose)
}
