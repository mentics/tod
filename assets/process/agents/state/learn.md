# State: `learn`

**Forward gate:** `learn` → `done`

## On entry

1. Read lifecycle state, obligations (including design-phase) and plan steps, the interview history (answered questions and interview memory), and evidence from the full lifecycle.
2. Read the **Work history** section of the context when there is one: every obligation and plan step that failed verification or was handed back, every code review finding, every gate criterion that did not pass, and how many conversations of each kind the node took. The plan and obligations only show where the work ended up — every step `verified` says nothing about how many attempts that took — so the history, not the final state, is what the retrospective is about. Never report a clean run from the final state alone.
3. If a learn retrospective already ran with findings recorded → verify completeness and proceed to exit.

## Responsibilities

### Retrospective

Review the **full lifecycle**: requirements, design, planning, execution, verification, review, release.

Ask:

- What failed, and at which stage was it caught? Anything caught later than it could have been — a defect verification passed that review or the user found, a requirement no plan step covered — is a gap in an earlier stage's gate or role doc: name it.
- What slowed us down?
- What was unclear in docs, gates, or agents?
- What slipped through that should become a new **gate criterion** (app database catalog)?
- What worked well?

### Capture findings

Record outcomes in evidence notes and/or interview memory. **Process improvements** when warranted: propose new **gate criteria** rows or edits to state agent role files — only when there is something concrete to improve. The app or human applies catalog and bundle changes; do not assume filesystem access to the process bundle.

No requirement to change the process every time—only that the retrospective **ran** and outcomes are captured when there is something to improve.

## Forward gate rules (`learn` → `done`)

Apply these prose rules (no DB checklist items for this transition):

- **`learn` phase complete** for this task: the agent has reviewed what happened across the lifecycle and captured findings aimed at making the process more efficient and effective.
- Findings recorded (journal and/or updates to gate criteria catalog, state agent docs, or side tools when warranted). See retrospective responsibilities above.
- No requirement to change the process every time—only that the retrospective ran and outcomes are captured when there is something to improve.

## Exit

When learn phase is complete per the `learn` → `done` gate, return `forward_lifecycle: done`.

## Note

`done` has no state agent. Reopening a closed node moves lifecycle backward via app + gate re-check.
