//! `tod-cli tests` — the test run an implementation agent records, which is
//! how the app learns whether the work's tests pass.
//!
//! Only meaningful inside an implementation conversation: the conversation is
//! the one named by `TOD_IMPLEMENT_CONVERSATION`, which the app sets for the
//! agent.

use crate::Invocation;
use crate::args::Args;
use tod_core::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, TestRun};
use tod_store::conversation::ConversationRepo;
use tod_store::interview::InterviewCommand;
use uuid::Uuid;

pub(crate) const USAGE: &str = "\
tod-cli tests — record a test run for this implementation

Only works inside an implementation conversation (TOD_IMPLEMENT_CONVERSATION=<UUID>).

COMMANDS:
    record    --command <TEXT> --passed <N> [--failed <N>] [--errors <N>]

`record` stores the counts from the test run you just made. A later `record`
in the same turn replaces it, so record the final run.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "record" => record(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

fn record(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let conversation = conversation(inv)?;
    let command = args.require("--command")?.trim();
    anyhow::ensure!(!command.is_empty(), "--command must name the command you ran");
    let count = |flag: &str| -> anyhow::Result<u32> {
        match args.get(flag) {
            None => Ok(0),
            Some(raw) => raw
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{flag} must be a whole number (got `{raw}`)")),
        }
    };
    let run = TestRun {
        command: command.to_string(),
        passed: args.require("--passed").and_then(|_| count("--passed"))?,
        failed: count("--failed")?,
        errors: count("--errors")?,
    };
    inv.client().interview(InterviewCommand::RecordConversationReport {
        conversation_id: conversation,
        body: serde_json::to_value(&run)?,
    })?;
    if inv.json {
        return Ok(serde_json::to_string(&run)?);
    }
    Ok(format!("ok {}", run.label()))
}

/// The conversation this invocation records for, which must exist.
fn conversation(inv: &Invocation) -> anyhow::Result<Uuid> {
    let raw = std::env::var(IMPLEMENT_CONVERSATION_ENV).map_err(|_| {
        anyhow::anyhow!(
            "`tests` only works inside an implementation conversation: \
             {IMPLEMENT_CONVERSATION_ENV} is not set"
        )
    })?;
    let id = Uuid::parse_str(raw.trim())
        .map_err(|_| anyhow::anyhow!("{IMPLEMENT_CONVERSATION_ENV} is not a UUID (`{raw}`)"))?;
    inv.client().read(|conn| {
        ConversationRepo::new(conn)
            .get(id)?
            .map(|_| id)
            .ok_or_else(|| anyhow::anyhow!("conversation {id} not found"))
    })
}
