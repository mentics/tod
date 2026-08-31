# State: `merged`

**Forward gate:** `merged` → `released`

## On entry

1. Read lifecycle state and merge evidence.
2. If already released to the agreed production/runtime environment → verify and proceed to exit.

## Responsibilities

Drive **release** to the agreed production/runtime environment:

- Follow the node/project release process (deploy pipeline, tags, etc.).
- Watch release/build pipelines; record release identifier in evidence.
- On failure, diagnose or escalate—do not mark `released` without actual release.

## Forward gate rules (`merged` → `released`)

Apply these prose rules (no DB checklist items for this transition):

- Changes are released to the agreed production/runtime environment. That release is the gate.

## Exit

When release completes, return `forward_lifecycle: released`.

## Blockers

Release pipeline failure or missing deploy permissions → `blocked`.
