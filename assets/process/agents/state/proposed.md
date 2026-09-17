# State: `proposed`

**Forward gate:** `proposed` → `design`

## On entry

1. Read the node's lifecycle state, resolved obligations (including inherited), and details.
2. If obligations already satisfy the gate and the human has directed design → verify preconditions and proceed to exit.

## Responsibilities

### Capture

The user captures what they want in the app's **conversation** view: the conversation agent turns each piece of direction into requirements-phase obligations (and the node's details, when it has none), and flags what it is unsure of in the conversation's change set. This session does not run sequential Q&A.

- Obligations are in effect as soon as they are written. Never ask the user to confirm obligations.
- **Inherit, do not duplicate** — nodes inherit ancestor constraints automatically. Record only node-specific obligations and exceptions. Never copy or paraphrase inherited obligations at this node.
- Gaps appear as short notes in the conversation, not as a list of questions.
- Anything about how the work gets built stays a visible requirement as the user said it; design refines it.

## Forward gate rules (`proposed` → `design`)

Apply these prose rules in addition to any checklist criteria the app sends:

- User directs starting design (e.g. “let’s work on the design”).
- Agent reads **resolved obligations** for the node (including inherited ancestor obligations).
- **Obligation dedupe (blocking):** Compare node-local requirements and constraints against (1) inherited ancestor obligations, and (2) **sibling** nodes for near-duplicate cross-cutting obligations. The conversation agent normally keeps this satisfied as it places direction. If the same concern is restated at this node, duplicated across siblings, or conflicts with an ancestor:
  - **Do not advance** until it is resolved: drop the duplicate, keep an intentional specialization, or **elevate** it to an ancestor node and remove lower copies.
  - Re-check after edits.

## Exit

Advance when the human directs starting design and the `proposed` → `design` gate passes (including **obligation dedupe**). Return `forward_lifecycle: design` (app applies).

## Blockers

Product intent unclear, doc conflicts, or requirements not checkable → `paused`/`blocked`; do not invent product scope.
