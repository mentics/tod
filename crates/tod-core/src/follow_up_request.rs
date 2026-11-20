//! A request an agent may not raise: "should we open a follow-up ticket?".
//!
//! Out-of-scope work is a **suggestion** (`tod-cli suggest ticket`,
//! `doc/requests/design.md` sections 7.3 and 9), never a decision: it must not
//! stop the node or sit among the requests that need the user. The role docs
//! say so, but a prompt is only a request, so `tod-cli decisions ask` refuses
//! a question that is plainly about opening a ticket and sends the agent to
//! `suggest ticket` instead ([`refusal`]).
//!
//! The check is deliberately conservative: it reads only the question's first
//! line (the part the user must decide) and wants a verb of creating a ticket
//! right before the word "ticket" ("open a follow-up ticket", "file a new
//! Linear ticket"), in something asked as a question. A question that merely
//! mentions a ticket ("the ticket says X, which do you want?") passes.

/// The error text an agent is sent instead of the request.
pub const REFUSAL: &str = "not recorded: whether to open a follow-up ticket is never a question. File it as a suggestion with \
    `tod-cli suggest ticket --title <TEXT> --body <TEXT>` (the ticket exactly as it should be created: description and \
    acceptance criteria) and carry on with this task's work. The user accepts or dismisses it when it suits them.";

/// Verbs of creating a ticket.
const VERBS: &[&str] = &[
    "open", "create", "file", "raise", "make", "log", "cut", "add", "spin", "write", "record", "track",
];

/// Words that make a first line a question rather than a statement.
const QUESTION_WORDS: &[&str] = &[
    "should", "shall", "whether", "want", "would", "ok", "okay", "ought", "need", "decide", "confirm", "approve",
];

/// How many words before "ticket" a creating verb may stand.
const REACH: usize = 4;

/// Whether `question`'s first line asks if a ticket should be created.
pub fn is_follow_up_ticket_question(question: &str) -> bool {
    let Some(line) = question.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return false;
    };
    let words: Vec<String> = line
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let asks = line.contains('?') || words.iter().any(|w| QUESTION_WORDS.contains(&w.as_str()));
    if !asks {
        return false;
    }
    words.iter().enumerate().any(|(i, w)| {
        matches!(w.as_str(), "ticket" | "tickets")
            && words[i.saturating_sub(REACH)..i].iter().any(|before| VERBS.contains(&before.as_str()))
    })
}

/// The refusal for `question`, if it is one an agent may not raise.
pub fn refusal(question: &str) -> Option<&'static str> {
    is_follow_up_ticket_question(question).then_some(REFUSAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_ticket_questions_are_refused() {
        for q in [
            "Should we open a follow-up ticket for the flaky retry?",
            "Decide whether to create a new Linear ticket for the export bug.",
            "Open a ticket for the dead code in billing?",
            "Do you want me to file a separate ticket for this?\n\nWhy: it is out of scope.",
            "Shall I create a ticket for the missing index",
        ] {
            assert!(is_follow_up_ticket_question(q), "{q}");
            assert!(refusal(q).unwrap().contains("tod-cli suggest ticket"));
        }
    }

    #[test]
    fn other_questions_are_left_alone() {
        for q in [
            "Decide whether the CSV export may drop rows with no email.",
            "The ticket says to create a cache, but the design forbids it. Which do you want?",
            "Should I update the ticket's description?",
            "Grant read access to the billing database.",
            "Create a ticket-based login flow?",
            "",
            "Decide which approach to take.\n\nIt may need a follow-up ticket later: should we open a follow-up ticket?",
        ] {
            assert!(!is_follow_up_ticket_question(q), "{q}");
        }
    }
}
