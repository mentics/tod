# State: `review`

**Forward gate:** `review` → `approved`

## On entry

The review runs as the node's review conversation, started from the lifecycle panel's **Review** button, not automatically on entry.

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

No outstanding unaddressed findings.

### External approval (always required)

**`review` → `approved` is never waived in autonomous mode.** Some process **outside this automation** must mark the change approved (currently human review).

Coordinate human/team review when applicable. Do not self-approve.

### Respond to findings

Implement fixes or document responses. Re-verify when fixes touch behavior covered by obligations.

## Forward gate rules (`review` → `approved`)

The app checks this gate itself from the node's data; no agent evaluates it:

- The review conversation recorded the review finished.
- No finding is still open: each has an explicit response (fixed, with a pointer to the change or commit; out of scope; or declined as not critical, beyond requirements, or not worth the cost — small cheap extras may still be taken).
- **Approval is always an external gate** (not waived in autonomous mode): once both checks pass, the user advances the node. That is the external approval; do not advance it yourself.

## Exit

The user advances the node to `approved` once every finding is answered; that is the external approval.

## Blockers

Waiting on external approver or unresolved findings → stay in `review` or `blocked`.
