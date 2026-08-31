# State: `review`

**Forward gate:** `review` → `approved`

## On entry

1. Read lifecycle state, obligations, design/plan content, implementation, and verification evidence from `verifying`.
2. Functional correctness should already be established—do not treat this state as primary QA.

## Responsibilities

### Independent code review

Spawn a **clean subagent** not involved in building this node’s docs or implementation. Use a code-review skill when available.

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

Apply these prose rules (no DB checklist items for this transition):

- Every review finding has an explicit response (fix with pointer to change/commit; out of scope; or not critical / beyond requirements / not worth the cost—small cheap extras may still be taken).
- No outstanding **unaddressed** findings.
- **Approval is always an external gate** (not waived in autonomous mode): some process outside this automation marks the change approved. On this team that is currently human review; the process only requires that the external gate fires.

## Exit

When every finding is addressed and **external approval** is recorded, return `forward_lifecycle: approved`.

## Blockers

Waiting on external approver or unresolved findings → stay in `review` or `blocked`.
