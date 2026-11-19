//! The hidden operator tier of `tod-cli` (`TOD_OPERATOR=shepherd`).
//!
//! `runner` and `decisions answer` do what the task panel's runner menu and
//! requests do, for a Claude "shepherd" that works without the app's window.
//! They are not part of what an agent is told: the nouns' fragments under
//! `media/context/cli/` never mention them and `doc_sync` is pinned to the
//! visible `USAGE` strings only. With `TOD_OPERATOR` unset they do not exist:
//! no help lists them and invoking one is an unknown noun or command, like any
//! other. With it set they appear in `--help`, `help` and its search, and are
//! documented in `.claude/skills/shepherd-common/operator-cli.md`, which a test
//! here pins to the usage below.
//!
//! Either way they are refused in an agent session (`TOD_INTERVIEW_ACTOR`
//! set), and they record who acted: `TOD_ACTOR=shepherd:<id>`, else the
//! operator.


pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The process environment.
pub(crate) fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(crate) fn enabled(env: Env) -> bool {
    tod_core::runner_operator::enabled(env)
}

/// An error in an agent session, even with the tier on.
pub(crate) fn refuse_agent_session(what: &str, env: Env) -> anyhow::Result<()> {
    tod_core::lifecycle::operator::refuse_agent_session_for(what, "", env)
}

pub(crate) const RUNNER_SUMMARY: &str =
    "runner                 The task panel's runner menu: what it offers and each entry (operator tier)";

pub(crate) const RUNNER_USAGE: &str = "\
tod-cli runner — what the task panel's runner menu does (operator tier)

COMMANDS:
    menu <NODE>
    start <NODE>
    resume <NODE>
    resume-fresh-budget <NODE>
    run-fresh <NODE>
    pause <NODE>
    stop-now <NODE>
    check-now <NODE>
    rerun-phase <NODE>
    migrate <NODE> --to <LOCATION> [--commit | --discard]
    reset-environment <NODE>
    update-workspace <NODE>
    delete-sandbox <NODE> [--stop]
    delete-sandbox-anyway <NODE> [--stop]
    sync-now <NODE>
    adopt-sandbox <NODE>

<NODE> is a node slug or full UUID. `menu` lists what the split button would
offer now, from the same rules and facts as the panel: its default action,
then its menu entries, each as the verb that does it. Every other verb is one
of those entries, does what the panel's click does where the node's runner
runs, and is refused when the menu does not offer it now (the refusal says
what it does offer). A node whose files are on this machine is run by the
daemon, tod-agentd, which the app starts: when it is not running the verb fails
saying so. A node in a cloud sandbox is run by its supervisor there.
`start`, `resume`, `resume-fresh-budget` and `run-fresh` start or continue the
run (a new session, or the turn and time budget reset); `pause` stops it once
the turn ends and `stop-now` cancels the turn in flight; `check-now` checks the
phase's gate with no agent and moves the node on when it is clear;
`rerun-phase` runs the current phase again in a new session.
`migrate` moves the node's files to the location named by --to (a name or id;
`menu` lists them): its sessions are kept, its current files are removed with
each branch pushed first, and nothing is started. It refuses while the run is
active, and when the files have uncommitted changes unless --commit commits
them on the branch first or --discard throws them away.
`delete-sandbox` deletes the node's sandbox after a checkpoint of its
uncommitted work, `delete-sandbox-anyway` without one; each refuses while the
run is active unless --stop stops it first. `reset-environment`, `update-workspace`,
`sync-now` and `adopt-sandbox` are the entries of the same names.
Refused in an agent session (TOD_INTERVIEW_ACTOR set). With TOD_ACTOR=shepherd:<ID>
the act is left on the node's activity as that shepherd's, else as the operator's.
";

pub(crate) const STATS_SUMMARY: &str =
    "stats                  How the lifecycle is going: cost, review, PR, requests, stops, sandbox churn, git retries (operator tier)";

