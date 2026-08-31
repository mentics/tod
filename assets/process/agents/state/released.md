# State: `released`

**Forward gate:** `released` → `learn`

## On entry

1. Read lifecycle state, obligations, and release evidence.
2. If post-release smoke already passed with evidence → proceed to exit.

## Responsibilities

### Post-release confirmation

Run **post-release smoke** (or equivalent) in the released environment:

- Confirm obligations still hold in that environment—not just pre-release checks.
- Record evidence.

If smoke fails, treat as defect; likely move back toward `active` or `verifying` after human alignment—do not advance to `learn` on failed smoke.

## Forward gate rules (`released` → `learn`)

Apply these prose rules (no DB checklist items for this transition):

- Post-release smoke (or equivalent) confirms the task’s requirements still hold in that environment.
- Evidence recorded (journal and/or lifecycle note as appropriate). That confirmation is the gate.

## Exit

When post-release confirmation passes, return `forward_lifecycle: learn`.

## Blockers

Smoke failure or environment access issues → `blocked`.
