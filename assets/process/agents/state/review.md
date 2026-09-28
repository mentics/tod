# State: `review`

**Gate:** `review` → `pr`. The app checks it: the review is recorded done, and no finding is still open.

## First

The review runs as the node's review conversation: the runner starts it, or the conversation view's **Review**.

1. Read lifecycle state, obligations (including design-phase) and plan steps, implementation, and verification evidence from `verifying`.
2. Functional correctness should already be established—do not treat this state as primary QA.

## Responsibilities

### Independent code review

The review is done by an agent **not involved** in building this node's docs or implementation — the review conversation's agent is one. Use a code-review skill when available.

Record every finding on the node through the `review` noun, one at a time, as it is found: that is how the user sees them, and how their responses are tracked. A finding that is only in a reply is lost.

Track all findings until each has an explicit response:

- Fix with pointer to change/commit
- Out of scope
- Not critical / beyond requirements / not worth the cost
- Rejected: not a problem after all, with a note saying why

Fixing is a separate fix conversation (the runner starts it, or the conversation view's **Fix**): its agent answers each open finding `fixed` or `rejected`. The other answers are the user's.

No outstanding unaddressed findings.

### External approval

Approval comes from outside this automation: the PR's own review in `pr`. Never approve your own PR.

### Respond to findings

Implement fixes or document responses. Re-verify when fixes touch behavior covered by obligations.

## Done when

- The review conversation recorded the review finished.
- No finding is still open: each has an explicit response (fixed, with a pointer to the change or commit; out of scope; declined as not critical, beyond requirements, or not worth the cost; or rejected as not a problem, with a note saying why).

## Blockers

Unresolved findings keep the node in `review`; a finding only the user can answer is asked through the `decisions` noun.
