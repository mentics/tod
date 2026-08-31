## Requirements interview

**Lifecycle:** `proposed` → forward gate **`design`** when complete.

### Coverage targets

Gather until design can start with clear product intent:

- **Goal** — what success looks like at a product level
- **Requirements** — numbered, measurable via statement and/or success criteria (omit redundant criteria)
- **Constraints** — binding limits that always hold

### Probe

- Prefer **principles / clusters** over per-feature clones
- Read inherited obligations before asking; skip settled concerns
- Use write-from-decision / `proposed_text` Accept — do not ask the user to approve a paraphrase of an answer they already gave
- **Do not** probe for non-goals, Links, Overview, or other optional metadata unless the human volunteers
- Ensure node requirements do not conflict with inherited obligations

### Outcomes (this phase)

| Belongs here | Does not belong here |
|--|--|
| Requirements, constraints, goal at the correct `layer` | Vendors, tools, constructions, step-level implementation choices |
| Node-specific deltas, exceptions, cross-sibling ownership | Duplicates of inherited ancestor obligations |

Wrong-phase detail → **phase overflow** (`to-process.md`), not requirements.

### Inheritance

Nodes inherit ancestor obligations automatically. Record only what is **new or exceptional** at this node. Never copy or paraphrase inherited text here.

### Completion

Enough measurable requirements and constraints to pass **obligation dedupe** and start design. Use defaults for local/reversible detail; record defaults in the transcript.

### Phase overflow

Read entity `to-process.md` before asking. Open items are already known — do not make the human restate. When consuming overflow from earlier sessions, promote only what belongs in requirements; leave design/planning detail parked with `suggested_phase: design | planning`.
