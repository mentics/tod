# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tod` is a desktop task/agent management application built with **GPUI** (GPU-accelerated UI, Rust) and **gpui-component**. It manages "obligations", an interview-style workflow for turning conversation into tasks, and a "fleet" of coding agents (Claude, Cursor) working in worktrees.

## Commands

```bash
# Build and run (dev, agent-socket feature enabled by default)
cargo run -p tod

# Dogfooding: use a persistent local data root instead of OS app-data dir
export TOD_DATA_ROOT=.local/data
cargo run -p tod

# Fresh, isolated sandbox (recommended for testing changes)
rm -rf .local/test/my-sandbox
cargo run -p tod -- --data-root .local/test/my-sandbox --agent mock --no-focus

# Type-check across the workspace (matches CI)
cargo check --workspace --all-targets

# Run all tests
cargo test --workspace

# Run a single test
cargo test -p tod-store fleet::tests::some_test_name
cargo test -p tod some_test_name

# Release build — excludes the TCP agent-control socket entirely
cargo build --release -p tod --no-default-features

# Verify a bundled install without opening the UI
tod --verify-process-bundle
```

CI runs `cargo check --workspace --all-targets` on Ubuntu, Windows, and macOS. Code behind `#[cfg(target_os = "...")]` is only type-checked on the matching OS — a change can pass locally and still fail on another platform until CI or a same-OS build runs it.

### `--agent mock` for UI work

For any UI-facing change, prefer driving the real app over guessing: `--agent mock` gives an instant, in-process fake agent (no real API calls), and `--no-focus` lets it run without stealing OS focus while you keep working. `--agent cursor` drives the real Cursor Agent CLI over ACP and is only for rare protocol-level smoke tests.

### Agent control socket (dev/CI only, not in release builds)

Requires the default `agent-socket` feature. Launch with `--agent-socket-port PORT` (give parallel instances distinct ports) and drive it with a line-oriented protocol (`key`, `text`, `click`, `sync`, `shot`) — see [README.md](README.md) for the full command table and the `.local/agent/ui-smoke/` scripts. Use a random/unused port per subagent run to avoid clashing with other instances.

## Architecture

### Workspace layout

- `crates/tod-store` — durable persistence layer, no UI dependency. SQLite-backed (`rusqlite`) storage for **fleet** (agents/tasks/worktrees) and **outline** (task tree) data, plus credentials (OS keyring + `chacha20poly1305` encryption), settings, paths, and Linear API integration.
- `crates/tod` — the GPUI desktop application binary. Depends on `tod-store` for all persistence; owns UI, interview flow, and process orchestration.
- `crates/nov-viz` — a separate visualization crate (layout/nav/keyboard model), not part of the main app binary path.
- `assets/process/` — version-controlled source for agent behavior docs (SKILL files, agent definitions, manifest). Copied by `build.rs` to `target/{debug,release}/process/` so dev runs mirror an installed layout.

### Data root resolution

There is a strict precedence for where durable state lives, checked in this order: `--data-root` CLI flag → `TOD_DATA_ROOT` env var → `install.toml` (in the OS config dir, e.g. `%APPDATA%\tod\install.toml` on Windows). If none are set, the app shows a first-run picker. Everything (the SQLite DB, YAML config, working-set JSON, logs) lives flat under that one data root. **Never edit `install.toml` directly** — always use `--data-root` to point at a different root; this is enforced by a Cursor rule and should be treated the same way here.

Bundled agent docs resolve separately via `TodInstallPaths` (`TOD_PROCESS_ROOT` env → `{executable_dir}/process/` → walk-up-from-cwd fallback to `assets/process/`) — this is distinct from the data root and holds no user data.

### `tod-store::fleet` — agent/worktree orchestration

Tracks agents running against git worktrees: provisioning (`provision.rs`), launching (`launch.rs`, `runtime.rs`), reattaching to running processes (`reattach.rs`), terminal sessions (`terminal/`), prompt queuing (`prompt_queue.rs`), and an undo log (`undo.rs`). `store.rs` / `writer.rs` / `schema.rs` / `migration.rs` are the SQLite persistence core; `projection.rs` derives read-side views for the UI.

### `tod-store::outline` — task tree

A hierarchical task/outline model with its own DDL/migration path (`ddl.rs`, `migrate_interview.rs`), slug-based addressing (`slug.rs`), and import from the older interview-session format (`import.rs`).

### `crates/tod::interview` — conversational task creation

The interview flow turns a running conversation with an agent into obligations/tasks: `kickoff.rs` starts a session, `queue.rs`/`queue_watcher.rs` manage question flow, `replenishment.rs` decides when to ask for more, `routing.rs` decides what happens next (`interview_work_remains` gates whether the task list can proceed). `db.rs` is the interview-session store (separate from the fleet/outline stores in `tod-store`).

### `crates/tod::views` and `ui`

GPUI views live under `views/` (task list, obligations, agent panels, transcripts, command history) and share input/focus primitives from `ui/`. See the keyboard-focus convention below — it applies to any new editable view.

### GPUI keyboard focus: navigation mode vs. edit mode

Multi-line and single-line text fields must not trap keyboard navigation. The established pattern (see `crates/tod/src/interview/views/workspace.rs` for the reference implementation):

- **Navigation mode (default)**: parent view owns focus; arrow keys move a highlight among stops (buttons, fields, rows); text inputs are stops but stay **disabled** until edit mode; Enter or a click enters edit mode; Tab does *not* move between stops.
- **Edit mode**: a `*_editing` flag re-enables the `Input` and focuses it next frame; Escape exits edit mode and restores the nav highlight; for single-line fields Enter also exits/commits (bind via `key_context::including_input`); multi-line fields keep Enter for newlines and use Ctrl+Enter to submit.
- Disabled inputs must be removed from the GPUI tab order via `set_input_tab_stop` (see `crates/tod/src/ui/key_context.rs`), or Tab will focus them with a cursor while typing is silently blocked.
- Use `key_context::excluding_input` / `NOT_INPUT` for surface-level shortcuts and `key_context::including_input` for handlers (Escape, Enter-to-commit) that must still fire while an `Input` has focus.

### Cross-panel keyboard navigation

Any multi-column view moves the focused panel with Left/Right. Where those keys already act on a panel's own content (the task tree collapses/expands and selects the parent with them), the panel binds **Ctrl+Left / Ctrl+Right** instead. `crates/tod/src/ui/pane_nav.rs` owns the shared `PaneFocusLeft` / `PaneFocusRight` actions: `bind_pane_nav(cx, surface)` registers plain *and* Ctrl arrows, `bind_modified_pane_nav(cx, surface)` registers Ctrl only. Ctrl+arrows are registered on every multi-column surface, so the same chord crosses panels everywhere.

Drawer panels in the Tasks view do not move focus themselves — they emit a `FocusTaskList` event and the shell (`crates/tod/src/app/window.rs`) routes it, mirroring how `Close` is handled.

### Feature flags

`agent-socket` (default-on) compiles the TCP UI-automation control socket into dev/CI builds; release builds should use `--no-default-features` so that code isn't present in the shipped binary at all (not just disabled at runtime).
