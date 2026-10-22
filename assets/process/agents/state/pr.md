# State: `pr`

**Gate:** `pr` → `pr_review`. The app checks it against GitHub; no agent evaluates it: the pull request is open and no longer a draft, and a person is on it as reviewer. The gate is hardcoded for now (what "ready for human review" means will want to be scriptable per project).

## First

The PR is opened as the node's `pr` conversation. Read lifecycle state, obligations, plan, and the review's findings and their answers. Check the pull request's status first (`tod-cli pr status`): an earlier session may already have opened it.

## Responsibilities

Open the pull request **as a draft** if none exists yet, and link it to the node (`tod-cli pr open` does both). That is the whole of this state's agent work:

- Push the branch first, so the PR has something to open from.
- Write the title and description from the node's obligations and what was built, so a reviewer who has not followed the work can follow the change.
- Open it as a draft, always. A person marks it ready for review and puts a reviewer on it; that is what the gate waits for. Do not mark it ready, and do not request reviewers yourself.

Feedback, failing checks, a branch behind its base, and bot reviews are `pr_review`'s work, not this state's. Leave them.

## Done when

The pull request is opened and linked to the node. The gate then checks from GitHub that it is open for review and has a reviewer; until a person does that, the run waits at the gate with those criteria failing.

## Blockers

A branch you cannot push, or nothing to open a pull request from → ask the user, or record blocked with why, and stop.
