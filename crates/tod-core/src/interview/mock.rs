//! `--agent mock` interview agents (and the dispatch to the conversation
//! mock, `crate::conversation::mock`). They act through [`InterviewClient`],
//! exactly the path `tod-cli` gives real agents, so a mock run exercises the
//! same writes, attribution, and guards.

use crate::interview::client::InterviewClient;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tod_agent::{MockInterviewTurn, MockReply, SessionPurpose, set_mock_interview_handler};
use tod_store::interview::*;
use uuid::Uuid;

/// Questions the mock question maker asks per phase before it is exhausted.
const MOCK_QUESTION_LIMIT: usize = 12;

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
    // A gate check carries only its node.
    let text = turn.blocks.join("\n\n");
    if let Some(node) = env_uuid(IMPLEMENT_NODE_ENV)
        && text.contains("phase_purpose:** gate_check")
    {
        let client = InterviewClient::new(data_root, ACTOR_USER.to_string());
        return crate::conversation::gate_check::mock_turn(&client, node, &text)
            .map(MockReply::from);
    }
    if let (Some(node), Some(conversation)) =
        (env_uuid(IMPLEMENT_NODE_ENV), env_uuid(IMPLEMENT_CONVERSATION_ENV))
    {
        let client = InterviewClient::new(data_root, ACTOR_USER.to_string());
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
        return crate::conversation::mock::plan_turn(&client, node, conversation)
            .map(MockReply::from);
    }
    // A conversation's actor is `conversation:<uuid>`, not a session id.
    if turn.purpose == SessionPurpose::Conversation {
        return crate::conversation::mock::handle_turn(data_root, turn);
    }
    interview_turn(data_root, turn).map(MockReply::from)
}

fn interview_turn(data_root: &Path, turn: &MockInterviewTurn) -> Result<String> {
    let actor = turn
        .env
        .iter()
        .find(|(key, _)| key == ACTOR_ENV)
        .map(|(_, value)| value.clone())
        .context("mock interview turn has no actor")?;
    let session_id = Uuid::parse_str(&actor).context("mock interview actor is not a session id")?;
    let client = InterviewClient::new(data_root, actor);
    let row = client
        .read(|conn| InterviewRepo::new(conn).get_agent_session(session_id))?
        .context("unknown interview agent session")?;
    let text = turn.blocks.join("\n\n");
    // Reported so a verification run can see each session got one snapshot, then deltas.
    let received = if text.contains("# Interview state") {
        format!("snapshot, {} chars", text.len())
    } else if text.contains("# Changes") {
        format!("delta, {} chars", text.len())
    } else {
        format!("instruction only, {} chars", text.len())
    };
    match turn.purpose {
        SessionPurpose::QuestionMaker => question_maker(&client, &row, &text, &received),
        SessionPurpose::AnswerProcessor => answer_processor(&client, &row, &text, &received),
        SessionPurpose::Chat | SessionPurpose::Drafter | SessionPurpose::Conversation => {
            bail!("not an interview turn")
        }
    }
}

fn question_maker(
    client: &InterviewClient,
    row: &AgentSessionRow,
    text: &str,
    received: &str,
) -> Result<String> {
    let target: usize = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Target open questions: "))
        .and_then(|rest| rest.trim_end_matches('.').trim().parse().ok())
        .unwrap_or(8);
    let (open, authored, handoffs) = client.read(|conn| {
        let repo = InterviewRepo::new(conn);
        let questions = repo.list_questions(row.node_id, &[])?;
        let open = questions.iter().filter(|q| q.is_open()).count();
        let authored = questions
            .iter()
            .filter(|q| q.phase == row.phase && q.author == AUTHOR_QUESTION_MAKER)
            .count();
        let handoffs = repo.list_memory(row.node_id, Some(MEMORY_HANDOFF), Some(MEMORY_OPEN))?;
        Ok((open, authored, handoffs))
    })?;

    let mut added = 0;
    for handoff in &handoffs {
        let n = authored + added + 1;
        client.interview(InterviewCommand::AddQuestion {
            node_id: row.node_id,
            session_id: None,
            phase: None,
            draft: QuestionDraft {
                context: Some(format!("Following up: {}", handoff.body)),
                question: format!("Mock follow-up question {n}: what should change?"),
                ..Default::default()
            },
        })?;
        client.interview(InterviewCommand::UpdateMemory {
            node_id: row.node_id,
            seq: handoff.seq,
            body: None,
            status: Some(MEMORY_DONE.into()),
        })?;
        added += 1;
    }

    let room = MOCK_QUESTION_LIMIT.saturating_sub(authored + added);
    let wanted = target.saturating_sub(open + added).min(room);
    for i in 0..wanted {
        client.interview(InterviewCommand::AddQuestion {
            node_id: row.node_id,
            session_id: None,
            phase: None,
            draft: mock_draft(&row.phase, authored + added + i + 1),
        })?;
    }
    added += wanted;

    client.interview(InterviewCommand::AddMemory {
        node_id: row.node_id,
        kind: MEMORY_PLAN.into(),
        phase: None,
        body: format!(
            "Mock plan: {} of {MOCK_QUESTION_LIMIT} questions asked.",
            authored + added
        ),
        question_seq: None,
    })?;

    if authored + added >= MOCK_QUESTION_LIMIT {
        let session = row
            .interview_session_id
            .context("agent session has no interview session")?;
        client.interview(InterviewCommand::SetExhausted {
            session_id: session,
            reason: Some("The mock question maker has asked everything.".into()),
        })?;
        return Ok(format!("added {added}; exhausted ({received})"));
    }
    Ok(format!("added {added} ({received})"))
}

