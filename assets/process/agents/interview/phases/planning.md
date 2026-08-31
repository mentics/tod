## Implementation interview

**Lifecycle:** `planning` → forward gate **`ready`** when complete.

### Coverage targets

Gather until the node can enter **`active`** with an actionable plan:

- How the work will be built **step-by-step**
- How verification will prove **each requirement**
- Assumptions — accept or convert to requirements
- Named constructions from design (when present) carried into plan steps

### Probe

- Prefer write-from-decision / `proposed_text` when constructions are clear
- Do not re-ask settled obligations or design decisions
- Record waiver in transcript if explicitly skipped

### Outcomes (this phase)

| Belongs here | Does not belong here |
|--|--|
| **Plan** extra content — ordered steps, traceability to requirements | New design spikes without decision trees |
| Obligation updates when planning reveals missing requirements | Open phase-overflow items |

**Traceability:** each requirement maps through the plan to verification. List assumptions explicitly.

### Drain phase overflow (blocking)

Before this phase completes, **every open item** in entity `to-process.md` must be consumed, promoted, or explicitly discarded. None may remain blocking **`planning` → `ready`**. When this interview drains, the file should be empty or deleted.

### Completion

Plan is **actionable** and traceable; assumptions explicit; overflow drained. Interactive mode may include human look-over before `ready` — autonomous mode waives that when other gate criteria pass.
