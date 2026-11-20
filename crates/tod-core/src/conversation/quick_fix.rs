//! The quick fix (`doc/requests/design.md` section 5, "rung 1"): one cheap,
//! narrow session (Haiku at low effort unless the lifecycle config key
//! `quick_fix` says otherwise) that tries to make one failing thing pass before
//! the full Fix protocol is asked.
//!
//! It is given the failing check (its label, command and output tail), the
//! part of the branch's diff that touches the files the output names, and the
//! repository's usual commands (the phase's configured checks): no obligations,
//! no plan, no design. It edits files in the node's checkout and runs that
//! check's command. It may not touch obligations, the plan or decisions
//! (`tod_store::conversation::settled`, `tod-cli decisions ask`).
//!
//! What it works on is the node's open [`QuickFix`] row
//! (`tod_store::quick_fixes`), made by the autopilot before the session starts.
//! The row records the attempt and its outcome, per check, so how often each
//! check's quick fixes fail can be measured.
//!
//! Nothing in the reply is parsed. After each turn tod runs only that check
//! again ([`crate::checks::run_one`]): green is success, and the loop stops
//! after `lifecycle.quick_fix_turns` turns (3 by default) or when nothing
//! changed. A failing CI check has no command to run here, so its session
//! takes one turn and its outcome is read from the pull request afterwards.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, commit_run, node_id, worktree_fingerprint};
use super::protocol::{Next, Protocol, ProtocolEnv, RunNotice, Stop, TurnContext};
use crate::context_recipes::{QUICK_FIX, build_message};
use crate::dynamic::DynamicContext;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tod_store::check_runs::Verdict;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::fleet::{FleetMutation, FleetStore, Workdir};
use tod_store::quick_fixes::{self, OUTCOME_ESCALATED, OUTCOME_SUCCEEDED, QuickFix, SOURCE_CI};
use uuid::Uuid;

/// The most of the branch's diff a quick fix is shown.
const DIFF_BYTES: usize = 12 * 1024;

/// The most of a reply kept as the notes the Fix session is handed.
const NOTES_CHARS: usize = 4000;

/// The most files an output may name before the rest are left out.
const MAX_FILES: usize = 20;

pub struct QuickFixProtocol;

/// The open attempt this conversation is working on: the one it is linked
/// to, else the node's open one.
fn current(env: &ProtocolEnv<'_>) -> Result<Option<QuickFix>> {
    let node = node_id(env)?;
    env.fleet.read(|conn| match quicker(conn, env.conversation_id)? {
        Some(fix) => Ok(Some(fix)),
        None => quick_fixes::open_for_node(conn, node),
    })
}

fn quicker(conn: &rusqlite::Connection, conversation: Uuid) -> Result<Option<QuickFix>> {
    quick_fixes::for_conversation(conn, conversation)
}

/// Write `fix` to its table, through the writer, before returning.
pub fn save(fleet: &FleetStore, fix: &QuickFix) -> Result<()> {
    fleet.enqueue(FleetMutation::RecordQuickFix(Box::new(fix.clone())))?;
    fleet.writer().flush()?;
    Ok(())
}

/// The directory a check ran in and its command, from the attempt's check key.
pub fn dir_and_command(fix: &QuickFix) -> (String, String) {
    match fix.check_key.split_once('\u{1f}') {
        Some((dir, command)) => (dir.to_string(), command.to_string()),
        None => (String::new(), fix.command.clone()),
    }
}

/// How many turns a quick fix may take (`lifecycle.quick_fix_turns`, at least one).
pub fn turn_limit(data_root: &std::path::Path) -> Result<u32> {
    Ok(tod_store::TodSettings::load(&tod_store::TodPaths::at(data_root))?.lifecycle.quick_fix_turns.max(1))
}

