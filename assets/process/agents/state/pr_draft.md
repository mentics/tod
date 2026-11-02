# State: `pr_draft`

**Gate:** `pr_draft` → `pr_review`. The app checks it against GitHub; no agent evaluates it: the pull request is open and no longer a draft, and a person is on it as reviewer. The gate is hardcoded for now (what "ready for human review" means will want to be scriptable per project).

## First

The PR is worked as the node's `pr` conversation (the protocol is named for the pull request; this state is its draft). Read lifecycle state, obligations, plan, and the review's findings and their answers. Check the pull request's status first (`tod-cli pr status`): the `pr` state opened it as a draft and certified it. You run on every stay in `pr_draft`, whether or not GitHub shows work, so that the skills configured for this phase are applied each time.

## Responsibilities

The pull request already exists: do not open another. Get the draft **ready for a person to review**, by clearing the automated feedback on it first:

- Apply this phase's configured skills to the PR, as its context lists them.
- Clear everything you can: bring the branch up to date with its base, fix failing checks, answer review threads from automated reviewers and bots, and meet any bot's required score. The app waits for reviews and for CI; you are sent back when there is more to do.
- Keep it a draft. A person marks it ready for review and puts a reviewer on it; that is what the gate waits for. Do not mark it ready, and do not request reviewers yourself.

Decide the scope of each piece of feedback as `pr_review` does: fix what this change introduced, and decline with a reason what was already there or would enlarge the change.

A human reviewer's feedback comes after that, in `pr_review`.

## Done when

Nothing is left that you can clear: the branch is current, checks pass, bot feedback is answered and any bot's score is met. The gate then checks from GitHub that it is open for review and has a reviewer; until a person does that, the run waits at the gate with those criteria failing.

## Blockers

A conflict you cannot resolve, a failing check you cannot fix, a question of scope you cannot settle → ask the user, or record blocked with why, and stop.
