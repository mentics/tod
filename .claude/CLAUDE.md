# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tod` is a desktop task/agent management application built with **GPUI** (GPU-accelerated UI, Rust) and **gpui-component**. It manages "obligations", a conversation view where the user directs an agent that edits the outline (with every change reversible), an interview-style workflow for turning conversation into tasks, and a "fleet" of coding agents (Claude, Cursor) working in worktrees.

## Commands

```bash
cargo run -p tod -- --data-root .local/agent/scratchpad/tod/root-<unique if needed>

# Fresh, isolated sandbox (recommended for testing changes)
rm -rf .local/test/my-sandbox
cargo run -p tod -- --data-root .local/agent/scratchpad/tod/root-<unique if needed> --agent mock --no-focus

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

`cargo run -p tod` rebuilds only `tod`, never the `tod-cli` beside it. After
changing anything `tod-cli` is built from, run `cargo build -p tod-cli` too: a
conversation refuses to send a turn when the two were built from different
source (`tod_core::CLI_BUILD_STAMP`), rather than let the agent work from
stale commands.

CI runs `cargo check --workspace --all-targets` on Ubuntu, Windows, and macOS. Code behind `#[cfg(target_os = "...")]` is only type-checked on the matching OS — a change can pass locally and still fail on another platform until CI or a same-OS build runs it.

### Running builds/tests without getting stuck

- Always run `cargo check` / `cargo test` / `cargo build` with an explicit ~120s timeout, even when you expect them to be fast. Tests can hang intermittently (a spawned process, a port, a wait on stdin); a timeout is cheap insurance and should be the default, not a special case. If 120s proves too short for a particular command, raise it for that command rather than dropping the policy.
- Scope test runs to what the change actually touches (`cargo test -p tod-store ...`) rather than defaulting to `cargo test --workspace`, which pulls in the full GPUI build. Use `--workspace` when the change is broad or you need the CI-equivalent check.
- Piping through `grep`/`head` to cut noise is fine and preferred — don't remove that just to see more output.
- If you do intentionally background a long-running command (e.g. an ultra review, a release build), don't just sit idle waiting on it — either continue other work, or if there's nothing else to do, poll it periodically (sleep, then check) rather than blocking indefinitely on a single wait.

### `--agent mock` for UI work

For any UI-facing change, prefer driving the real app over guessing: `--agent mock` gives an instant, in-process fake agent (no real API calls), and `--no-focus` keeps every window it opens from stealing OS focus while the user keeps working (the agent control socket implies it, but pass it anyway on any automated launch). `--agent cursor` drives the real Cursor Agent CLI over ACP and is only for rare protocol-level smoke tests.

### Agent control socket (dev/CI only, not in release builds)

Requires the default `agent-socket` feature. Launch with `--agent-socket-port PORT` (give parallel instances distinct ports) and drive it with a line-oriented protocol (`key`, `text`, `click`, `sync`, `shot`) — see [README.md](README.md) for the full command table and the `.local/agent/ui-smoke/` scripts. Use a random/unused port per subagent run to avoid clashing with other instances.

## Architecture

### Workspace layout

Crates are layered so that each one below only depends on the ones after it.
The dependency direction is deliberate: **policy depends on transport, never the
reverse.**

