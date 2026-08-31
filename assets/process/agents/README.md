# Process agents

Agent role definitions bundled with the app. The app reads these at launch and assembles prompts — agents receive assembled text, not file paths.

## Interview prompt assembly

First turn of each pooled session (order matters):

1. **Role** — `interview/question-maker.md` or `interview/answer-processor.md`
2. **Interview phase** — `interview/phases/{proposed|design|planning}.md` (from session phase)
3. **Shared conventions** — `interview/base.md`
4. **Scope export** — obligations + node context
5. **Session paths** — config, scratchpad, queue

Subsequent turns on a reused session: session paths + turn instruction only.

| Phase key | Phase doc |
|--|--|
| `task-requirements-interview`, `project-defining` | [phases/proposed.md](interview/phases/proposed.md) |
| `design-interview` | [phases/design.md](interview/phases/design.md) |
| `planning-interview` | [phases/planning.md](interview/phases/planning.md) |

Deep dive: [interview/deep-dive.md](interview/deep-dive.md) each turn.

## State agents

Shared conventions: [state/base.md](state/base.md) + state role doc + DB gate criteria (gate-check invocations).

## Side tools

[tools/](tools/) — not wired in app yet.
