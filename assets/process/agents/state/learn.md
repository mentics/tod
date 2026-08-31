# State: `learn`

**Forward gate:** `learn` → `done`

## On entry

1. Read lifecycle state, obligations, design/plan content, session transcripts, and evidence from the full lifecycle.
2. If a learn retrospective already ran with findings recorded → verify completeness and proceed to exit.

## Responsibilities

### Retrospective

Review the **full lifecycle**: requirements, design, planning, execution, verification, review, release.

Ask:

- What slowed us down?
- What was unclear in docs, gates, or agents?
- What slipped through that should become a new **gate criterion** (app database catalog)?
- What worked well?

### Capture findings

Record outcomes in evidence notes and/or session transcripts. **Process improvements** when warranted: propose new **gate criteria** rows or edits to state agent role files — only when there is something concrete to improve. The app or human applies catalog and bundle changes; do not assume filesystem access to the process bundle.

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
