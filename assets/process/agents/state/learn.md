# State: `learn`

**Gate:** `learn` → `done`. The app checks it: this pass's retrospective is recorded through the `learn` noun. There is no certificate in this state.

## First

1. Read lifecycle state, obligations (including design-phase) and plan steps, the interview history (answered questions and interview memory), and evidence from the full lifecycle.
2. Read the **Work history** section of the context when there is one: every obligation and plan step that failed verification or was handed back, every code review finding, every gate criterion that did not pass, and how many conversations of each kind the node took. The plan and obligations only show where the work ended up — every step `verified` says nothing about how many attempts that took — so the history, not the final state, is what the retrospective is about. Never report a clean run from the final state alone.
3. The work history covers only this **pass**: a node sent back goes through its lifecycle again, and each earlier pass's retrospective is already stored. When it opens with "This pass began because …", that incoming change is why the node was reworked; say whether an earlier stage should have caught it.
4. If this pass's retrospective is already recorded (the `learn` noun of `tod-cli` lists it) → verify completeness and proceed to exit.

## Responsibilities

### Retrospective

Review the **full lifecycle**: requirements, design, planning, execution, verification, review, release.

Ask:

- What failed, and at which stage was it caught? Anything caught later than it could have been — a defect verification passed that review or the user found, a requirement no plan step covered — is a gap in an earlier stage's gate or role doc: name it.
- What slowed us down?
- What was unclear in docs, gates, or agents?
- What slipped through that should become a new item on a state's **Done when** checklist?
- What worked well?

### Capture findings

Record the retrospective with the `learn` noun of `tod-cli`: it is stored as this pass's record when the node reaches `done`, and never changes after. **Process improvements** when warranted: propose new **Done when** items or edits to state agent role files — only when there is something concrete to improve. The app or human applies catalog and bundle changes; do not assume filesystem access to the process bundle.

No requirement to change the process every time—only that the retrospective **ran** and outcomes are captured when there is something to improve.

## Done when

- [ ] You have reviewed what happened across the whole lifecycle, from the work history, not the final state alone.
- [ ] This pass's retrospective is recorded through the `learn` noun, with proposed changes to a state's **Done when** checklist, a state agent doc, or a side tool when there is something concrete to improve.

Recording it is what passes the gate: nothing else is needed.

## Note

`done` has no state agent. Reopening a closed node moves its lifecycle backward.
