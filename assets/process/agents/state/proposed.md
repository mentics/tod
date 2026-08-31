# State: `proposed`

**Forward gate:** `proposed` → `design`

## On entry

1. Read the node's lifecycle state, resolved obligations (including inherited), and any phase-overflow items.
2. If obligations already satisfy the gate and the human has directed design → verify preconditions and proceed to exit.

## Responsibilities

### Requirements interview

Run a **requirements interview** via the app (question maker + answer-processor sessions — not sequential Q&A in this session). Help draft and refine **requirements proposals** for human approval:

- Goal; numbered requirements (measurable via statement and/or success criteria — omit redundant criteria); constraints.
- **Inherit, do not duplicate** — nodes inherit ancestor obligations automatically. Record only node-specific obligations, exceptions/specializations, and cross-sibling ownership. Never copy or paraphrase inherited obligations at this node.
- Do **not** probe for non-goals, Links, Overview, or other optional metadata. Record them only if the human volunteers.
- Ensure node requirements do not conflict with inherited obligations.
- Prefer **principles / clusters** over per-feature clones; read inherited scope before asking; use write-from-decision / `proposed_text` Accept so the user is not asked to approve a paraphrase of an answer they already gave.

Record each session as a **transcript** (exact session content).

Use defaults for local/reversible detail; record defaults and user replies in the transcript. Detail that belongs in design/planning (vendors, tools, constructions) is phase overflow — not forced into requirements at this state.

**Write on accept / write-from-decision:** when the user Accepts official wording, or a decision fully determines the obligation, the answer processor returns structured outcomes; the **app** persists obligations. Do not batch “already-accepted” items for a later write-permission ask.

### Propose, do not own

Do not invent product obligations. Per-item Accept during the interview is permission to record that item. After the interview drains, an optional **organize pass** side tool may reorder/group obligations before exit look-over.

## Forward gate rules (`proposed` → `design`)

Apply these prose rules in addition to any checklist criteria the app sends:

- User directs starting design (e.g. “let’s work on the design”).
- Agent reads **resolved obligations** for the node (including inherited ancestor obligations).
- **Interview** runs unless the user explicitly waives it; waiver (if any) is recorded in the session transcript.
- **Obligation dedupe (blocking):** Compare node-local requirements and constraints against (1) inherited ancestor obligations, and (2) **sibling** nodes for near-duplicate cross-cutting obligations. If the same concern is restated at this node (including meta-items like “conform to parent obligations”), duplicated across siblings, or conflicts with an ancestor:
  - **Do not advance** until the human resolves: drop the duplicate, keep an intentional specialization (note why), or **elevate** to an ancestor node and remove lower copies.
  - Record the resolution in transcript or journal. Re-check after edits.

## Exit

Advance when the human directs starting design and the `proposed` → `design` gate passes (including **obligation dedupe**). Return `forward_lifecycle: design` (app applies).

## Blockers

Product intent unclear, doc conflicts, or requirements not measurable → `paused`/`blocked`; do not invent requirements.
