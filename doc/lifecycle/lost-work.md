# Lost work

Status: built (`tod_core::lost_work`, the autopilot's `reopen_lost_work`).

## Problem

A node's implementation commits can be lost: a sandbox replaced before its
branch was pushed (`doc/agentd.md`, "Pushing the branch", pushes every
repository at each step boundary now, but work lost before that is still
lost), a checkout removed. The node is then in `verifying`, `review` or `pr`
with its plan steps `implemented` or `verified`, while its Files checkout holds
plain `main`. A reviewer finds nothing to review and asks the user; the
user's "send the task back to active" moved nothing, because an answer only
reaches the agent, so the reviewer asked again.

## The rule

This is the one place the app moves a node back by itself. Elsewhere a state
that no longer holds is the user's to move (`lifecycle_validity`); this rule
applies only to a condition the checkout **proves**, so nobody has anything to
decide. It holds when all of these are true:

1. The node is in `verifying`, `review` or `pr` (`lost_work::STATES`).
2. At least one implementation or verification plan step (phase `active` or
   `verifying`, the ones reopening opens again) is `implemented` or
   `verified`.
3. The node's Files directory is a git checkout, and in it and in every
   initialized submodule (recursively):
   - nothing is uncommitted (no changes, no untracked files), and
   - HEAD has no commit beyond the base: for the checkout, the repository's
     default branch from `origin` (`origin/HEAD`, else `origin/main` or
     `origin/master`); for a submodule, the commit its parent pinned there at
     the parent's merge base with that default branch.

Commits on the node's branch (local, or `origin/<branch>`) count wherever the
checkout sits, and a node branch on `origin` at a commit other than the base
(merged into the default branch, or behind it) is not lost. A repository with
no `origin` proves nothing, so the rule stays silent. Every other git failure
(a submodule listing, a missing default-branch ref, an unreadable count) is an
error naming the command, and so is a stale or missing Files location: the
rule never turns a failure into "not lost" or "lost". A node with no Files, or whose location
is not made, is left to the launch, which says so itself.

## What the autopilot does

At the top of each pass, before the pending-decision check, in those states
(`Autopilot::reopen_lost_work`): the checkout is read once per state of the
run, and again whenever a `broken` question from a verification, review or
pull-request agent is pending (it may be about exactly this). When proven:

- the lifecycle goes back to `active` (first, so a restart in the middle finds
  the node again from its steps);
- every `implemented`/`verified` step of `active` or `verifying` goes back to
  `ready`/`pending` with a note saying why (`PlanStepRepo::reopen_lost_work`),
  and every `verified` verdict is reopened;
- the pending `broken` questions from those agents that are about the missing
  work are withdrawn (the answer they wanted is the one just given). The rule
  (`lost_work::is_about_lost_work`): the question's text, case-insensitive,
  mentions `checkout`, `branch`, `commit`, `pushed` or `lost`. Any other
  `broken` question (a tool that failed, a credential refused) stays pending
  and the node is reopened regardless; the note names both the withdrawn and the
  left-pending questions;
- a note is left on the node's activity ("Lost work: implementation reopened",
  `node_timeline` kind `note`, shown in the Activity panel), and the run
  records a `lost_work` step.

No question reaches the user. The run then implements again.

**Once per run.** If the rebuilt work is lost again (or the steps make no
commits at all, so the check keeps holding), the second finding stops the run
with `NeedsHuman::LostWork`, stopped on the user with the reason: the first
reopening did not stick and another would repeat it. The counter is cleared as
the run stops, so when the user runs it again the rule may reopen once more.

## What agents do

A reviewer or verifier that finds the change missing asks one `broken` decision
saying what it found and stops. It must not offer "send back to active" as an
option (`surface/review.md`, `surface/verify.md`). If the checkout proves the
loss the question is withdrawn and the node reopened; if it does not (the
branch has commits, so the agent may be mistaken or the cause is something
else) the question stays for the user as any other.

## Limits

- A node branch, local or on `origin`, at exactly the default branch's current
  tip (a merge with nothing after it, or an empty branch) is not told apart from
  lost work. A branch at any other commit with nothing beyond the default branch
  (merged and the default moved on, then deleted on origin) is not lost, as long
  as a ref of it remains here or on origin.
- The checkout must be the top of its own work tree: a directory inside another
  repository, a bare repository, a `.git` directory, or a `.git` git cannot read
  is an error, decided from git's exit status and paths, never from its wording.
  Only a directory with no repository and no `.git` is left to the launch.
- With no node branch known, only the checked-out HEAD is compared; no branch is
  guessed from what is checked out, so work on another branch of a checkout then
  reads as lost.
- Work in a repository outside the node's Files is not seen. The reopened
  implementation finds it quickly.
- Done steps whose work makes no commit (documentation in another system,
  say) in a node whose Files is a repository would read as lost; the once-per-run
  stop is what keeps that from looping.
- The withdrawal rule is a keyword match; a question about the missing work that
  uses none of the words stays pending (and is named in the note).
