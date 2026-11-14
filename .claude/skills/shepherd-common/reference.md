# Reference

## Tables (read-only)

- `run_state` (`phase`, `reason_kind`, `reason_text`, `updated_at`);
- `node_files_locations` (the sandbox name);
- `waits` (`match_spec`, `state`, `due_at`).

Decide a node's state from these (the survey reads them), never from a glyph,
a chip or a screenshot: a red triangle and an empty run line can hide a stop
from days ago.

## Survey

```bash
python3 -I .claude/skills/shepherd-common/scripts/survey.py <root> <item>…
```

One line per node: its state and how long, its run state, pending requests
and waits, and its sandbox.

## Logs

- app log: `<root>/log/tod.log`;
- a sandbox: `tod-sandbox --data-root <root> exec <sandbox> -- sh -c '…'`.
- Probe a sandbox once, in one command:
  `tod-sandbox --data-root <root> exec <sandbox> -- sh -c 'uptime; free -m; ps -eo pid,etime,rss,args --sort=-rss | head -15'`.

## Diagnosing a node that is not making progress

Look, before touching it, at:
- its latest agent session;
- the gate criteria that fail;
- its findings and plan;
- its PR on GitHub (`gh pr view … --json mergeable,mergeStateStatus,statusCheckRollup,reviewDecision`);
- the sandbox's processes, memory and logs.

Typical gaps: a stop that was a counting or state bug; a phase agent or
evaluator looping or giving up; a gate that cannot be met; a status that says
something untrue; an error with no next step; setup that fails; a request that
only asks the user because tod lacked a capability; after `pr_draft`: review
feedback the babysitter does not take up, an approved PR left behind its base,
a merge or release that tod did not notice (wait never satisfied, webhook
missed), a post-release check or retrospective that fails or loops.

## Facts

- **Node sandboxes are pruned on purpose.** A periodic workflow deletes node
  sandboxes asleep for more than an hour. The status then says "its sandbox
  was deleted; Start makes a new one". Resume the node.
- **Sandbox memory:** 8 GB, no swap, and the root filesystem lives in RAM.
  The dev servers take most of it.
- **Checks** (per-phase commands) come only from the user's Lifecycle config,
  never from tod itself. A check that cannot work in a sandbox is the user's
  to change: say why in the node's verdict.
- **Bot findings:** a PR whose bot posts real new findings each round is
  doing real work. It is not stuck.
- **After `pr_draft`** (`doc/lifecycle/pr-readiness.md`, `phase-agents.md`):
  `pr_review` is run by the babysitter until `pr-approved.*` passes and a
  person approves; `approved` waits on `github:pr:<n>:merged`; `merged` waits
  on `github:release:published` for a release naming the PR, then its phase
  agent certifies with evidence; `released` certifies a post-release smoke;
  `learn` records the retrospective (`learn-recorded`). Waits in `waits` are
  woken by webhook or the next scheduled look; a wait past its `due_at` with
  the event already on GitHub is a gap.
- **Production reads** are the node's agent's job (`tod-cli db` in its
  sandbox). The auto-mode classifier refuses them here; don't retry.

## Shell

- In zsh, `$B:refs` inside double quotes is a modifier. Write `${B}:refs`.
- zsh's `[ a == b ]` fails with `= not found`. Use `=` or `[[ ]]`.
- macOS has no `timeout` command.
