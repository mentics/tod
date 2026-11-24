//! A `broken` request about a project helper that is missing or hangs.
//!
//! A project's own skills sometimes name a helper (`git-info`, a wrapper
//! script) that is not installed where the agent runs. A judging session
//! (evaluator, verify, review, fix) that hits one used to file a `broken`
//! request asking the user to supply it, and the run stopped for something the
//! user cannot reasonably do and the session never needed: scope and base are
//! plain git (`git merge-base`, `git diff <base>...HEAD`). [`refusal`] turns
//! that request into an error pointing at the fallback. It is conservative:
//! only judging protocols, only `broken`, only when the failure reads as a
//! missing or hanging tool and does not involve `tod-cli` itself.

/// Protocols whose work is reading the change, so plain git always suffices.
const JUDGING: &[&str] = &["evaluate", "verification", "review", "fix", "phase", "triage"];

/// Phrases that say a command was absent or never answered.
const SYMPTOMS: &[&str] = &[
    "command not found",
    "not found",
    "not installed",
    "no such file",
    "hung",
    "hang",
    "timed out",
    "timeout",
    "no output",
    "interrupted",
    "not on path",
    "not available",
];

/// The error text a session is sent instead of the request.
pub const REFUSAL: &str = "not recorded: a project helper that is missing or hangs here (a `git-info`-style script a skill \
    mentions) is not something the user can supply, and you do not need it. Use plain git instead: the task's branch and base \
    are in your context; `git merge-base <base> HEAD` gives the fork point, `git diff <base>...HEAD --stat` and `git diff \
    <base>...HEAD -- <path>` give the scope and the change, and `git log <base>..HEAD` the commits (use `origin/<base>` if the \
    local ref is absent). Do not run the helper again. Carry on with the judgement, and record in your notes that the helper \
    was unavailable. Ask only if plain git itself fails.";

/// The refusal for a `broken` request, if it is one a session may not raise.
pub fn refusal(protocol: Option<&str>, reason: &str, question: &str, command: &str, error: &str) -> Option<&'static str> {
    if reason != "broken" || !protocol.is_some_and(|p| JUDGING.contains(&p)) {
        return None;
    }
    let all = format!("{question}\n{command}\n{error}").to_lowercase();
    if all.contains("tod-cli") || all.contains("tod_cli") {
        return None;
    }
    // Plain git failing is a real fault.
    let first = command.split_whitespace().next().unwrap_or("");
    if first == "git" {
        return None;
    }
    SYMPTOMS.iter().any(|s| all.contains(s)).then_some(REFUSAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluator_asking_for_git_info_is_redirected() {
        let why = refusal(
            Some("evaluate"),
            "broken",
            "A working git-info scope/base resolution for the independent verification audit",
            "git-info scope",
            "hung 80-90s with no output; interrupted",
        )
        .unwrap();
        assert!(why.contains("git merge-base"));
    }

    #[test]
    fn other_requests_pass() {
        // Not broken, not a judging protocol, tod-cli or git itself failing, no symptom.
        assert!(refusal(Some("evaluate"), "intent", "q", "git-info", "not found").is_none());
        assert!(refusal(Some("implement"), "broken", "q", "git-info", "not found").is_none());
        assert!(refusal(Some("review"), "broken", "q", "tod-cli plan list", "not found").is_none());
        assert!(refusal(Some("review"), "broken", "q", "git diff x", "no such file").is_none());
        assert!(refusal(Some("review"), "broken", "q", "", "disk full").is_none());
    }
}
