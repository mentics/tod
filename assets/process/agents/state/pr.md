# State: `pr`

**Forward gate:** `pr` → `approved`

## On entry

The PR is driven as the node's `pr` conversation, started from the conversation view's **Pr** step alongside Send.

1. Read lifecycle state, obligations, plan, and the review's findings and their answers.
2. Check `tod-cli pr status` before doing anything else — an earlier session may already have opened the PR.

## Responsibilities

Open the PR if none exists yet (`tod-cli pr open`), then watch it until it is mergeable:

- Watch CI checks, review comments, and requested changes.
- Push fixes for a failing check or a requested change; reply to comments through `tod-cli pr comment reply`.
- Record the current status through `tod-cli pr status` / `tod-cli pr mergeable` each time something changes.

Your job ends at **mergeable**, not **approved** — the forward gate is the app's own live check against GitHub, not something you decide.

If you cannot make further progress without the user (a merge conflict you cannot resolve, a requested change you cannot judge), record it blocked with why and stop.

## Forward gate rules (`pr` → `approved`)

The app checks this gate itself from GitHub's live status; no agent evaluates it:

- The PR is approved (per GitHub's review decision) and its checks are green.

## Exit

The gate advances the node once GitHub reports the PR mergeable — no agent turn decides this.

## Blockers

CI failure you cannot fix, a merge conflict you cannot resolve, or a requested change you cannot judge → record blocked and hand back.
