# Checks: commands a phase's gate runs

A project's own mechanical checks (type checking, lint, tests) belong in the
phase they guard, run by the app, so an agent is only spent on what they find.

## Configuration

The Lifecycle config capability holds, per phase, an ordered list of checks:
an optional **label** and a **command** (`tod_store::lifecycle_config::Check`).
They are edited in the task editor next to the phase's skills ("Add check"),
resolve like skills (the nearest node that lists the phase wins; an explicit
empty list switches an ancestor's off), and exist for every phase key,
including `implement` (the `active` state) and `verify` (`verifying`).
`tod-cli` cannot set them: a command is run by the app, so only the user
configures one.

## Setup: preparing a fresh worktree

A node's Files directory is often a brand-new worktree with nothing installed,
so a check that needs dependencies would time out on a cold install or fail
for lack of them. Each check can declare a **Setup** command
(`Check::setup`, "setup" in the editor and the YAML; blank means none): the
command that gets `dir` ready, such as installing what the check needs. tod
names no package manager; whatever the project declares is run.

- Run by the shell in the check's `dir`, before the check, with its own
  30-minute limit (`checks::SETUP_TIMEOUT`).
- **Once per worktree**: a pass leaves a marker file in the worktree's
  own git directory (`tod-setup-<hash of the command>`), so later rounds and
  other checks with the same setup skip it. The marker goes with the worktree:
  one recreated at the same path (Reset environment, a removed location, a
  replaced sandbox) has none and runs the setup again. The run is also shown
  in `check_runs` (command `setup: <command>`), for display only.
- A failed or timed-out setup makes the check *broken* (the run stops for the
  user, `NeedsHuman::CheckBroken`) with the setup's output; the check is not
  run without it, and the next look runs the setup again.
- Setup runs show in the node's Activity beside the checks.
- With no Setup, a check that fails with output that points at missing
  dependencies ("Cannot find module", "command not found", "No module named",
  "node_modules", "Run ... install", and similar), or that times out, begins
  its reason with that: probably missing dependencies, and to set a Setup
  command on the check (`checks::missing_dependencies_hint`).

## Fix: a command that repairs a failing check

A check may carry a **fix** (`Check::fix`, "fix" in the editor and the YAML;
blank means none): a command that repairs what the check finds mechanically (a
formatter, a lint autofix). When the check fails, tod runs the fix in the same
directory (`tod_core::fixer::run_fix`, from `checks::run_stale_notifying`),
commits what it changed as **its own mechanical commit** (subject
`tod: fix <check>`, no hooks, a submodule pointer never included), and runs
**only that check again** on the new tree.

- If it passes there is no Fix protocol turn, no finding, and no new round of
  review: a change a tool made is not reviewed. In `review` or earlier, a
  round of review that was done before the commit is recorded as covering the
  new head too (`pr_draft_review::record_review_phase_heads`), so the pull
  request's first head is not read as unreviewed; the node's lifecycle state
  is untouched, so `lifecycle_next` does not send it round Review again.
- A fix that fails, changes nothing, or leaves the check failing hands the
  failure on as it was, with what the fixer did added to the output and shown
  in the Activity's "fix for ..." entry: the agent (Fix protocol) takes it from
  there. Nothing is hidden.
- If the tree already held uncommitted work that is not the fixer's, the fix's
  changes are left uncommitted (committing them would put someone else's change
  in a commit marked mechanical).
- The fix does not enter the check's fingerprint: changing it does not make
  the check stale.
- CI checks have the same in the project's `pr_readiness.check_fixes`
  (`doc/lifecycle/pr-readiness.md`, "Mechanical failures").

## What runs, and when

- **Where**: the node's Files directory (this machine, a dev container, or its
  cloud sandbox), through the shell, stderr folded into stdout.
- **Result**: exit 0 passes; any other exit fails, with the tail of the output
  (16 KB) as the finding. A command that cannot run at all (directory missing,
  ten-minute timeout (host, sandbox and dev container alike; in a container the
  command runs under the container's `timeout` and `docker exec` is killed
  shortly after as a backstop), the sandbox failing) is *broken*: the run stops for the
  user (`NeedsHuman::CheckBroken`), nothing is sent to an agent.
- **Killed for memory** is broken too, never a finding: exit 137 (SIGKILL; the host's own
  signal is mapped to 128 + signal, and the relay and `docker exec` already report it that
  way), V8's heap abort or a `bad_alloc` (exit 134, with its message), Python's uncaught
  `MemoryError` as the traceback's last line (exit 1), or the shell's "Killed" as the last
  line. A failing test that merely mentions memory stays a failure. The output begins
  "Killed for memory (...)" with where it ran and, where `/proc/meminfo` exists, total and
  free MB when the check started (the script prints it ahead of the command; `run_command`
  strips that line wherever it is). The Fix agent is not sent it: the stop asks the user to
  lower the check's heap, scope it, or remove it (its first line is that decision, then why,
  then the last lines of output). A stored broken result of any kind (timeout, memory kill, could not start) is never reused
  for unchanged files: the next look, a Resume or Check now included, runs the check again
  (`checks::reusable`; gap 60, where a recorded timeout stopped the run again within seconds).
  The stop says it is a fresh run and when it ended; the gate criterion says when it shows a
  recorded result.
  **In a sandbox, the first kill is retried once** (`checks::run_command`,
  `free_sandbox_memory`): tod stops the dev servers the workspace boot started (found by
  program and arguments, not text anywhere in the line: `python manage.py runserver`,
  `python -m uvicorn`/`gunicorn`, node running vite but not vitest; a grep, tail or editor
  never matches; stopped by PID, TERM then KILL, never `pkill`; a process is reported stopped
  only once it is gone, and a kill that fails is an error), drops the page cache, and runs the check again. The servers are
  **left stopped** (not restarted: a restart would bring the memory pressure back for the
  next check; the sandbox is for checks and agents, not for browsing). If the second run
  also dies for memory, the check is broken as above and its output adds "killed for memory
  twice" with what was stopped and the free MB before and after. If freeing memory fails,
  the check is not run again: the broken request carries the error. A check outside a sandbox is never retried and
  nothing on the user's machine is stopped.
