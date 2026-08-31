# Process bundle

Agent role definitions **bundled with tod** (`assets/process/`). At build time this tree is copied next to the binary as `process/`.

## Interaction model

The app reads bundled markdown at launch and **inlines** instructions and context in agent prompts. Agents do not fetch these paths remotely.

| Interaction | Bundled docs | App status |
|--|--|--|
| Interview question maker + answer processor | role doc + `phases/{proposed\|design\|planning}.md` + `base.md` + scope export | **Implemented** |
| Deep dive | `agents/interview/deep-dive.md` + question context | **Implemented** |
| State agents | `agents/state/base.md` + `agents/state/{state}.md` + DB gate criteria | **Docs ready** (runtime wiring pending) |
| Side tools | `agents/tools/*.md` | **Docs ready** (runtime wiring pending) |

Working notes under `doc/new-reqs/` are for humans editing the bundle — not sent to agents.

## Layout

```text
assets/process/
  README.md              # this file
  agents/
    README.md            # index of agent role files
    state/               # base.md + one file per lifecycle state (includes forward gate prose)
    interview/           # base.md, question maker, answer-processor, deep-dive
    tools/               # optional side tools
```

Gate **checklist criteria** live in the app database (`gate_criteria`); see [gate-criteria.md](../../doc/new-reqs/gate-criteria.md).

## Finding the bundle at runtime

1. `TOD_PROCESS_ROOT` environment variable
2. `{executable_dir}/process/` (installed layout)
3. Walk up from cwd for `assets/process/README.md` (dev checkout)

User data (obligations, interviews, DB) lives under the app **data root** — not in this directory.

## What to read when

| Goal | Start here |
|------|------------|
| Agent roles | [`agents/README.md`](agents/README.md) |
| Interview protocol (in bundled form) | [`agents/interview/base.md`](agents/interview/base.md) + role docs |
| State agent protocol (in bundled form) | [`agents/state/base.md`](agents/state/base.md) + state role docs |
| Gate checklist criteria (DB) | [`doc/new-reqs/gate-criteria.md`](../../doc/new-reqs/gate-criteria.md) |
