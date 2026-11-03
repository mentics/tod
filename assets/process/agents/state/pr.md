# State: `pr`

**Gate:** `pr` → `pr_draft`. The app checks it; no agent evaluates it: a pull request is linked to the task and open (as a draft), and you have certified the draft ready in this stay. The certificate does not carry over: when the task is moved back and comes to `pr` again, the gate fails until you have reviewed the pull request again and certified it.

## First

The PR is worked as the task's `pr` conversation. Read lifecycle state, obligations, plan, and the review's findings and their answers. Check the pull request's status first (`tod-cli pr status`): an earlier session, or an earlier stay in this state, may already have opened it. You run on every stay in `pr`, whether or not the PR exists, so that a PR left from before a move back is looked at again rather than passed through.

## Responsibilities

Get the **draft** pull request in place and describing the change as it is now:

- If none exists yet, open it and link it to the task (`tod-cli pr open` does both; it always opens a draft). Push the branch first. Write the title and description from the task's obligations and what was built, so a reviewer who has not followed the work can follow the change.
- If one exists, push what is not pushed yet, and bring its title and description up to date with what is built now.
- Apply this phase's configured skills to the PR, as its context lists them.
- Keep it a draft. Do not mark it ready for review, and do not request reviewers.
- Certify the phase (`tod-cli phase certify --note <why>`) once the draft is open, pushed, and describes the change. In this state no separate session evaluates it: you certify it yourself, whether or not independent evaluation is on.

Checks, bot reviews, and branch drift are not this state's work: `pr_draft`, next, clears them before a person looks.

## Done when

The pull request is open as a draft, linked, its branch pushed, and its title and description match what was built; and you have certified the phase in this stay.

## Blockers

A branch you cannot push, or nothing to open a pull request from → ask the user, or record blocked with why (`tod-cli pr blocked`), and stop.
