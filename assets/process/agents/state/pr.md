# State: `pr`

**Gate:** `pr` → `pr_review`. The app checks it against GitHub; no agent evaluates it: the pull request is open and no longer a draft, and a person is on it as reviewer. The gate is hardcoded for now (what "ready for human review" means will want to be scriptable per project).

## First

The PR is worked as the node's `pr` conversation. Read lifecycle state, obligations, plan, and the review's findings and their answers. Check the pull request's status first (`tod-cli pr status`): an earlier session, or an earlier stay in this state, may already have opened it. You run on every stay in `pr`, whether or not the PR exists and whether or not GitHub shows work, so that the skills configured for this phase are applied each time.

## Responsibilities

Get the pull request **ready for a person to review**, as a draft:

- If none exists yet, open it and link it to the node (`tod-cli pr open` does both). Push the branch first. Write the title and description from the node's obligations and what was built, so a reviewer who has not followed the work can follow the change; when the PR exists, bring them up to date with what is built now.
- Apply this phase's configured skills to the PR, as its context lists them.
- Clear everything you can: bring the branch up to date with its base, fix failing checks, answer review threads from automated reviewers and bots, and meet any bot's required score. The app asks a bot to review again and waits for it and for CI; you are sent back when there is more to do.
- Keep it a draft. A person marks it ready for review and puts a reviewer on it; that is what the gate waits for. Do not mark it ready, and do not request reviewers yourself.

A human reviewer's feedback comes after that, in `pr_review`.

## Done when

The pull request is open and linked, and nothing is left that you can clear: the branch is current, checks pass, bot feedback is answered and any bot's score is met. The gate then checks from GitHub that it is open for review and has a reviewer; until a person does that, the run waits at the gate with those criteria failing.

## Blockers

A branch you cannot push, or nothing to open a pull request from → ask the user, or record blocked with why, and stop.