- `crates/tod` — thin launcher binary. Owns `build.rs` (which installs the `process/` and `media/` bundles next to the executable) and little else.
- `crates/tod-ui` — all GPUI code: views, app shell, input/focus primitives, the conversation view, interview views, and the dev-only agent control socket.
- `crates/tod-cli` — the `tod-cli` binary that agents shell out to. Depends on `tod-core` + `tod-store` only (no GPUI, no agent transport) so it starts fast.
- `crates/tod-core` — policy and orchestration shared by the UI and the CLI: conversation and interview flow, process/phase rules, bundled process- and media-doc resolution, path/settings resolution, the task model, and agent context assembly. Decides *when* and *what* to persist.
- `crates/tod-agent` — agent transport: the provider interface and its implementations across platforms (Cursor, Claude, mock) and environments. **A leaf crate with no `tod-*` dependencies by design** — it knows how to hold conversations and sessions, and nothing about paths, settings, process docs, or persistence. It is told what to say and reports back.
- `crates/tod-store` — durable persistence. SQLite-backed (`rusqlite`) storage for **fleet** (agents/tasks/worktrees) and **outline** (task tree) data, plus credentials (OS keyring + `chacha20poly1305` encryption), settings, paths, and Linear API integration. Depends on `tod-agent` for the agent types it persists (`AgentPlatform`, `AgentLaunchOptions`).
- `crates/nov-viz` — a separate visualization crate (layout/nav/keyboard model), not part of the main app binary path.
- `assets/process/` — version-controlled source for agent behavior docs (SKILL files, agent definitions, manifest). Copied by `build.rs` to `target/{debug,release}/process/` so dev runs mirror an installed layout.
- `crates/tod/media/context/` — version-controlled agent context documents (see **Agent chat context** below). Copied by `build.rs` to `target/{debug,release}/media/`.

When adding code, put it in the lowest layer that can hold it. In particular, do
not reach into `tod-core` or `tod-store` from `tod-agent`: pass what the provider
needs in as a parameter instead (see `CursorAcpProvider::with_write_roots` and
the assembled-prompt argument to `send_session_turn` for the established
pattern).

### Data root resolution

There is a strict precedence for where durable state lives, checked in this order: `--data-root` CLI flag → `TOD_DATA_ROOT` env var → `install.toml` (in the OS config dir, e.g. `%APPDATA%\tod\install.toml` on Windows). If none are set, the app shows a first-run picker. Everything (the SQLite DB, YAML config, working-set JSON, logs) lives flat under that one data root.

**Never write the user's `install.toml`** (`%APPDATA%\tod\install.toml` on Windows, `~/Library/Application Support/tod/install.toml` on macOS, `~/.config/tod/install.toml` on Linux) — directly *or indirectly*. It is for the user's use only. Always use `--data-root` to point at a different root. In practice:

- Don't edit it, even if the app can't find its data root — pass `--data-root` instead.
- Never launch `tod` without `--data-root` (or `TOD_DATA_ROOT`). Without one the first-run picker appears, and completing it — including via the agent socket — calls `save_data_root` and overwrites the file.
- Code or tests that exercise `install.toml` must set `TOD_CONFIG_DIR` to a temp dir first. Don't rely on `XDG_CONFIG_HOME`: `dirs::config_dir()` ignores it on Windows and macOS.

Bundled agent docs resolve separately via `TodInstallPaths` (`TOD_PROCESS_ROOT` env → `{executable_dir}/process/` → walk-up-from-cwd fallback to `assets/process/`) — this is distinct from the data root and holds no user data. Agent context docs resolve the same way via `MediaPaths` (`TOD_MEDIA_ROOT` env → `{executable_dir}/media/` → walk-up to `crates/tod/media/`).

### Agent chat context

