# UI scaffolding

Project: `doc/process/projects/tod/`

## Goal

Deliver a runnable local desktop application shell for tod — no navigation chrome or placeholder pages in this task.

## Requirements

1. Runnable desktop shell — `cargo run` opens a desktop application window on the developer's OS.
   - Success criteria:
     - Running `cargo run` from the project produces a visible desktop application window

2. No placeholder UI surfaces — This task does not require app shell navigation, task/agent lists, detail pages, status area, notifications queue, or settings as routes or views.

3. Out of scope — The following are explicitly excluded (not even stubbed): agent launch and runtime operations; external integrations (Slack, Linear, GitHub); fuzzy search on lists; real fleet persistence and JSON import; credential management UI.

## Constraints

1. UI stack — Use GPUI and gpui-component (same stack as Zed).

2. Dev preview only — No packaged installable desktop build is required in this task.

3. Cross-platform — Verification on one development OS is sufficient; multi-OS proof is deferred.

4. Keyboard — Keyboard operability requirements are deferred to a later task.
