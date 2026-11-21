//! What the user reads for an access, broken or capability request: written by
//! the app from the structured fields the agent filed
//! (`tod_store::decisions::Brief`: `decisions ask --need --command --error
//! --tried --after`), never by a model (`doc/requests/design.md` 7.1).
//!
//! This is the one template. The task panel and every other surface that shows
//! such a request ask here for its parts or its text, so improving the way
//! these requests read means editing this file only. A request without the
//! fields (an older one, or any other reason) is [`None`]: it is shown as it
//! always was, from its question.

use tod_store::decisions::Decision;

/// The parts of a rendered request, in the order they are shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBrief {
    /// The first line: what is needed.
    pub need: String,
    /// The exact command to paste, shown in a copy block.
    pub command: Option<String>,
    /// The exact error.
    pub error: Option<String>,
    /// What the agent tried, one entry per attempt.
    pub tried: Vec<String>,
    /// What happens once the user is done.
    pub after: Option<String>,
}

/// The request's parts, when the agent filed them.
pub fn of(decision: &Decision) -> Option<RequestBrief> {
    let brief = &decision.brief;
    if brief.is_empty() {
        return None;
    }
    let some = |text: &str| Some(text.trim().to_string()).filter(|t| !t.is_empty());
    let need = some(&brief.need).unwrap_or_else(|| first_line(&decision.question));
    Some(RequestBrief {
        need,
        command: some(&brief.command),
        error: some(&brief.error),
        tried: brief.tried.iter().filter_map(|t| some(t)).collect(),
        after: some(&brief.after),
    })
}

/// What names the request in a list or a summary: its need, else its question.
pub fn headline(decision: &Decision) -> String {
    match of(decision) {
        Some(brief) => brief.need,
        None => decision.question.clone(),
    }
}

impl RequestBrief {
    /// Everything but the command, as markdown: the error (a code block), what
    /// was tried, and what happens after. Empty when there is none of those.
    /// The command is left out so a surface can give it a copy block of its own.
    pub fn details_markdown(&self) -> String {
        let mut sections: Vec<String> = Vec::new();
        if let Some(error) = &self.error {
            sections.push(format!("**Error**\n\n{}", code_block(error)));
        }
        if !self.tried.is_empty() {
            let list: Vec<String> = self.tried.iter().map(|t| format!("- {t}")).collect();
            sections.push(format!("**Tried**\n\n{}", list.join("\n")));
        }
        if let Some(after) = &self.after {
            sections.push(format!("**After this:** {after}"));
        }
        sections.join("\n\n")
    }

    /// The whole request as markdown, the command in a code block: for a
    /// surface with no copy block, and for what is sent to an agent or logged.
    pub fn markdown(&self) -> String {
        let mut out = self.need.clone();
        if let Some(command) = &self.command {
            out.push_str("\n\n");
            out.push_str(&code_block(command));
        }
        let details = self.details_markdown();
        if !details.is_empty() {
            out.push_str("\n\n");
            out.push_str(&details);
        }
        out
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// A fenced block whose fence is longer than any run of backticks inside.
fn code_block(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::decisions::Brief;
    use uuid::Uuid;

    fn decision(question: &str, brief: Brief) -> Decision {
        Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: None,
            question: question.into(),
            options: vec![],
            evidence: vec![],
            status: "pending".into(),
            created_at: 0,
            reason: "access".into(),
            moves: vec![],
            edits: vec![],
            blocks: vec![],
            new_evidence: String::new(),
            brief,
            error_signature: String::new(),
            cause: String::new(),
            triage_conversation_id: None,
            asked: None,
        }
    }

    #[test]
    fn an_older_request_has_no_brief_and_reads_as_its_question() {
        let d = decision("Can you give me the key?\nMore text", Brief::default());
        assert!(of(&d).is_none());
        assert_eq!(headline(&d), "Can you give me the key?\nMore text");
    }

    #[test]
    fn the_template_puts_the_need_first_then_the_command_error_tried_and_after() {
        let d = decision(
            "ignored",
            Brief {
                need: "Run the Dagster job on prod".into(),
                command: "dagster job launch -j repoint".into(),
                error: "AccessDenied: no".into(),
                tried: vec!["ran it as me".into(), "asked the API".into()],
                after: "I check the output".into(),
            },
        );
        let brief = of(&d).unwrap();
        assert_eq!(headline(&d), "Run the Dagster job on prod");
        assert_eq!(
            brief.markdown(),
            "Run the Dagster job on prod\n\n```\ndagster job launch -j repoint\n```\n\n\
             **Error**\n\n```\nAccessDenied: no\n```\n\n**Tried**\n\n- ran it as me\n- asked the API\n\n\
             **After this:** I check the output"
        );
        assert!(!brief.details_markdown().contains("dagster job"));
    }

    #[test]
    fn missing_parts_are_left_out_and_the_need_falls_back_to_the_question() {
        let d = decision("Need the VPN\nlong text", Brief { after: "I retry".into(), ..Default::default() });
        let brief = of(&d).unwrap();
        assert_eq!(brief.need, "Need the VPN");
        assert_eq!(brief.markdown(), "Need the VPN\n\n**After this:** I retry");
    }

    #[test]
    fn a_command_with_backticks_gets_a_longer_fence() {
        let d = decision("q", Brief { need: "n".into(), command: "echo ```x```".into(), ..Default::default() });
        assert!(of(&d).unwrap().markdown().contains("````\necho ```x```\n````"));
    }
}