**Ctrl+J** opens the conversation view (`crates/tod-ui/src/conversation/`) from
anywhere, focused on the selection. `crates/tod-ui/src/ui/agent_chat.rs` owns the
app-wide `OpenAgentChat` action (bound with no key context) and the
`OpenConversation { focus }` action the shell handles. A view that knows its
selection handles `OpenAgentChat` with `on_action` and dispatches
`OpenConversation`, propagating when it has nothing to offer; the shell root is
the fallback (the task tree's selection, else the whole project). The badge sits
in the title bar (`render_shortcut_pill_in_context(.., &OpenAgentChat, None, ..)`).
`proposed` and `design` nodes open the same view from the lifecycle panel, and
the app nav's "Conversation" opens it on the project.

On a node with the Lifecycle capability, the step that moves it along —
Implement, Verify, Review, the gate check, Advance — sits beside Send
(`conversation/lifecycle.rs`). The gate check's verdict, with a Waive per
failing criterion, sits in the side pane beneath the list the node's state is
about (obligations, plan, or findings), never above the input. Gate checks, waiving, advancing, and on-entry
turns run in `views::lifecycle_control::LifecycleController`, one entity the
shell shares between the conversation view and the lifecycle panel, so a
check started in either shows in both. The manual escape hatches (force
advance, revert, open interview) stay in the panel.

A node's state can stop holding after the fact: its obligations or plan
changed since it entered `ready` (compared with the snapshot
`tod_store::lifecycle_baseline` takes then, so a reversed change stops
counting), or verification failed. `tod_core::lifecycle_validity` decides
that deterministically and names the latest state that still holds; the
lifecycle panel shows it as an orange callout with **Move back**, re-judged on
every store change. The app never moves the node itself: the user confirms,
since a conversation's changes may still be reversed.

Every conversation has a **focus** (project, node, obligation, or plan step) and
the view lists only that focus's conversations. Ctrl+J opens the focus's most
recent one, or an empty one that is saved on its first send; new conversations
start only from the picker (Ctrl+N). The view always says **reverse**, never
"undo" — reversal is built from the conversation's own action log, not the
Ctrl+Z history. See the conversation section below and
`doc/conversation/spec.md`.

A conversation holds **one agent session at a time**, keyed
`conversation-<id>`, with its resume id stored on the conversation row
(`conversations.agent_session_id`, not `agent_runs`). The first message sends
the opening context and the message as one turn; every later message sends only
the *delta* (the user's own edits and reversals since the previous turn, and
items changed elsewhere) plus the message. When the session outgrows
`context_budget_tokens`, or can no longer be resumed, the driver rotates to a fresh session seeded with a snapshot, and
the transcript shows a "Started a fresh agent session" marker. **Reply rule:**
the agent never describes what it changed — the user sees the change set — so
an empty reply is normal (shown as "Done, no notes"); `surface/conversation.md`
states this, and that the agent acts without confirming because everything is
reversible.

Implement, Verify, Review, and the action panel's Chat now run in the conversation view (see
**Protocols** below). The one chat left on the old path is the visual-design
panel's embedded chat, which uses `InteractiveAgentView`; it, the view, and
`InteractiveAgentWindow` are slated for deletion once visual design is rebuilt
as a protocol. What follows describes that old path. Its context is assembled by
`tod_core::agent_context` and held until the user submits their first message —
nothing reaches the agent before then. A chat window holds one long-lived agent session
(`AgentProvider::send_session_turn`):
the first message opens it — the context and the message go out together as one
turn, and the session is given its name (for Claude, a `custom-title` record
written as soon as that first prompt has started the session log; Cursor names
its own sessions) — and every later message sends only itself. The name
(`SessionTurn::title`) goes with every turn: all of a session's agent traffic
is filed under its key and listed under that name in the agent transcripts
window.
The provider keeps the agent process alive between messages; when the window
closes or the process idles out, the next message resumes the recorded
agent-side session id (`agent_runs.agent_session_id`) instead of replaying
history. Session names come from `tod_core::session_name` (surface, task title,
start time) for both kinds. For every surface, the context is:

1. **Static fragments** from `crates/tod/media/context/`, named by an explicit
   ordered list — there is **no** implicit ancestor chain. Fragments are
   organized by category, and a prompt is assembled in this order:

   | Category | Rule |
   |---|---|
   | `stance/` | **Exactly one.** Interactive-chat, autonomous-session, one-shot, or agent-to-agent. The *only* place behavioral policy (confirm-or-act, brevity, whether questions are possible) may live. |
   | `domain/` | Only the concepts the surface actually handles — outline, obligations, lifecycle, capabilities, plan. |
   | `cli/` | `cli/intro` plus only the nouns the surface uses. The only place `tod-cli` syntax is documented. |
   | `surface/` | What this particular job is. Scoped exceptions to the stance are stated here, *as* exceptions. |

2. **A dynamic block** with the data root and the live selection — ids *and*
   text, so the agent can work with the content directly and only needs
   `tod-cli` for what it was not given.

