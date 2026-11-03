# State: `pr_draft`

**Gate:** `pr_draft` → `pr_review`. The app checks it against GitHub; no agent evaluates it: the pull request is open and no longer a draft, and a person is on it as reviewer. The gate is hardcoded for now (what "ready for human review" means will want to be scriptable per project).

## First

The PR is worked as the task's `pr` conversation (the protocol is named for the pull request; this state is its draft). Read lifecycle state, obligations, plan, and the review's findings and their answers. Check the pull request's status first (`tod-cli pr status`): the `pr` state opened it as a draft and certified it. You run on every stay in `pr_draft`, whether or not GitHub shows work, so that the skills configured for this phase are applied each time.

## Responsibilities

The pull request already exists: do not open another. Get the draft **ready for a person to review**, by clearing the automated feedback on it first:

- Apply this phase's configured skills to the PR, as its context lists them.
- Clear everything you can: bring the branch up to date with its base, fix failing checks, answer review threads from automated reviewers and bots, and meet any bot's required score. The app waits for reviews and for CI; you are sent back when there is more to do.
- Keep it a draft. A person marks it ready for review and puts a reviewer on it; that is what the gate waits for. Do not mark it ready, and do not request reviewers yourself.

Decide the scope of each piece of feedback as `pr_review` does: fix what this change introduced, and decline with a reason what was already there or would enlarge the change.

A human reviewer's feedback comes after that, in `pr_review`.

### Before you say nothing is left

Do not rely on what the runner sent you or on GitHub's merge state alone: the merge state reads `blocked` while a review is missing, and that hides a branch that is behind its base. Check each of these yourself, and fix what fails:

- **The branch is current.** Compare it with its base (`git fetch`, then see whether the base has commits the branch lacks); if it does, merge the base in.
- **Every review bot the project names** (there may be several, or none) **has reviewed the current head, and its latest score for the head meets its threshold.** A score, or a review, left on an earlier commit does not count: if a bot has not reviewed the current head, ask it to, as the project's own skill says how, and fix what it finds that this change introduced.
- **Every open review thread, from a person or a bot, is answered.**
- **This phase's configured skills have been applied**, as the context lists them.

Only when all of these hold, say nothing is left.

## Done when

Nothing is left that you can clear: the branch is current, checks pass, bot feedback is answered and any bot's score is met. The gate then checks from GitHub that it is open for review and has a reviewer; until a person does that, the run waits at the gate with those criteria failing.

## Blockers

A conflict you cannot resolve, a failing check you cannot fix, a question of scope you cannot settle → ask the user, or record blocked with why, and stop.