pub(crate) const STATS_USAGE: &str = "\
tod-cli stats — how the lifecycle is going, from what tod already keeps (operator tier)

COMMANDS:
    stats [<NODE>] [--section <NAME>] [--since <DURATION>]

Read-only. One report over the store with a section per question: `cost`
(per node and phase: sessions, agent turns, tool calls, wall time), `review`
(rounds, findings, clean rounds, reopened findings, rejected/declined ratio),
`pr-draft` (PR agent sessions, draft review rounds, threads posted, pushes, CI
failures), `requests` (how many, how long they waited, answered with an offered
option or carry-on words, about tod's setup), `stops` (runs stopped now by
reason, and who resumes them), `sandbox` (locations made and removed,
replacements, losses) and `git-retries` (git calls tod retried after a ref-lock
race or a network error, per node: calls, retries, recovered, gave up, by
operation; a call that gave up also leaves a note on the node's activity). <NODE> (a slug or UUID) narrows it to one node;
--section to one section; --since (30m, 2h, 7d) to what happened in that
window. Every section ends with `not available:` lines naming what tod does
not keep (tokens, bot reviews, stop history, setup time), so a missing figure
is never read as a zero. --json gives the same as JSON.
Refused in an agent session (TOD_INTERVIEW_ACTOR set).
";

/// The command line `decisions answer` adds to `decisions`'s own.
pub(crate) const DECISIONS_ANSWER_COMMAND: &str = "    answer <ID> (--option <N> | --text <TEXT>)\n";

pub(crate) const DECISIONS_ANSWER_NOTES: &str = "\
`answer` (operator tier) answers request <ID> exactly as the task panel does:
--option picks option N (1-based; an option that moves the node back moves it,
as the user's own move, before the answer is recorded, and a permission's answer
is a standing grant), --text gives a free-text answer, and one of them is required.
A silent review bot's answer releases the run parked on it, and a node in a cloud
sandbox is woken. The in-app conversation that asked is not sent a message: the
answer reaches the agent's next turn in the node's answered questions.
Refused in an agent session (TOD_INTERVIEW_ACTOR set). With TOD_ACTOR=shepherd:<ID>
the answer is recorded as that shepherd's, else as the operator's.
";

/// The top-level usage, with the operator nouns listed when the tier is on.
pub(crate) fn top_usage(top: &str, on: bool) -> String {
    if !on {
        return top.to_string();
    }
    let anchor = "    decisions              What the user answers: ask, list, show\n";
    top.replace(
        anchor,
        &format!(
            "    decisions              What the user answers: ask, list, show, answer (operator tier)\n    {RUNNER_SUMMARY}\n    {STATS_SUMMARY}\n"
        ),
    )
}

/// `decisions`'s usage, with `answer` when the tier is on.
pub(crate) fn decisions_usage(visible: &str, on: bool) -> String {
    if !on {
        return visible.to_string();
    }
    let mut out = visible.replace("    show  <ID>\n", &format!("    show  <ID>\n{DECISIONS_ANSWER_COMMAND}"));
    out.push_str(DECISIONS_ANSWER_NOTES);
    out
}

/// Every noun and its usage text as `--help` and `help` show them: the
/// visible ones, with the operator additions when the tier is on.
pub(crate) fn nouns(on: bool) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = crate::NOUNS
        .iter()
        .map(|&(noun, usage)| (noun, if noun == "decisions" { decisions_usage(usage, on) } else { usage.to_string() }))
        .collect();
    if on {
        out.push(("runner", RUNNER_USAGE.to_string()));
        out.push(("stats", STATS_USAGE.to_string()));
    }
    out
}

/// The usage of `noun` for `--help`, or `None` when it is not a noun here.
pub(crate) fn noun_usage(noun: &str, on: bool) -> Option<String> {
    nouns(on).into_iter().find(|(n, _)| *n == noun).map(|(_, usage)| usage)
}

