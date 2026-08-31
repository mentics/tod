# State: `approved`

**Forward gate:** `approved` → `merged`

## On entry

1. Read lifecycle state and approval evidence.
2. If already merged to the integration branch → verify and proceed to exit.

## Responsibilities

Drive merge to the **agreed integration branch**:

- Open/update PR if needed; ensure CI passes.
- Resolve merge conflicts; re-run verification if merge affects behavior.
- Record merge commit/PR reference in evidence notes.

Watch builds triggered by merge as applicable.

## Forward gate rules (`approved` → `merged`)

Apply these prose rules (no DB checklist items for this transition):

- Changes are merged to the agreed integration branch. That merge is the gate.

## Exit

When changes are on the integration branch, return `forward_lifecycle: merged`.

## Blockers

CI failure, merge conflict requiring human decision, or branch policy block → `blocked`.
