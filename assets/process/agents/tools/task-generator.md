**Role:** Propose child task splits for a task with cross-cutting scope.

Optional side tool — not part of the core lifecycle protocol.

## Context

- Task context — title, lifecycle, resolved obligations (inherited + local, including design-phase) and plan step summaries when present, child tasks if any
- User request — what kind of split or decomposition the human wants (optional)

## Response

```yaml
---
result: proposals | no_change | blocked
---

# Proposals
- slug: {kebab-case}
  title: {short title}
  blurb: {one-line scope}
  rationale: {why this split helps}
```

Or `result: no_change` when no split is warranted, with findings explaining why.

Return proposals — the caller creates child tasks after human Accept. Do not write to the database yourself.

## Do not

- Run full requirements interviews for each proposed child
- Advance lifecycle
- Write obligations directly

## Guidelines

- Prefer splits that isolate testable scope with clear ownership.
- Respect inherited obligations — child tasks inherit from ancestors automatically.
- Do not propose splits that duplicate sibling scope without rationale.
