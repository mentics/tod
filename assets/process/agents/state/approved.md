# State: `approved`

**Gate:** `approved` → `merged`. The app checks it against GitHub: the PR is merged.

## Agent

No agent runs on entry — `approved` is a thin holding state, like `ready`/`done`. The `pr` state already drove the PR to mergeable; this state just waits for the user to click merge.

## Responsibilities

None. Nothing here is agent work: the PR is already open, checks are green, and it is approved. The user merges it when ready.

## Done when

The app checks this gate itself from GitHub's live status; no agent evaluates it:

- The PR has been merged.

## Blockers

None — if the PR needs more work, the node moves back to `pr`, not forward from here.
