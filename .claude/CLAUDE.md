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
cargo test -p tod-ui some_test_name

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

Crates are layered so that each one below only depends on the ones after it.
The dependency direction is deliberate: **policy depends on transport, never the
reverse.**

- `crates/tod` — thin launcher binary. Owns `build.rs` (which installs the `process/` and `media/` bundles next to the executable) and little else.
- `crates/tod-ui` — all GPUI code: views, app shell, input/focus primitives, interview views, and the dev-only agent control socket.
- `crates/tod-cli` — the `tod-cli` binary that agents shell out to. Depends on `tod-core` + `tod-store` only (no GPUI, no agent transport) so it starts fast.
- `crates/tod-core` — policy and orchestration shared by the UI and the CLI: interview flow, process/phase rules, bundled process- and media-doc resolution, path/settings resolution, the task model, and agent context assembly. Decides *when* and *what* to persist.
- `crates/tod-agent` — agent transport: the provider interface and its implementations across platforms (Cursor, Claude, mock) and environments. **A leaf crate with no `tod-*` dependencies by design** — it knows how to hold conversations and sessions, and nothing about paths, settings, process docs, or persistence. It is told what to say and reports back.
- `crates/tod-store` — durable persistence. SQLite-backed (`rusqlite`) storage for **fleet** (agents/tasks/worktrees) and **outline** (task tree) data, plus credentials (OS keyring + `chacha20poly1305` encryption), settings, paths, and Linear API integration. Depends on `tod-agent` for the agent types it persists (`AgentPlatform`, `AgentLaunchOptions`).
- `crates/nov-viz` — a separate visualization crate (layout/nav/keyboard model), not part of the main app binary path.
- `assets/process/` — version-controlled source for agent behavior docs (SKILL files, agent definitions, manifest). Copied by `build.rs` to `target/{debug,release}/process/` so dev runs mirror an installed layout.
- `crates/tod/media/context/` — version-controlled agent context documents (see **Agent chat context** below). Copied by `build.rs` to `target/{debug,release}/media/`.

When adding code, put it in the lowest layer that can hold it. In particular, do
not reach into `tod-core` or `tod-store` from `tod-agent`: pass what the provider
needs in as a parameter instead (see `CursorAcpProvider::with_write_roots` and
the assembled-prompt argument to `start_deep_dive_chat` for the established
pattern).

### Data root resolution

There is a strict precedence for where durable state lives, checked in this order: `--data-root` CLI flag → `TOD_DATA_ROOT` env var → `install.toml` (in the OS config dir, e.g. `%APPDATA%\tod\install.toml` on Windows). If none are set, the app shows a first-run picker. Everything (the SQLite DB, YAML config, working-set JSON, logs) lives flat under that one data root.

**Never write the user's `install.toml`** (`%APPDATA%\tod\install.toml` on Windows, `~/Library/Application Support/tod/install.toml` on macOS, `~/.config/tod/install.toml` on Linux) — directly *or indirectly*. It is for the user's use only. Always use `--data-root` to point at a different root. In practice:

- Don't edit it, even if the app can't find its data root — pass `--data-root` instead.
- Never launch `tod` without `--data-root` (or `TOD_DATA_ROOT`). Without one the first-run picker appears, and completing it — including via the agent socket — calls `save_data_root` and overwrites the file.
- Code or tests that exercise `install.toml` must set `TOD_CONFIG_DIR` to a temp dir first. Don't rely on `XDG_CONFIG_HOME`: `dirs::config_dir()` ignores it on Windows and macOS.

Bundled agent docs resolve separately via `TodInstallPaths` (`TOD_PROCESS_ROOT` env → `{executable_dir}/process/` → walk-up-from-cwd fallback to `assets/process/`) — this is distinct from the data root and holds no user data. Agent context docs resolve the same way via `MediaPaths` (`TOD_MEDIA_ROOT` env → `{executable_dir}/media/` → walk-up to `crates/tod/media/`).

### Agent chat context

A chat icon appears in the upper-right of a context panel when the node has the
Agent capability, and opens an agent conversation scoped to that panel. Sessions
are **not** reused — every click starts a fresh one.

**Ctrl+J** opens an agent chat everywhere. `crates/tod-ui/src/ui/agent_chat.rs`
owns the app-wide `OpenAgentChat` action (bound with no key context); any surface
that offers a chat handles it with `on_action`, propagating when it can't open one,
and shows the badge via `chrome_control_with_shortcut_in_context(.., &OpenAgentChat, None, ..)`.

The context is assembled by `tod_core::agent_context` and held until the user
submits their first message — nothing reaches the agent before then. A chat
window holds one long-lived agent session (`AgentProvider::send_session_turn`):
the first message opens it — the context and the message go out together as one
turn, and the session is given its name (for Claude, a `custom-title` record
written once that first turn has created the session log; Cursor names its own
sessions) — and every later message sends only itself.
The provider keeps the agent process alive between messages; when the window
closes or the process idles out, the next message resumes the recorded
agent-side session id (`agent_runs.agent_session_id`) instead of replaying
history. Session names come from `tod_core::session_name` (surface, task title,
start time). The context is:

