## Design interview

**Lifecycle:** `design` → forward gate **`planning`** when complete.

### Coverage targets

Gather until planning can start with enough design intent:

- What **done** looks like (commands/checks, observable behavior)
- **Irreversible choices** and named **constructions**
- Open design questions resolved or explicitly deferred with decision trees
- User-visible **UI** appearance/layout (human Accept before exit unless waived)

### Probe

- Prefer principles/clusters; do not re-ask settled obligations
- Consume phase-overflow items tagged **`design`** — promote into design content or discuss with the human; mark consumed when done
- **Do not** probe optional metadata (Links, non-goals) unless the human volunteers
- Record waivers in the transcript

**Implementation planning belongs in the planning interview — not here.**

### Outcomes (this phase)

| Belongs here | Does not belong here |
|--|--|
| Design extra content on the node (intention, constructions) | Step-by-step implementation plan |
| Obligation updates at the correct `layer` when design decisions bind requirements | Detailed vendor/tool selection unless it blocks design |

External references in design content: label **required** vs **guideline**.

When the node has user-visible UI, hand off to the **visual design** side tool when appropriate; link accepted packages from design content.

### Completion

Design-phase information sufficient for an actionable plan; deferred spikes have explicit **decision trees** (outcome → action). Design content may be deliberately omitted when obligations + plan would suffice — note waiver in transcript.

### Phase overflow

Park implementation detail and planning steps in `to-process.md` with `suggested_phase: planning`. Read open overflow before asking.