Both halves are named by the surface's `ContextRecipe` in
`tod_core::context_recipes`, which holds every surface's fragment list and
block list and is the one place they are registered. `build_message` there is
the single assembler — there is no per-surface prompt builder. The dynamic
block renderers live in `tod_core::dynamic` and know nothing about which
surface they are serving; anything surface-specific is a parameter on the block
(e.g. `SelectedObligation { fallback }`).

The tests in `context_recipes` enforce the rules above. A missing fragment is
an error at load time (the agent is not launched), and the tests catch a
misspelled recipe before it ever ships.

One fragment is not in any recipe: `workspace/codebase` (no dev containers)
is compiled into `tod_core::codebase_rules` and appended to the opening of
**every** agent whose working directory is inside a git checkout, at each
launch site (conversation driver, interview driver, fleet prompt, visual-design
chat). A new launch site must call `with_codebase_rules` too.

To add a surface: write a `surface/*.md`, add a `ContextRecipe` const, register
it in `ALL_RECIPES`, and call `build_message`. `doc/agent-context-map.md` maps
every surface to what it needs and why.

### `tod-cli` — the agent's interface to the data

Agents do not get raw database access; they get `tod-cli`, installed next to the
`tod` executable and documented for them under `media/context/cli/`. Every
mutation goes through `tod_store`'s `OutlineMutation` queue — the same path the
GUI uses — so invariants cannot be bypassed and the agent never sees the schema.
The `node` noun (list/show/create/rename/move/delete) is CRUD on outline nodes
themselves, addressed by the node's stable slug or full UUID; every other noun
(`obligations`, `plan`, etc.) acts on a node that already exists.

Adding a command means adding a noun/verb that wraps an existing mutation, not
new SQL. Keep `tod-cli`'s dependencies minimal: agents shell out to it
repeatedly, so startup cost is a feature.

