# State: `approved`

**Forward gate:** `approved` → `merged`

## On entry

No agent runs on entry — `approved` is a thin holding state, like `ready`/`done`. The `pr` state already drove the PR to mergeable; this state just waits for the user to click merge.

## Responsibilities

None. Nothing here is agent work: the PR is already open, checks are green, and it is approved. The user merges it when ready.

## Forward gate rules (`approved` → `merged`)

The app checks this gate itself from GitHub's live status; no agent evaluates it:

- The PR has been merged.

## Exit

The gate advances the node once GitHub reports the PR merged — no agent turn decides this.

## Blockers

None — if the PR needs more work, the node moves back to `pr`, not forward from here.