fn mock_draft(phase: &str, n: usize) -> QuestionDraft {
    // Planning questions never carry a proposal — plan steps go through
    // `tod-cli plan add`, and obligation changes (rare during planning) go
    // through `tod-cli obligations` directly, outside the question/answer
    // flow. See `normalize_proposal` in tod-store's interview command.
    if n % 2 == 1 && phase != PHASE_PLANNING {
        QuestionDraft {
            covers: vec![format!("mock-area-{n}")],
            context: Some(format!("Mock context for question {n}.")),
            question: format!("Mock question {n}: record this requirement?"),
            intent: Some(
                "Option 2 means the user wants something different: hand off a follow-up.".into(),
            ),
            recommend: Some("1".into()),
            options: vec![
                "Accept as written".into(),
                "Not quite — explain in notes".into(),
            ],
            proposal: Some(Proposal {
                op: ProposalOp::Add,
                kind: Some("requirement".into()),
                section: Some("Mock".into()),
                node: None,
                id: None,
                content_type: None,
                text: Some(format!("Mock requirement {n} holds.")),
                append: false,
                replaces: Vec::new(),
            }),
        }
    } else {
        QuestionDraft {
            covers: vec![format!("mock-area-{n}")],
            question: format!("Mock question {n}: which approach?"),
            options: vec!["Simple".into(), "Thorough".into()],
            ..Default::default()
        }
    }
}

fn answer_processor(
    client: &InterviewClient,
    row: &AgentSessionRow,
    text: &str,
    received: &str,
) -> Result<String> {
    let seqs: Vec<i64> = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Process: "))
        .map(|rest| {
            rest.trim_end_matches('.')
                .split(',')
                .filter_map(|part| part.trim().strip_prefix("q-")?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let mut processed = 0;
    for seq in seqs {
        let Some(q) =
            client.read(|conn| InterviewRepo::new(conn).get_question(row.node_id, seq))?
        else {
            continue;
        };
        if q.status != STATUS_ANSWERED || q.processed_at.is_some() {
            continue;
        }
        let summary = if q.question.is_none() {
            client.interview(InterviewCommand::AddMemory {
                node_id: row.node_id,
                kind: MEMORY_CONTEXT.into(),
                phase: None,
                body: format!(
                    "User said: {}",
                    q.answer_text.as_deref().unwrap_or_default()
                ),
                question_seq: Some(seq),
            })?;
            "Recorded the text as context.".to_string()
        } else if q.proposal.is_some() && q.answer_option == Some(1) {
            "Reviewed the applied proposal; no conflicts (mock).".to_string()
        } else if q.proposal.is_some() {
            client.interview(InterviewCommand::AddMemory {
                node_id: row.node_id,
                kind: MEMORY_HANDOFF.into(),
                phase: None,
                body: format!(
                    "The user did not accept the proposal in {}; notes: {}",
                    q.label(),
                    q.answer_text.as_deref().unwrap_or("(none)")
                ),
                question_seq: Some(seq),
            })?;
            "Handed off a follow-up.".to_string()
        } else {
            "Noted the answer (mock).".to_string()
        };
        client.interview(InterviewCommand::MarkProcessed {
            node_id: row.node_id,
            seq,
            summary,
        })?;
        processed += 1;
    }
    Ok(format!("processed {processed} ({received})"))
}
