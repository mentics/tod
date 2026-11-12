---
name: shepherd-pr-drafts
description: Drive the real tod app on the user's live data root until every node in pr_draft is clean and waiting only for the user to open it, fixing tod (app, prompts, context) at the root along the way and logging what went wrong in doc/improvement-log.md. Use when asked to get the pr_draft nodes ready, babysit the live nodes, or work the live queue.
user_invocable: true
---

Get every `pr_draft` node on the user's **live** root ready for a person: CI
green, every review thread and bot finding handled, tod's own review clean, no
conflict with main. Then the user picks a priority, clicks **Open** and chooses
a reviewer. While doing it, treat every stop, confusing status, or unclear
request as a tod bug: fix it at the root, merge it, deploy it, and log it.

This is not `live-verify`: that skill uses its own test root and test repo.
This one works on the user's real nodes and real Cofactr PRs. Read
`doc/live-verification.md` for the general how-to; the rules below are stricter.

## Hard rules

- Live root: `/Users/jshellman/.config/tod`. Always pass `--data-root`; never
  write `install.toml`.
- Read its database read-only: `sqlite3 'file:/Users/jshellman/.config/tod/tod.db?mode=ro'`.
  Change node state only through the app (control socket) or `tod-cli`.
- **Never move a node to `pr_review`, never mark a PR ready, never request a
  reviewer.** That is the user's step.
- Never push to, comment on, or edit a Cofactr PR yourself: the node's agent
  does that, as the product should. Reading (`gh pr view`, `gh api`) is fine.
- **Never label a node sandbox `keep-when-idle=true`.** Letting idle node
  sandboxes be pruned is deliberate; only goldens and the orchestrator get it.
  A pruned node gets a new sandbox when it is resumed.
- tod fixes go straight to main (no PRs for tod). A change in another repo
  (Cofactr/workspace) goes on a branch and waits for the user's OK to push and
  open a PR.
- Delegate exploration and fixes to Sonnet subagents at medium effort, each in
  its own worktree, with a regression test. Verify their commit on main
  yourself before reporting it.

## Setup and deploy (repeat after every merged fix)

From a checkout current with main:

```sh
git fetch && git merge --ff-only origin/main
cargo build -p tod -p tod-cli -p tod-sandbox-cli
scripts/build-sandbox-binaries.sh
./target/debug/tod-sandbox --data-root /Users/jshellman/.config/tod orchestrator \
  --bin target/sandbox/tod-orchestrator --tod-cli target/sandbox/tod-cli
```

The orchestrator is shared by every root and user: say in your report when you
redeployed it. Then relaunch. Stop only what you started: send `quit` on the
control socket, then kill the daemon by its data root.

```sh
python3 -c "import socket;s=socket.create_connection(('127.0.0.1',47621));s.sendall(b'quit\n')"
pkill -f "tod-agentd.*--data-root /Users/jshellman/.config/tod"
./target/debug/tod --data-root /Users/jshellman/.config/tod --no-focus \
  --width 1800 --height 1000 --agent-socket-port 47621 &
```

The app starts a new `tod-agentd` (copied to `<root>/daemon/tod-agentd-<hash>`).
Running nodes keep going: their supervisors live in their sandboxes. If the
scratchpad directory (`.local/agent/scratchpad`) is gone, recreate it.

**A newer golden refuses Start.** Goldens are ordered by schema, wire version
and built commit time, and a node refuses to fork from a golden built by a
newer build than its own. If another session's build (not on main) refreshed
the shared golden, rebuild from current main and refresh the golden
(`tod-sandbox golden refresh`). `tod-sandbox golden` lists them.

## Driving the app

Control socket `127.0.0.1:47621`: `click x y`, `shot2 <png>`, `text`, `key
enter`, `key ctrl-enter`, `scroll x y dx dy`, `sync`, `quit`. Wait ~1.5 s
between clicks. Read every screenshot yourself with Read; text dumps miss
things. Positions move, so take a screenshot before clicking.

- Task panel: status line, "waiting on / working", the runner split button.
  The upper part scrolls: `scroll 1400 400 0 300` brings the runner buttons
  into view. "Reading GitHub…" lines shift the layout when they clear.
- Runner chevron: about (1775, 280) for a node with one PR, (1775, 348) with
  two. "Resume with a fresh budget" is about 33 px below it.
- To answer a request's free-text field: click it, `key enter` (edit mode),
  `text …`, `key ctrl-enter`.
- Useful tables (read-only): `run_state` (phase, reason_kind, reason_text,
  updated_at), `node_files_locations` (sandbox name), `waits`
  (`match_spec, state, due_at`). App log: `<root>/log/tod.log`.
