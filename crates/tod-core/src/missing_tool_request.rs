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

/// Phrases in `--error` that say the helper was absent or never answered.
const SYMPTOMS: &[&str] = &[
    "command not found",
    "not installed",
    "not on path",
    "hung",
    "hangs",
    "timed out",
    "no output",
    "interrupted",
];

/// Programs that are never a project helper: a failure of one is a real
/// failure, whatever its error says.
const KNOWN_TOOLS: &[&str] = &[
    "git", "tod-cli", "tod", "cargo", "rustc", "gh", "docker", "docker-compose", "npm", "npx", "pnpm", "yarn", "node",
    "bun", "python", "python3", "pip", "pip3", "uv", "psql", "pg_dump", "mysql", "redis-cli", "curl", "wget", "ssh",
    "make", "go", "java", "mvn", "gradle", "terraform", "kubectl", "aws", "sh", "bash", "zsh", "sudo", "env",
];

/// The error text a session is sent instead of the request.
pub const REFUSAL: &str = "not recorded: a project helper that is missing or hangs here (a `git-info`-style script a skill \
    mentions) is not something the user can supply, and you do not need it. Use plain git instead: the task's branch and base \
    are in your context; `git merge-base <base> HEAD` gives the fork point, `git diff <base>...HEAD --stat` and `git diff \
    <base>...HEAD -- <path>` give the scope and the change, and `git log <base>..HEAD` the commits (use `origin/<base>` if the \
    local ref is absent). Do not run the helper again. Carry on with the judgement, and record in your notes that the helper \
    was unavailable. Ask only if plain git itself fails.";

/// The refusal for a `broken` request, if it is one a session may not raise:
/// the `--command`'s program is not a known tool (so it is a project helper)
/// and `--error` says it was not found or that it hung, timed out or printed
/// nothing.
pub fn refusal(protocol: Option<&str>, reason: &str, _question: &str, command: &str, error: &str) -> Option<&'static str> {
    if reason != "broken" || !protocol.is_some_and(|p| JUDGING.contains(&p)) {
        return None;
    }
    let first = command.split_whitespace().next()?;
    let program = first.rsplit('/').next().unwrap_or(first);
    if KNOWN_TOOLS.contains(&program) {
        return None;
    }
    let error = error.to_lowercase();
    SYMPTOMS.iter().any(|s| error.contains(s)).then_some(REFUSAL)
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
    fn legitimate_failures_pass_through() {
        for (command, error) in [
            ("cargo test -p tod-core", "timed out after 600s"),
            ("psql -h db -c 'select 1'", "connection refused: database not installed"),
            ("docker exec app true", "hung with no output"),
            ("gh pr view 12", "command not found"),
            ("git worktree add x", "no such file or directory"),
            ("tod-cli plan list", "command not found"),
            ("./scripts/check", "exit 1: assertion failed"),
            ("", "timed out"),
        ] {
            assert!(refusal(Some("review"), "broken", "q", command, error).is_none(), "{command}");
        }
        assert!(refusal(Some("evaluate"), "intent", "q", "git-info", "command not found").is_none());
        assert!(refusal(Some("implementation"), "broken", "q", "git-info", "command not found").is_none());
    }

    #[test]
    fn missing_helper_by_path_is_redirected() {
        assert!(refusal(Some("verification"), "broken", "q", "/usr/local/bin/git-info base", "bash: git-info: command not found").is_some());
    }
}