- **Freshness**: each run records a fingerprint of the files
  (`checks::fingerprint`: every repository's and submodule's `HEAD`, diff
  against `HEAD`, and untracked files' contents). A run is current while the
  fingerprint is unchanged, so uncommitted edits, a commit, or a push each make
  the next look run them again, and nothing changed means nothing re-runs.
- **Gate**: each check is a criterion of the gate out of its phase
  (`Check: <label>`), answered from its latest run; `settle_gate` (the
  autopilot's gate step and the runner menu's Check now) runs the stale ones
  first. They are not catalog rows, so they are not saved as evaluations.

## The loop

`Autopilot::checks_round` runs at the top of every loop iteration and, in
`pr_draft`, before tod's own review (the PR agent may just have pushed). All
passing proceeds. Failing checks in a state with its own fixer (`active`, `verifying`,
`review`, `pr`, `pr_draft`, `pr_review`) are recorded as open review findings
(`Check \`<name>\` fails`, detail: the output) and the **Fix protocol** runs.
In a phase-agent state (`proposed`, `design`, `planning`, `merged`, `released`,
`learn`) there is no fixer: the failing check criterion fails the gate, which
runs the phase agent again, and the gate status it is given carries the
check's label and output. Both routes count rounds and stop on unchanged files.
After a change the files differ, so the checks run again.

Limits: rounds count against `lifecycle.review_round_limit` and reset once all
checks pass (`NeedsHuman::ChecksNotConverging`); failing again on the files
the fixer was already sent means it changed nothing
(`NeedsHuman::ChecksUnfixed`). A gate that fails only on checks returns to
the loop instead of stopping.

## Scope: directory, changed path, file pattern

A check can be scoped so one list on the workspace node serves every
repository below it. Each row has, besides label and command:

- **Run in** (`dir`): a directory under the node's Files directory (a
  submodule, say); blank is the directory itself. `..` and absolute paths are
  refused at save.
- **Only if changed** (`changed`): the path whose changes make the check
  apply; blank means `dir`, and `.` the whole tree.
- **Files** (`pattern`): space-separated globs (`*.py`; `*` also crosses `/`)
  a changed file must match, taken against its path under `dir`.

With none of the three the check always applies. Otherwise it applies when
something in its scope differs from where this branch left `origin`'s default
branch (`origin/HEAD`, else `origin/main`/`master`; none known is an error,
not a guess): commits, uncommitted changes, untracked files, and deletions,
in the repository that holds the scope and every submodule inside it. A check
that does not apply is recorded as `skipped`: it does not run and is not in
the gate. The files that matched (those that exist, under `dir`, relative to
it) reach the command as `$TOD_CHANGED_FILES`, one per line.

A scoped check's fingerprint covers only the repositories in its scope plus
its file list, so a change in one repository does not run another's checks.
Runs are keyed by phase, directory and command, so the same command may be
configured for several directories. Messages name a check `label in dir`.

## Where the runs are seen

The task panel's **Watch** opens the node's **Activity** panel. Each round of
commands that actually ran (not a cached or skipped check) is a row there
(`node_timeline`, kind `checks`), in the group for the stay in its phase; the
row opens to one chunk per command, collapsed, named by its label, tinted red
when it failed or could not run, and holding the output when opened. Each gate
check is a row too, listing its criteria; a check that reads exactly as the
last one did is not recorded again, so a runner polling a waiting gate does not
fill the list. A node's stays come from the phase changes the timeline
recorded (the phase chip's are marked "(user)"); a node older than the
timeline shows everything under its current phase.

## Quick fix: rung 1, before Fix

After a configured fixer has failed or is absent, and before the Fix
protocol, a failing check of tod's own gets one **quick fix**
(`Autopilot::quick_fix_checks`, protocol `quick_fix`,
`tod_core::conversation::quick_fix`; spec `doc/requests/design.md` section 5).
It is a small session on Haiku at low effort (lifecycle config key `quick_fix`
overrides) that is given one failing thing and nothing else: the check's label,
command and output tail, the part of the branch's diff that touches the files
the output names, and the phase's checks as the repository's usual commands.
It gets no obligations, plan or design, may edit files and run that check's
command, and may not touch obligations, the plan or decisions
(`settled::check_conversation_agent`, `decisions ask`).

It has `lifecycle.quick_fix_turns` turns (default 3). After each, tod reruns
only that check (`checks::run_one`): green is success, tod commits the work and
goes on with no Fix turn and no new review round. A session that changed
nothing, or the last turn failing, is an escalation: the check becomes a
finding for the full Fix protocol, whose opening carries the quick fix's notes
(`checks::record_findings_with_notes`). Every attempt and outcome
(`succeeded` / `escalated`) is recorded per check in `node_quick_fixes`
(schema v110, `tod_store::quick_fixes`, with per-check failure rates). A check
tried once is not tried again until the checks are clear. A check whose
`skip_quick_fix` is set (`lifecycle_config::Check`) goes straight to Fix.
In `pr_draft` the commit gets a small review (`doc/lifecycle/pr-readiness.md`).
