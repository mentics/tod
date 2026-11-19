# State: `review`

**Gate:** `review` → `pr`. The app checks it: the latest review is recorded done and found nothing standing, and no finding is still open. Fixing findings is never enough: a fresh review of the fixed change has to come back clean. The runner loops review → fix → review, up to a few rounds, and then stops and asks the user.

## First

The review runs as the task's review conversation, which the runner starts.

1. Read lifecycle state, obligations (including design-phase) and plan steps, implementation, and verification evidence from `verifying`.
2. The review does not run the verification itself, but it judges whether it was done: evidence that falls short sends the task back to `verifying`.

## Responsibilities

### Independent code review

The review is done by an agent **not involved** in building this task's docs or implementation — the review conversation's agent is one. Use a code-review skill when available. Every round reviews the whole change against its base, through every lens, and for each finding looks for every other place its root cause holds.

Record every finding on the task through the `review` noun, one at a time, as it is found: that is how the user sees them, and how their responses are tracked. A finding that is only in a reply is lost.

Track all findings until each has an explicit response:

- Fix with pointer to change/commit
- Out of scope
- Not critical / beyond requirements / not worth the cost
- Rejected: not a problem after all, with a note saying why

Fixing is a separate fix conversation, which the runner starts: its agent answers each open finding `fixed` or `rejected`. The other answers are the user's.

No outstanding unaddressed findings. A review that records findings is followed by a fix, and the fix by another review in a new session; the reviewer, not the fixer, decides the change is right.

### External approval

Approval comes from outside this automation: the PR's own review in `pr`. Never approve your own PR.

### Respond to findings

Implement fixes or document responses. Re-verify when fixes touch behavior covered by obligations.

## Done when

- The latest review conversation recorded the review finished, and recorded no finding that still stands (a finding the user marked out of scope or declined does not count).
- No finding is still open: each has an explicit response (fixed, with a pointer to the change or commit; out of scope; declined as not critical, beyond requirements, or not worth the cost; or rejected as not a problem, with a note saying why).

## Blockers

Unresolved findings keep the task in `review`; a finding only the user can answer is asked through the `decisions` noun.
