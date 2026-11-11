---
name: live-verify
description: Walk a real Linear ticket through the whole lifecycle in a cloud sandbox, in the persistent live data root, find bugs (the one reported and any others), fix them, and repeat. Use when asked to reproduce a reported bug live, or to run the live process end to end.
user_invocable: true
---

Run tod the way an engineer on the team does, against real services, find what
breaks, and fix it. The argument (if any) is a bug the user saw or a part of
the process to exercise; reproduce that first, then keep walking and fix
everything else you meet. The user wants bugs found by you, not by them: work
autonomously, do not stop to ask unless something is theirs to decide.

Read `doc/live-verification.md` first (setup, the tree, gotchas). This file is
the procedure.

## The root (kept long term)

`~/.local/share/tod-live-verify` is the one data root for this. Never delete
it, never create a second one for this work, never touch the user's
`install.toml`, never launch `tod` without `--data-root`. It was seeded from
the user's real root by `scripts/seed-live-verify-root.sh` (they run it: it
copies credentials; agents do not read the real root's credentials).
`credentials_file_only: true` is set: never use the OS keyring. If the root is
missing or its credentials are stale, ask the user to run the seed script.

Its tree: `Linear tickets` (generator) -> Quick accept -> `ENG work` (Files =
location `Golden workspace`, a fork of the `golden` sandbox, `/workspace`, i.e. `Cofactr/workspace` like the real roots;
lifecycle-config: review `code-review`, pr `open-pr`+`fix-ci`, pr_review `fix-ci`).
A ticket accepted there is the node under test. The root has a second
location, `Build test` (an image location on the private `joel-cofactr/build-test`):
use `Golden workspace` for real-data checks and the real golden flow, and
`Build test` for anything that pushes a branch or opens a PR or needs
flexibility. Never push from a `Golden workspace` node, and never use
`/workspace/flagship` as a location (its origin is Cofactr/flagship). A node left from an earlier
run (ENG-2925, `nexar-ingest-mints-products-with-the-w-2`) may still be there:
reuse it, or accept another unstarted ticket the user names.

## Build and (re)deploy: from a checkout that is current with main

```sh
git fetch && git merge --ff-only origin/main
cargo build -p tod -p tod-cli -p tod-sandbox-cli
scripts/build-sandbox-binaries.sh                  # Linux supervisor, relay, orchestrator, tod-cli (Docker)
./target/debug/tod-sandbox --data-root ~/.local/share/tod-live-verify orchestrator \
  --bin target/sandbox/tod-orchestrator --tod-cli target/sandbox/tod-cli
```

The orchestrator is shared by every user of the workspace and by the user's
real root: redeploying it is expected here, but say so in your report. Never
leave a copy of a sandbox binary in `target/debug/sandbox`.

## Launch (and relaunch after every rebuild)

Stop only instances you started (control socket `quit`, then kill by data root
or port), including this root's `tod-agentd` (it outlives the app and runs the
database broker; an old build's daemon answers wrongly):

```sh
python3 -c "import socket;s=socket.create_connection(('127.0.0.1',47611));s.sendall(b'quit\n')"
pkill -f "tod-agentd.*tod-live-verify"
./target/debug/tod --data-root ~/.local/share/tod-live-verify --no-focus \
  --width 1800 --height 1000 --agent-socket-port 47611 &
```

Launching the app reinstalls any idle node sandbox whose install is older than
the build (`cloud_sync::refresh`), so a running node picks up fixes at relaunch.
An unrelated toast (ACP adapter out of date) may cover the task panel; dismiss it.

## Drive it, and look at real images

Control socket on 127.0.0.1:47611 (`key`, `text`, `click x y`, `sync`, `shot2
<png>`, `scroll`, `quit`; README.md). Use `shot2`, never `shot`. Read the PNGs
yourself (the Read tool): text dumps are not enough. Wait ~1.5 s between clicks
(dialogs animate). Positions move; take a screenshot before clicking.

Useful in the task panel (Task pane, right side): the status line and "waiting
on / working", the runner split button (Start / Resume / Pause as the default; its menu: Resume
with a fresh budget, Run with a new session, Stop now, Check now, Migrate to…,
Reset environment, Update workspace to latest main, Delete sandbox, Sync now), Watch, and the Log button (bottom right) for errors.
`tod-cli --data-root <root> ...` reads and edits the root's data (a node id is a
UUID: `node show <slug>`). Look in the sandbox with `tod-sandbox --data-root
<root> exec <sandbox> -- sh -c '...'` (processes, files, logs).

