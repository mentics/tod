# Learnings from supervising two nodes (2026-11-19)

Two nodes under "Devcontainer ENG tasks" (ENG-3557 add index on
`v2_org_part.cofactr_id`, ENG-3559 remove ungated `public_part` joins) were
supervised from `proposed` on the live data root. Both reached a user decision
at `proposed`/`design` and stopped there; neither needed a runner action.

## Cost (new)

Cost is derived at read time from `conversation_usage` tokens and list prices
(`tod_agent::pricing`); nothing dollar-valued is stored, so a price change
re-prices history. Shown in the Activity panel on each step and each stay, in
the task panel as the node's total, and by `tod-cli stats <node> --section cost`.

First numbers (list prices):

| Step | ENG-3557 | ENG-3559 |
|---|---|---|
| phase agent, `proposed` | $1.48 | $2.34 |
| phase agent, `design` | $1.11 | $3.12 |
| evaluator, `proposed` | $0.35 | $0.96 (2 sessions) |

- **An evaluator is a fresh session**, so it writes the prompt cache again
  (55k to 121k cache-write tokens) and costs 20 to 30 percent of the phase it
  judges. It is the one step whose cost is mostly setup, not work.
- Phase sessions are 93 to 98 percent cache reads: their cost follows the
  number of tool calls (each re-reads the whole context), not output length.
  ENG-3559's design made 77 tool calls and cost 2.8 times ENG-3557's 23.
- A fork or a cheaper model for the evaluator is the lever if cost matters;
  nothing to change yet without more samples.

## Send-back worked as designed

ENG-3557's design phase found requirement ba0c3b4e rested on a false premise
(the callers do filter `deleted_at`), sent the node back to `proposed` with the
evidence, and raised one decision (partial index or not) instead of building on
it. Check after any send-back that it converges: it did (one send-back).

## Found and fixed

- **PR-state read storm.** `refresh_pull_states` left a failed read "stale", so
  every redraw asked GitHub again and logged again: one pull request produced
  1,477 identical warnings and spent the hourly quota. A failed read now backs
  off (to the rate-limit reset when that is the reason, else 60 s), a rate limit
  ends the round, and it is logged once.
- **"GitHub gives no patch" stopped `pr_draft`.** Comparing the reviewed head
  with the current one failed for a binary or very large file. Such a file is
  now compared by blob; a differing or unreadable blob means "not covered",
  never an error.
- **Toasts.** Every ERROR in the log raises a toast, and every failed `tod-cli`
  call was logged as an ERROR, so an agent's own typos (a missing flag, an
  unknown node) toasted the user. A rejected command (usage, not found,
  refused) is now a warning; a failure of tod itself (store, daemon, io, network)
  stays an error. macOS's "could not create display link" (-6661), which is not
  a tod failure, is a warning too.
- **Cost on the wire**: `cargo test -p tod-agent` alone fails (a Uuid serde
  feature that only unifies when `tod-store` is built too); use
  `-p tod-agent -p tod-store`.

## Seen while supervising five nodes (2026-11-20)

- **Usage limit stops every running node at once.** A 5-hour plan limit
  ("You've hit your 5-hour limit") fails each live run as `agent_failed`; nothing
  is wrong with the nodes. `runner resume` each one when the window resets.
- **Review/design oscillation (ENG-3557).** Three send-backs over one claim (a
  partial index) ran past the loop guard because each carried different reasons.
  It ended in a user decision; watch for the same claim being sent back twice.
- **Diverged origin branch (ENG-3038).** An earlier run of the same task had
  pushed 29 commits to `origin/<branch>`; this run's local branch did not contain
  them, so the push was refused (tod says so, with the rebase/merge hint:
  `provision.rs` divergence error). `switch_to_branch` itself starts a missing
  local branch from `origin/<branch>`, so the local branch must already have
  existed at another commit; the cause was not traced. Non-destructive fix: `git
  merge -s ours origin/<branch>` in the node's worktree, then `runner resume`.
- **Two leases, one branch (ENG-3094).** A node leased a second worktree 10
  minutes after its first, still holding the branch, so `switch` failed ("already
  used by worktree"). Fix: `git switch --detach` in the idle one (check the branch
  equals origin first), switch the other onto the branch, resume. Gap: reuse the
  first lease.
- **Stale `needs_human` outcome (ENG-3559).** The outcome kept saying a decision
  was pending after every decision was answered, so the runner did not resume.
- **No `timeout` on macOS, and `$T` does not word-split in zsh.** Call the
  deployed `tod-cli` (`target/dev-fast/tod-cli`) by full path.

## Process pitfalls

- `ledger.py next` leases any due node in scope, and the scope already holds
  the whole parent subtree, so a shepherd for two nodes must not call it; hold
  the two by slug (`survey.py <root> <slug> <slug>`) and release any lease taken.
- Environment variables do not persist between Bash calls: export
  `TOD_OPERATOR`/`TOD_ACTOR` in each.
- A node waiting on `decisions` is the user's. Do not answer a design question
  to move a node along.
- Deploy from the main checkout, not the worktree; it also redeploys the shared
  orchestrator when `tod-agent` sources change (pricing did).