1. **Static layers** from `crates/tod/media/context/`, outermost first. The key
   is a `/`-separated path, and every ancestor level contributes: `obligations`
   loads `app.md` then `obligations.md`; a future `tasks/edit` would load
   `app.md`, `tasks.md`, then `tasks/edit.md`. Missing layers are skipped, so a
   new surface can ship with only its own file.
2. **A dynamic block** with the data root and the live selection — ids *and*
   text, so the agent can work with the content directly and only needs
   `tod-cli` for what it was not given.

To add a context, drop a new `.md` under `media/context/` and pass its key.

### `tod-cli` — the agent's interface to the data

Agents do not get raw database access; they get `tod-cli`, installed next to the
`tod` executable and documented for them in `media/context/app.md`. Every
mutation goes through `tod_store`'s `OutlineMutation` queue — the same path the
GUI uses — so invariants cannot be bypassed and the agent never sees the schema.

Adding a command means adding a noun/verb that wraps an existing mutation, not
new SQL. Keep `tod-cli`'s dependencies minimal: agents shell out to it
repeatedly, so startup cost is a feature.

### `tod-store::fleet` — agent/worktree orchestration

Tracks agents running against git worktrees: provisioning (`provision.rs`), launching (`launch.rs`, `runtime.rs`), reattaching to running processes (`reattach.rs`), terminal sessions (`terminal/`), prompt queuing (`prompt_queue.rs`), and an undo log (`undo.rs`). `store.rs` / `writer.rs` / `schema.rs` / `migration.rs` are the SQLite persistence core; `projection.rs` derives read-side views for the UI.

### `tod-store::outline` — task tree

A hierarchical task/outline model with its own DDL/migration path (`ddl.rs`, `migrate_interview.rs`), slug-based addressing (`slug.rs`), and import from the older interview-session format (`import.rs`).

### `crates/tod-core::interview` — conversational task creation

The interview flow turns a running conversation with an agent into obligations/tasks: `kickoff.rs` starts a session, `queue.rs`/`queue_watcher.rs` manage question flow, `replenishment.rs` decides when to ask for more, `routing.rs` decides what happens next (`interview_work_remains` gates whether the task list can proceed). `db.rs` is the interview-session store (separate from the fleet/outline stores in `tod-store`).

### `crates/tod-ui::views` and `ui`

GPUI views live under `views/` (task list, obligations, agent panels, transcripts, command history) and share input/focus primitives from `ui/`. See the keyboard-focus convention below — it applies to any new editable view.

### GPUI keyboard focus: navigation mode vs. edit mode

Multi-line and single-line text fields must not trap keyboard navigation. The established pattern (see `crates/tod-ui/src/interview/views/workspace.rs` for the reference implementation):

- **Navigation mode (default)**: parent view owns focus; arrow keys move a highlight among stops (buttons, fields, rows); text inputs are stops but stay **disabled** until edit mode; Enter or a click enters edit mode; Tab does *not* move between stops.
- **Edit mode**: a `*_editing` flag re-enables the `Input` and focuses it next frame; Escape exits edit mode and restores the nav highlight; for single-line fields Enter also exits/commits (bind via `key_context::including_input`); multi-line fields keep Enter for newlines and use Ctrl+Enter to submit.
- Disabled inputs must be removed from the GPUI tab order via `set_input_tab_stop` (see `crates/tod-ui/src/ui/key_context.rs`), or Tab will focus them with a cursor while typing is silently blocked.
- Use `key_context::excluding_input` / `NOT_INPUT` for surface-level shortcuts and `key_context::including_input` for handlers (Escape, Enter-to-commit) that must still fire while an `Input` has focus.

### Cross-panel keyboard navigation

Any multi-column view moves the focused panel with Left/Right. Where those keys already act on a panel's own content (the task tree collapses/expands and selects the parent with them), the panel binds **Ctrl+Left / Ctrl+Right** instead. `crates/tod-ui/src/ui/pane_nav.rs` owns the shared `PaneFocusLeft` / `PaneFocusRight` actions: `bind_pane_nav(cx, surface)` registers plain *and* Ctrl arrows, `bind_modified_pane_nav(cx, surface)` registers Ctrl only. Ctrl+arrows are registered on every multi-column surface, so the same chord crosses panels everywhere.

Drawer panels in the Tasks view do not move focus themselves — they emit a `FocusTaskList` event and the shell (`crates/tod-ui/src/app/window.rs`) routes it, mirroring how `Close` is handled.

### Feature flags

`agent-socket` (default-on) compiles the TCP UI-automation control socket into dev/CI builds; release builds should use `--no-default-features` so that code isn't present in the shipped binary at all (not just disabled at runtime).