- Look in a sandbox with `tod-sandbox --data-root <root> exec <sandbox> -- sh -c '…'`
  (processes, memory with `free -m`, logs).

## The loop

1. List the `pr_draft` nodes and each one's run state: working, waiting on
   GitHub (CI, a bot), waiting on you, stopped (by `reason_kind`), failed.
2. For each node that is not progressing, find out why before acting. Check the
   PR on GitHub (`gh pr view <n> -R Cofactr/<repo> --json mergeable,mergeStateStatus,statusCheckRollup,reviewDecision`),
   the sandbox (alive? pruned?), and the node's latest agent session.
3. If the stop is tod's fault (wrong count, misleading status, missed
   feedback, setup failure), fix tod, deploy, then resume the node. If it is a
   real stop, resume it (with a fresh budget if it ran out of budget) or turn
   it into a clear request for the user.
4. A node is done when its status says it is waiting for you to mark it
   ready, with checks green, the bot's review current for the head, no open
   thread, and the PR mergeable. Leave it there.
5. Keep going until every node is done or waiting on something only the user
   can answer. Check back on running nodes periodically; don't poll tightly.

## Asking the user

Ask only what only the user can answer (a product decision, an obligation that
turns out wrong, credentials). For each node waiting on the user, publish an
Artifact. It opens with the decision as one question and its options, the
recommended one first, each with its consequence. Then come the context and a
diagram of the mechanism, the obligations involved with their short ids, and
how to answer in tod (select the node, press the option's number in the task
panel). Mention overlaps between nodes, such as two PRs editing the same file.
Never ask the user to do what you or tod can do.

## Logging

Every problem kind gets an entry in `doc/improvement-log.md` (Seen with
count, nodes and dates · Symptom · Cause · Fix with commit or "open" ·
Measure). Bump the count when it recurs. Commit log updates to main as you go.

## Learned so far (keep adding)

- **Round counting.** The PR babysitter's `max_rounds` counts only feedback
  rounds (`Work::is_feedback`). Merging main and asking a bot again are not
  rounds. The review loops count only consecutive unclean reviews. An old stop
  of `pr_stuck` or `review_not_converging` with nothing standing was a
  counting bug: resume it with a fresh budget.
- **Draft conflicts.** GitHub reports a draft's `mergeable_state` as `draft`
  regardless of conflicts. Read `mergeable` (fixed in 74316387). `mergeable`
  null is transient: wait, don't decide.
- **Pruned sandboxes.** Cofactr's `Blaxel Prune` workflow deletes node
  sandboxes asleep over an hour. The status says "its sandbox was deleted;
  Start makes a new one". That is expected: resume the node. The replacement
  must fetch each submodule's branch from origin, or it starts on stale work.
- **Sandbox memory.** 8 GB, no swap, root filesystem in RAM. Dev servers
  (vite, two Django runservers, KB uvicorn, postgres) take most of it. A UI
  `tsc --noEmit` check does not fit at any working heap: 2 GB runs out of
  heap and 3 GB is OOM-killed. Checks come only from the user's lifecycle
  config, never from tod itself. CI runs tsc anyway, so a check that cannot
  fit belongs to the user to delete. Vite's cold prebundle holds about 1.6 GB.
  A warm `node_modules/.vite` cache in the golden plus on-demand start through
  ui-gateway (Cofactr/workspace#204) avoids it.
- **No database from a sandbox.** A sandbox has no aws CLI or SSO. Agents
  reach the database only through `tod-cli db` (the user's machine brokers
  it); a `db-query` shim in each node sandbox routes to it. A request quoting
  `db-query: aws CLI not found` means the shim or `cli/db` is missing.
- **Agent-made worktrees.** The workspace's CLAUDE.md tells agents to use a
  git worktree. In a node sandbox that hides the work from tod, because the
  node's checkout already is its worktree. Symptom: a detached HEAD after
  Reset environment, with the branch in `/workspace/.worktrees/…`. The
  `workspace/codebase` fragment forbids it now.
- **Failed setup.** "sandbox health check 'manifest' failed" on a fork means
  a half-made sandbox. Start now deletes a failed sandbox of the same node and
  makes a fresh one.
- **Requests.** Follow "Wording a request" in `stance/autonomous-session.md`.
  An unclear request text is a tod bug to fix, not just to relay.
- **Bot findings.** A `pr_stuck` with real new bot findings each round is
  real work, not a bug: resume it.
- **Shell.** In zsh, `$B:refs` inside double quotes is a modifier, so write
  `${B}:refs`. macOS has no `timeout` command.