/// The names of the environment variables the tier reads, for the docs.
#[cfg(test)]
pub(crate) const ENV_NAMES: [&str; 2] = [
    tod_core::runner_operator::OPERATOR_ENV,
    tod_core::runner_operator::ACTOR_ENV,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The verbs under `COMMANDS:` of the hidden usage.
    fn runner_verbs() -> Vec<&'static str> {
        crate::doc_sync::verbs(RUNNER_USAGE)
    }

    fn skill_doc() -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.claude/skills/shepherd-common/operator-cli.md");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn the_hidden_usage_names_every_runner_verb() {
        let verbs = runner_verbs();
        for verb in tod_core::runner_operator::Verb::ALL {
            assert!(verbs.contains(&verb.name()), "the runner usage does not list `{}`", verb.name());
        }
        assert!(verbs.contains(&"menu"));
        assert_eq!(verbs.len(), tod_core::runner_operator::Verb::ALL.len() + 1, "{verbs:?}");
    }

    /// The operator doc shows every hidden verb, and invents none.
    #[test]
    fn the_skill_doc_is_pinned_to_the_hidden_usage() {
        let doc = skill_doc();
        for verb in runner_verbs() {
            assert!(doc.contains(&format!("tod-cli runner {verb}")), "operator-cli.md never shows `runner {verb}`");
        }
        assert!(doc.contains("tod-cli decisions answer"), "operator-cli.md never shows `decisions answer`");
        let real = runner_verbs();
        for line in doc.lines() {
            let Some(rest) = line.split("tod-cli runner ").nth(1) else { continue };
            let claimed = rest.split_whitespace().next().unwrap_or_default();
            if claimed.is_empty() || !claimed.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                continue;
            }
            assert!(real.contains(&claimed), "operator-cli.md documents `runner {claimed}`, which does not exist");
        }
        for name in ENV_NAMES {
            assert!(doc.contains(name), "operator-cli.md never mentions {name}");
        }
        // Every flag of the hidden usage is in the doc.
        for flag in ["--to", "--commit", "--discard", "--stop", "--option", "--text"] {
            assert!(doc.contains(flag), "operator-cli.md never mentions {flag}");
        }
    }

    /// Nothing an agent reads mentions the hidden verbs.
    #[test]
    fn the_agent_facing_fragments_do_not_mention_the_hidden_verbs() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tod/media/context");
        let mut stack = vec![dir];
        let mut seen = 0;
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                    continue;
                }
                let text = std::fs::read_to_string(&entry).unwrap_or_default();
                seen += 1;
                assert!(!text.contains("tod-cli runner"), "{} mentions `tod-cli runner`", entry.display());
                assert!(!text.contains("decisions answer"), "{} mentions `decisions answer`", entry.display());
                assert!(!text.contains(ENV_NAMES[0]), "{} mentions {}", entry.display(), ENV_NAMES[0]);
            }
        }
        assert!(seen > 10, "found only {seen} fragments");
    }

    #[test]
    fn the_nouns_gain_runner_and_answer_only_when_on() {
        let off = nouns(false);
        assert!(off.iter().all(|(n, _)| *n != "runner"));
        let decisions_off = &off.iter().find(|(n, _)| *n == "decisions").unwrap().1;
        assert!(!decisions_off.contains("    answer <ID>"), "{decisions_off}");
        assert_eq!(decisions_off, crate::decisions::USAGE);

        let on = nouns(true);
        assert!(on.iter().any(|(n, _)| *n == "runner"));
        assert!(on.iter().any(|(n, _)| *n == "stats"));
        assert!(off.iter().all(|(n, _)| *n != "stats"));
        assert!(!crate::USAGE.contains("stats"));
        let decisions_on = &on.iter().find(|(n, _)| *n == "decisions").unwrap().1;
        assert!(decisions_on.contains("    answer <ID> (--option <N> | --text <TEXT>)"), "{decisions_on}");
        assert!(top_usage(crate::USAGE, false) == crate::USAGE);
        assert!(top_usage(crate::USAGE, true).contains("runner"));
        assert!(!crate::USAGE.contains("runner"));
    }
}