`media/context/cli/` is the one canonical place `tod-cli` command syntax is
documented for agents — one `cli/<noun>.md` per noun the binary dispatches,
which surfaces opt into, instead of being re-explained inline wherever it's
used. A prompt carries only the nouns its job is most likely to need: the
conversation and chat recipes are `situational`, adding obligations, content,
or plan by the focus and its lifecycle state
(`conversation::context::situational_cli`), and `cli/intro` always explains
how to find the rest (`tod-cli help <words>` searches every noun's commands).
An option no command reads is ignored with a warning on stderr.
Adding a noun means adding its fragment and putting it in the recipes
that need it. Three sets of tests hold this together:

- `tod_cli::doc_sync` pins each fragment to that noun's own `USAGE` string —
  every verb documented, no verb invented. This is what catches a new verb
  whose agent-facing docs were never updated.
- `context_recipes::tests::tod_cli_syntax_appears_only_under_cli` keeps syntax
  out of `stance/`, `domain/`, and `surface/`.
- `context_recipes::tests::process_docs_do_not_carry_their_own_command_tables`
  keeps it out of `assets/process/` too. Those role docs may still say *which*
  noun applies in prose; they must not restate how to call it.

### `tod-core::conversation` / `tod-store::conversation` — the conversation view's agent and log

`tod_store::conversation` (schema v36+) holds `conversations`, their transcript
`conversation_turns` (`user` / `agent` / `error` / `rotation`), the
`conversation_actions` each made, and per-conversation `conversation_flags` (the
unsure flag belongs to the change set, not the item). An agent writing as
`TOD_INTERVIEW_ACTOR=conversation:<uuid>` goes through the ordinary
`InterviewCommand::Outline` path; `record_and_execute` records each node,
obligation, and plan-step mutation with its before/after state **in the same
transaction** (only inside `run_interview`, never the batched flush).
`net_changes` projects the change set (net per item, never by turn; created then
deleted is hidden), `reverse_actions` applies inverse mutations and reports
conflicts and dependents for confirmation, and user edits from the view go
through `ConversationEdit`. Capability changes (enable, disable, each
capability's settings) are recorded too, as one `capabilities` item per node;
a disable archives every row it removes (`outline::archive`, following
`ON DELETE CASCADE` from the schema) so reversing it restores them. Content and
lifecycle mutations are not recorded, and nothing sets a lifecycle state
except the lifecycle process.

**Protocols.** A conversation's `protocol` (schema v39) decides what kind of
conversation it is: the context recipe, the working directory, the turn
envelope, what "done" means, whether the app loops it without the user, and
which side pane the view shows. Replies are never parsed: structured state
the app needs, the agent records through `tod-cli` as it works.
`tod_core::conversation::protocol` holds the `Protocol` trait and
`protocol_for`, the one registry; `implement.rs` is the implementation
protocol, whose loop keeps sending the agent back to open plan steps until
the plan is done and a test run it recorded (`tod-cli tests record`) is
green; a `blocked` plan step hands back to the user. `verify.rs` is its
mirror for a `verifying` node (the lifecycle panel's Verify). What it
verifies is the node's **obligations**, not only the plan: the agent
exercises each one in the running work and records a verdict with evidence
through `tod-cli verdicts` (`tod_store::verification`, an append-only
history per obligation; reimplementing a step reopens the `verified` ones,
and a Fix or Implement turn on a `verifying` node reopens every verified step
and verdict, so `tod_core::lifecycle_next` recommends Verify, not the gate).
It loops until every own obligation and every plan step is `verified` or
`failed`, every failed obligation has a `failed` step to carry it back to
implementation, and a test run is recorded; it replaces the old `verifying`
on-entry turn. The `verifying` → `review` gate app-checks both
(`obligations-verified`, `plan-steps-verified`). The `learn` state agent is
given the node's work history (`node_context::render_work_history`: failed
verdicts and steps, review findings, failed gate criteria, conversation
counts), since the final state alone reads as a clean run. `review.rs` is the code review
for a node in `review` (the lifecycle panel's Review): the agent records each
finding through `tod-cli review` (`tod_store::review`, on the node, not the
conversation) and loops until it records `review done`; it replaces the old
`review` on-entry turn, and the side pane lists the findings, each answered
from its status badge. `fix.rs` resolves them (the conversation view's Fix,
beside Review): given the open findings, the agent answers each `fixed` or
`rejected` with a note — `tod-cli` refuses it the user's answers — and loops
until none is open and a test run is green. The `review` → `approved` gate is app-checked
(`tod_core::gate::derived`): review recorded done, no finding still open. `tod_ui::conversation::side_pane` picks the pane, and the picker offers a
"New …" entry per kind the focus can start. Adding a kind means a
`ProtocolKind` variant, an impl, a registry arm, and a side pane. Spec:
`doc/conversation/protocols.md`.

`tod_core::conversation` runs it: `driver.rs` (`ConversationDriver`: send, tick,
resume, rotation), `context.rs` (opening message, per-turn delta, rotation
snapshot, the `Focus` block's loader), and `mock.rs`, which plays the agent for
`--agent mock` with one directive per line (`add obligation <slug>: <text>`,
`add plan …`, `add node …`, `rename <id>: …`, `delete <id>`,
`move <id> under <slug>`, `flag <id>: <reason>`, `ask <text>`, and
`think <text>`, which adds a thinking step to the reply). The agent reads
its change set with `tod-cli changeset`. Drafting, which this replaced, is gone
(schema v37); the legacy `Role::Drafter` / `SessionPurpose::Drafter` variants
stay only for the interview.

### `tod-store::fleet` — agent/worktree orchestration

Tracks agents running against git worktrees: provisioning (`provision.rs`), launching (`launch.rs`, `runtime.rs`), reattaching to running processes (`reattach.rs`), terminal sessions (`terminal/`), prompt queuing (`prompt_queue.rs`), and an undo log (`undo.rs`). `store.rs` / `writer.rs` / `schema.rs` / `migration.rs` are the SQLite persistence core; `projection.rs` derives read-side views for the UI.

### Dev containers

The Files capability can run a node's work in a running dev container
instead of on this machine (`node_files.dev_container` / `container` /
`container_repo_on_host`, schema v57; `container_dir` is unused; the task editor's
"Runs in" section, which lists `docker ps` in the background, or `tod-cli
capabilities set <node> files --container <name|id> [--mounted on|off]`).
Nothing starts or builds a container; tod only uses a running one.

- **Repository in the container** (the default): the workspace directory is a
  container path, and git runs there too. Git worktrees go to
  `<repo>/.worktrees/<branch>`, which is added to the repository's
  `info/exclude`. Treehouse runs there too: the `treehouse` on the
  container's `PATH` (else a login shell's), with its own configuration and
  `TREEHOUSE_NO_UPDATE_CHECK=1`; none of tod's Treehouse settings apply.
- **Mounted** (`container_repo_on_host`): the repository is on this machine,
  git runs here, and only launches go into the container. The directory there
  is mapped through the container's mounts.

- `tod_store::fleet::Workdir` (`Host(path)` / `Container { container, path }`)
  is the directory type for every git and worktree operation and every launch
  cwd (`FilesDirectory::Ready`, `resolve_launch_cwd`, `Protocol::cwd`). Its
  `git`/`output` run on the host or through `docker exec`
  (`tod_agent::devcontainer::ContainerExec`, which caches `docker inspect` for
  30s). A container directory is never checked from the UI thread.

- `tod_agent::devcontainer` is the transport: `docker ps`/`inspect`, mount
  mapping, `prepare` (check the container is running, resolve directory, user
  from `remoteUser`, and `PATH`; write files as root), and `docker exec`
  commands. `AgentEnvironment` on `SessionTurn` / `start_fleet_agent` tells
  the provider where to spawn the agent; Docker only runs on the provider's
  worker thread.
- `tod_store::fleet::dev_container::launch_for` / `FleetStore::agent_environment`
  decide the environment for a node and a `Workdir`: a container directory
  always launches in its container; a host one only when it is inside a
  mounted repository's ready Files directory (a turn that runs in the data
  root stays on this machine). The agent process itself is started from the
  data root on the host. Every launch site that has a node passes it: the
  conversation driver, the background run, and terminals.
- Agents in the container reach `tod-cli` through `fleet::cli_relay`: a bash
  shim at `/tmp/tod-cli-relay/tod-cli` sends its args, `TOD_*` env, and stdin
  over TCP (`host.docker.internal`) to a loopback listener in the app, which
  checks a per-process token and runs the real `tod-cli` with the app's own
  data root. This needs Docker Desktop (native Linux Docker does not forward
  `host.docker.internal` to the host's loopback).
- Shells and terminal agents open a host terminal whose startup command is
  `docker exec -it [-u user] <id> sh /tmp/tod-cli-relay/launch-<id>.sh`; the
  script sets the directory, `PATH`, and relay env, so the token never goes on
  a command line.
- Tests that need a real container are skipped unless `TOD_TEST_DEV_CONTAINER`
  (a running container with git; some also need
  `TOD_TEST_DEV_CONTAINER_HOST_DIR` or `TOD_TEST_TOD_CLI`) is set; the
  Treehouse one needs `TOD_TEST_DEV_CONTAINER_TREEHOUSE` (a container with
  `treehouse` on its `PATH`).

### `tod-store::outline` — task tree

A hierarchical task/outline model with its own DDL/migration path (`ddl.rs`, `migrate_interview.rs`), slug-based addressing (`slug.rs`), and import from the older interview-session format (`import.rs`).

### `crates/tod-core::interview` — conversational task creation

The interview flow turns a conversation with two agents (question maker, answer processor) into obligations. All interview data — questions (queue and history), agent memory, a trigger-fed change log, and agent sessions — lives in the database (`tod_store::interview`, schema v15); every write is an `InterviewCommand` run on the fleet writer and attributed to an actor (the user, or an agent session via `TOD_INTERVIEW_ACTOR`). `driver.rs` decides when each agent takes a turn and which session it goes to (reuse, resume, or rotate to a fresh snapshot); `context.rs` builds the snapshot a session gets once and the per-turn delta of changes it did not make itself; `client.rs` is how `tod-cli` and the mock agents reach the data (mutation socket when the app is running, direct store otherwise); `mock.rs` plays both agents for `--agent mock`; `routing.rs` decides completion (`interview_work_remains` gates whether the task list can proceed). `db.rs` is the interview-session store. Spec: `doc/new-reqs/interview-protocol.md`.

### `crates/tod-ui::views` and `ui`

GPUI views live under `views/` (task list, obligations, agent panels, transcripts, command history) and share input/focus primitives from `ui/`. See the keyboard-focus convention below — it applies to any new editable view.

### UI styling

[doc/ui-style-guide.yaml](../doc/ui-style-guide.yaml) is the source of truth for how the UI looks. Implement each style once, named after its guide entry; views use those implementations, never raw colors, sizes, or spacing.

### Lists

Every list-shaped view (obligations, plan steps, findings, the change set, the conversation side pane, command history) is converging on one item-list component, which owns the cursor, selection, navigation keys, the row menu, drag reordering, and edit mode. It groups to any depth but its *content rows never nest* — that, not grouping depth, is what separates it from the node tree, where nodes own nodes. The node tree and the transcript are out of scope. The same item affords the same actions wherever it is shown. Spec, including the keyboard-focus rules and the migration order: [doc/ui/item-list.md](../doc/ui/item-list.md).

### GPUI keyboard focus: navigation mode vs. edit mode

Multi-line and single-line text fields must not trap keyboard navigation. The established pattern (see `crates/tod-ui/src/interview/views/workspace.rs` for the reference implementation):

- **Navigation mode (default)**: parent view owns focus; arrow keys move a highlight among stops (buttons, fields, rows); text inputs are stops but stay **disabled** until edit mode; Enter or a click enters edit mode; Tab does *not* move between stops.
- **Edit mode**: a `*_editing` flag re-enables the `Input` and focuses it next frame; Escape exits edit mode and restores the nav highlight; for single-line fields Enter also exits/commits (bind via `key_context::including_input`); multi-line fields keep Enter for newlines and use Ctrl+Enter to submit.
- Disabled inputs must be removed from the GPUI tab order via `set_input_tab_stop` (see `crates/tod-ui/src/ui/key_context.rs`), or Tab will focus them with a cursor while typing is silently blocked.
- Use `key_context::excluding_input` / `NOT_INPUT` for surface-level shortcuts and `key_context::including_input` for handlers (Escape, Enter-to-commit) that must still fire while an `Input` has focus.

### Dynamic text must be selectable

Any text whose content is data rather than fixed UI chrome (error messages, status strings, agent replies, transcripts, query results, criteria/detail rows, etc.) must be rendered with `crate::ui::selectable_text::selectable_text` (or `selectable_markdown` for markdown) instead of a plain `div().child(string)`, so the user can drag-select and copy it everywhere in the app. Static chrome — button labels, section headings, fixed captions — may stay plain `div` text. See `crates/tod-ui/src/ui/selectable_text.rs` and its existing call sites (e.g. `agent_transcripts.rs`, `lifecycle_panel.rs`) for the pattern.

### Cross-panel keyboard navigation

Any multi-column view moves the focused panel with Left/Right. Where those keys already act on a panel's own content (the task tree collapses/expands and selects the parent with them), the panel binds **Ctrl+Left / Ctrl+Right** instead. `crates/tod-ui/src/ui/pane_nav.rs` owns the shared `PaneFocusLeft` / `PaneFocusRight` actions: `bind_pane_nav(cx, surface)` registers plain *and* Ctrl arrows, `bind_modified_pane_nav(cx, surface)` registers Ctrl only. Ctrl+arrows are registered on every multi-column surface, so the same chord crosses panels everywhere.

Drawer panels in the Tasks view do not move focus themselves — they emit a `FocusTaskList` event and the shell (`crates/tod-ui/src/app/window.rs`) routes it, mirroring how `Close` is handled.

### Feature flags

`agent-socket` (default-on) compiles the TCP UI-automation control socket into dev/CI builds; release builds should use `--no-default-features` so that code isn't present in the shipped binary at all (not just disabled at runtime).