/// The repository-relative files `output` names that exist in `cwd`'s
/// repository (tracked, or untracked and not ignored), at most [`MAX_FILES`],
/// in the order the output first names them.
pub fn named_files(cwd: &Workdir, output: &str) -> Result<Vec<String>> {
    let mut candidates: Vec<String> = Vec::new();
    for token in output.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\'' | '`' | ',' | ';' | '=' | '<' | '>' | '[' | ']')) {
        let mut token = token.trim_start_matches("./").trim_start_matches("a/").trim_start_matches("b/");
        // `path:12:3` and `path:12` name the file.
        while let Some((head, tail)) = token.rsplit_once(':') {
            if tail.chars().all(|c| c.is_ascii_digit()) {
                token = head;
            } else {
                break;
            }
        }
        let token = token.trim_end_matches(':');
        if token.len() < 3 || token.len() > 200 || !(token.contains('/') || token.contains('.')) {
            continue;
        }
        if token.chars().any(|c| matches!(c, '*' | '?' | '[' | '\\' | ':')) || token.starts_with('/') || token.starts_with('-') {
            continue;
        }
        if !candidates.iter().any(|c| c == token) {
            candidates.push(token.to_string());
        }
        if candidates.len() >= 200 {
            break;
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut args: Vec<&str> = vec!["ls-files", "--cached", "--others", "--exclude-standard", "--"];
    args.extend(candidates.iter().map(String::as_str));
    let listed = cwd.git(&args).context("find which files the failing output names")?;
    let present: std::collections::HashSet<&str> = listed.lines().collect();
    Ok(candidates.into_iter().filter(|c| present.contains(c.as_str())).take(MAX_FILES).collect())
}

/// The part of the branch's change (committed against where it left the
/// default branch, and uncommitted) that touches `files`, at most
/// [`DIFF_BYTES`] of it. Empty when `files` is.
pub fn diff_for_files(cwd: &Workdir, files: &[String]) -> Result<String> {
    if files.is_empty() {
        return Ok(String::new());
    }
    let base = tod_store::fleet::worktree::new_branch_base(cwd)
        .context("find the branch the change is measured against: no origin/HEAD, origin/main or origin/master")?;
    let merge_base = cwd.git(&["merge-base", &base, "HEAD"]).with_context(|| format!("find where this branch left {base}"))?;
    let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff", "-U3", &merge_base, "--"];
    args.extend(files.iter().map(String::as_str));
    let mut diff = cwd.git(&args).context("read the branch's diff of the named files")?;
    if diff.len() > DIFF_BYTES {
        let mut cut = DIFF_BYTES;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n... (the rest of the diff is left out)\n");
    }
    Ok(diff)
}

/// The text under "The failing check": the check, its output, the diff of the
/// files the output names, and the commands the project usually runs.
pub fn render_target(fix: &QuickFix, named: &[String], diff: Result<String, String>, usual: &[(String, String, String)]) -> String {
    let (dir, command) = dir_and_command(fix);
    let mut out = String::new();
    if fix.source == SOURCE_CI {
        out.push_str(&format!("Pull request check **{}** is failing.\n\n", fix.check_name));
        out.push_str(
            "You cannot run it here: it runs on the pull request's server. Run the project's own commands below that \
             cover the same ground, if any does, and make the change you believe fixes it. You have one turn.\n\n",
        );
        if !fix.output.trim().is_empty() {
            out.push_str(&format!("How it fails:\n\n```\n{}\n```\n\n", fix.output.trim()));
        }
    } else {
        out.push_str(&format!("Check **{}** is failing.\n\n", fix.check_name));
        out.push_str(&format!("- Command: `{command}`\n"));
        out.push_str(&format!("- Run it from: {}\n\n", if dir.is_empty() { "the root of your working directory".to_string() } else { format!("`{dir}`") }));
        out.push_str(&format!("The end of its output:\n\n```\n{}\n```\n\n", fix.output.trim()));
    }
    if named.is_empty() {
        out.push_str("The output names no file of the repository.\n\n");
    } else {
        out.push_str(&format!("Files the output names: {}.\n\n", named.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")));
    }
    match diff {
        Ok(diff) if diff.trim().is_empty() => {
            out.push_str("The branch's change does not touch those files.\n\n");
        }
        Ok(diff) => {
            out.push_str(&format!("The part of the branch's change that touches them:\n\n```diff\n{}\n```\n\n", diff.trim_end()));
        }
        Err(why) => {
            out.push_str(&format!("The branch's change could not be read, so it is not shown: {why}\n\n"));
        }
    }
    if !usual.is_empty() {
        out.push_str("The commands the project usually runs on this work:\n\n");
        for (label, dir, command) in usual {
            let place = if dir.trim().is_empty() || dir.trim() == "." { String::new() } else { format!(" (in `{}`)", dir.trim()) };
            if label.trim().is_empty() {
                out.push_str(&format!("- `{command}`{place}\n"));
            } else {
                out.push_str(&format!("- {label}: `{command}`{place}\n"));
            }
        }
    }
    out
}

/// The opening message for `fix`'s session, working in `cwd`.
pub fn opening_for(fleet: &FleetStore, media: &crate::media::MediaPaths, data_root: &std::path::Path, fix: &QuickFix, cwd: &Workdir) -> Result<String> {
    let named = named_files(cwd, &fix.output)?;
    let diff = diff_for_files(cwd, &named).map_err(|err| format!("{err:#}"));
    let usual: Vec<(String, String, String)> = fleet
        .read(|conn| crate::checks::configured(conn, fix.node_id, &fix.phase))?
        .into_iter()
        .map(|c| (c.label, c.dir, c.command))
        .collect();
    let target = render_target(fix, &named, diff, &usual);
    let working_dir = PathBuf::from(cwd.path_text());
    // Nothing of the node is in the context: only where the work is and the failure.
    let ctx = DynamicContext { data_root: Some(data_root), working_dir: Some(&working_dir), quick_fix: &target, ..Default::default() };
    build_message(media, &QUICK_FIX, None, &ctx, "")
}

impl Protocol for QuickFixProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::QuickFix
    }

    fn surface(&self) -> &'static str {
        crate::session_name::QUICK_FIX_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Make the failing check pass.")
    }

    /// The node's checkout: the check runs, and the change lands, there.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        let node = node_id(env)?;
        resolve_launch_cwd(env.fleet, &node.to_string())
    }

    /// Links the open attempt to this conversation.
    fn prepare(&self, env: &ProtocolEnv<'_>) -> Result<()> {
        if let Some(mut fix) = current(env)?
            && fix.conversation_id != Some(env.conversation_id)
        {
            fix.conversation_id = Some(env.conversation_id);
            save(env.fleet, &fix)?;
        }
        Ok(())
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(IMPLEMENT_CONVERSATION_ENV.to_string(), env.conversation_id.to_string())];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let fix = current(env)?.with_context(|| format!("no quick fix is open for node {}", node_id(env).map(|n| n.to_string()).unwrap_or_default()))?;
        opening_for(env.fleet, env.media, env.data_root, &fix, &self.cwd(env)?)
    }

    fn resume_snapshot(&self, env: &ProtocolEnv<'_>, _budget_tokens: i64, _before_seq: Option<i64>) -> Result<String> {
        self.opening(env)
    }

    fn loops(&self) -> bool {
        true
    }

    /// What the agent says last is what a Fix session is handed if this
    /// fails: it is kept on the attempt.
    fn on_reply(&self, env: &ProtocolEnv<'_>, reply: &str) -> Vec<RunNotice> {
        let save_notes = || -> Result<()> {
            if let Some(mut fix) = current(env)? {
                fix.notes = reply.trim().chars().take(NOTES_CHARS).collect();
                save(env.fleet, &fix)?;
            }
            Ok(())
        };
        match save_notes() {
            Ok(()) => Vec::new(),
            Err(err) => vec![RunNotice::Error(format!("could not keep the quick fix's notes: {err:#}"))],
        }
    }

    /// The files as they are: what changed in a turn is progress.
    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        Ok(Some(worktree_fingerprint(&self.cwd(env)?)))
    }

    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        let env = turn.env;
        let mut fix = current(env)?.context("a quick fix turn ended with no quick fix open")?;
        fix.turns = i64::from(turn.continuations) + 1;
        if fix.source == SOURCE_CI {
            // Nothing to run here: the pull request's checks say, afterwards.
            save(env.fleet, &fix)?;
            return Ok(Next::Done(Stop::Complete));
        }
        let limit = i64::from(turn_limit(env.data_root)?);
        let cwd = self.cwd(env)?;
        let files_now = crate::checks::fingerprint(&cwd)?;
        let mut still = String::new();
        if files_now == fix.fingerprint {
            still.push_str("You changed nothing since the check last failed, so it was not run again.\n\n");
        } else {
            let (dir, command) = dir_and_command(&fix);
            let run = crate::checks::run_one(env.fleet, fix.node_id, &fix.phase, &dir, &command)?;
            fix.fingerprint = files_now;
            match run.verdict {
                Verdict::Passed | Verdict::Skipped => {
                    fix.finish(OUTCOME_SUCCEEDED);
                    save(env.fleet, &fix)?;
                    return Ok(Next::Done(Stop::Complete));
                }
                Verdict::Broken => {
                    fix.finish(OUTCOME_ESCALATED);
                    fix.output = run.output.clone();
                    fix.notes = format!("{}\n\nThe check could not run afterwards: {}", fix.notes.trim(), crate::checks::broken_why(&run)).trim().to_string();
                    save(env.fleet, &fix)?;
                    return Ok(Next::Done(Stop::HandBack("the check could not run after the quick fix".to_string())));
                }
                Verdict::Failed => {
                    fix.output = run.output.clone();
                    still.push_str(&format!("tod ran the check again and it still fails. The end of its output:\n\n```\n{}\n```\n\n", run.output.trim()));
                }
            }
        }
        if fix.turns >= limit || !turn.progressed {
            fix.finish(OUTCOME_ESCALATED);
            save(env.fleet, &fix)?;
            return Ok(Next::Done(Stop::HandBack("the quick fix did not make the check pass".to_string())));
        }
        save(env.fleet, &fix)?;
        Ok(Next::Continue {
            message: format!("{still}Make the next most likely change and run the check again. Edit files and nothing else."),
            reason: "the check still fails".to_string(),
        })
    }

    /// A passing check's change is committed (the app's own commit, on the
    /// node's branch); a CI check's change too, since nothing here can say it
    /// failed. A failed attempt leaves its edits in the checkout for the Fix
    /// session, which has its notes. The commit it made is kept on the attempt.
    fn finish(&self, env: &ProtocolEnv<'_>) -> Vec<RunNotice> {
        let run = || -> Result<Vec<RunNotice>> {
            let Some(mut fix) = current(env)? else { return Ok(Vec::new()) };
            if fix.outcome == OUTCOME_ESCALATED {
                return Ok(Vec::new());
            }
            let cwd = self.cwd(env)?;
            let notices = commit_run(env, &cwd, "Quick fix");
            let head = cwd.git(&["rev-parse", "HEAD"]).context("read the commit the quick fix left")?;
            if fix.head != head {
                fix.head = head;
                save(env.fleet, &fix)?;
            }
            Ok(notices)
        };
        run().unwrap_or_else(|err| vec![RunNotice::Error(format!("{err:#}"))])
    }
}

/// Plays the quick fix for `--agent mock`: it changes nothing, so the check
/// still fails and the attempt escalates, as a real one that cannot help does.
pub fn mock_turn(access: &impl super::mock::Access, node_id: Uuid, _conversation_id: Uuid) -> Result<String> {
    let open = access.read(|conn| quick_fixes::open_for_node(conn, node_id))?;
    anyhow::ensure!(open.is_some(), "no quick fix is open for node {node_id}");
    Ok("Mock: a quick fix changes no code.".to_string())
}
