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
  then the last lines of output). A stored memory kill is never reused for unchanged files:
  the next run runs the check again.
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
