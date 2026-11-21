# Process agents

Agent role definitions bundled with the app. The app reads these at launch and assembles prompts — agents receive assembled text, not file paths.

## State agents

Shared conventions: [state/base.md](state/base.md) + the state's role doc, whose **Done when** checklist is what the phase agent and the evaluator judge against. Gates are app checks; no agent evaluates them.

## Side tools

[tools/](tools/) — not wired in app yet.