GitHub: test pull requests and branches go only in the private repo
`joel-cofactr/build-test` (see doc/live-verification.md); never create, push,
mark ready, comment on or touch a PR or branch in any Cofactr org repo. Close
the PRs and delete the branches you make. You may read anything (`gh pr view ...`). Do not post to, push to, or
edit the user's real pull requests yourself; the node's agent does that, as the
product should. If a test needs a new comment or review on a PR, ask first.

## The loop

1. Reproduce the reported bug, if any, with the real flow. Take screenshots.
2. Walk the node forward one lifecycle step at a time (proposed, design,
   planning, ready, active, verifying, review, pr, pr_review, ...). At each step
   check: the status line says the truth; the sandbox really is doing it
   (`exec ... ps`); the PR, ticket and plan state match; nothing is stuck
   waiting on something that is not coming. Anything confusing is a bug too.
3. On any failure or confusion: write a short repro, then fix it. Small fixes:
   do them yourself. Larger or independent ones: a sub-agent in its own worktree
   with a regression test, then merge. Always add a test.
4. Merge to `main` (the user's rule: no pull requests for tod:
   `git push origin HEAD:main` after tests pass), rebuild, redeploy, relaunch, and
   rerun from the failing step. Do not trust a fix you did not see work live.
5. Keep going until the process works cleanly, or you hit something that is the
   user's call.

Run tests with explicit timeouts and scoped (`cargo nextest run -p <crate> ...`);
`cargo check --workspace --all-targets` before pushing. Known failure not yours:
`tod-supervisor::wake a_wake_runs_the_node_and_its_work_reaches_the_orchestrator`
(stale since main `a6aa1544`); an orchestrator broker test can time out under load.

## Things learned (keep adding)

- A cloud node waits on GitHub through an event wait whose deadline is the next
  scheduled look (5 minutes by default, `pr_readiness.review_schedule`). The
  orchestrator's gate (`tod-orchestrator::github_gate`) answers each due wake
  itself with `tod_core::autopilot::check_github`; only feedback or a cleared
  wait pokes the sandbox. No app needs to be running. "Check now" does the same
  from the app and wakes the node.
- Feedback the pr agent acts on: changes requested, open threads, failing
  checks, behind/conflict, a bot score under its minimum, and a person's
  COMMENTED review or top-level PR comment newer than the head commit and the
  agent's last turn (`pr_readiness::Work::ReviewComments`). The agent posts as
  the same user, so its own comments are told apart by time.
- A supervisor woken while its wait is not due goes back to sleep (and now says
  so with an idle beat). To run it anyway: Check now (clears a satisfied wait),
  or Run with a new session.
- Wake times and waits are in the root's `waits` table (`sqlite3 tod.db "select
  match_spec,state,datetime(due_at/1000,'unixepoch') from waits"`) and the
  orchestrator's `/data/wakes.json` (`tod-sandbox exec tod-orchestrator -- cat ...`).
- No AWS CLI or credentials in sandboxes: database access goes through the
  user's machine (`tod-cli db`, the broker).

When you finish, report: what you ran, each bug found (fixed, or why not), what
you merged and deployed (including the orchestrator), and what is left. Add any
new gotcha to this file and `doc/live-verification.md` and merge them.
- The orchestrator is behind Blaxel's edge: every request needs the Blaxel
  bearer token (a bare `curl` to its URL, `/health` too, answers 401). Its log
  is in its sandbox under `/var/log/sandbox-api/` (see doc/live-verification.md).
- Blaxel masks env values (`TOD_USER` reads `****`): treat as unknown.
- A node with a live heartbeat is skipped by the launch reinstall: use
  Reset environment (runner menu) after a deploy, and check `user_version` of the
  supervisor's `tod.db` in the sandbox to be sure it is the new build.
- No `timeout` on macOS; do not wrap cargo in it, and `sed -i ''`.
- Typing into a request's free-text field: click it, send `key enter` (edit
  mode), then `text ...`, then `key ctrl-enter` to submit. Without the Enter
  the text goes nowhere.
- The task panel's upper part scrolls: with a long failure text the runner
  buttons sit below its fold (`scroll 1400 400 0 300` brings them up).
- Sandboxes left asleep over an hour are deleted by Cofactr's `Blaxel Prune`
  workflow unless labelled `keep-when-idle=true` (node sandboxes are).
- A submodule whose origin fetches only its default branch needs the node's
  branch in its fetch refspec and an upstream in config; `git switch --track`
  refuses it.
- A node sandbox is a fork with its golden's memory (8 GB), its root
  filesystem in RAM, and the workspace's dev servers started at boot (about
  3 GB): memory-hungry checks (basedpyright) can be killed by the kernel.
- `pkill -f <pattern>` inside `tod-sandbox exec ... sh -c` also matches, and
  kills, that `sh`: kill by PID.
- In zsh, `"$B:refs"` is a history modifier; write `"${B}:refs"`.
