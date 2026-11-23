# tod

Task and agent management desktop application.

The [redesign specifications](doc/redesign/README.md) track proposed product behavior. The [standalone ZGX frontend](frontends/tod-zgx/README.md) exercises the command palette and collection panels in one app.

## Prerequisites

- **Rust:** stable toolchain (latest stable at clone time). Edition **2024**.
- **Windows:** GPUI requires a GPU and runtime support for the desktop window (typical Windows dev machine).
- **Other OS:** macOS and Linux are project targets but not verified for this scaffolding task.

## Build and run

From the repository root:

```bash
cargo run
```

This builds the `tod` binary (`crates/tod`) with the **agent-socket** feature enabled (default) and opens a desktop application window. Note that plain `cargo run` does **not** build `tod-cli`, so agents in that session will find it missing or stale.

### Dev loop (`tod` + `tod-cli` together)

Agents shell out to `tod-cli`, which `cargo run -p tod` alone does not build. Use `scripts/run.sh` (or `scripts/dev.ps1` on Windows) instead, which brings every binary up to date (`scripts/build.sh`) before launching `tod` so agent chats always see a current CLI:

```bash
scripts/run.sh --data-root .local/test/my-sandbox --agent mock --no-focus
```

Any arguments are passed straight through to `tod`. Both binaries land in the same `target/{debug,release}/` directory, so `tod-cli` is already a sibling of `tod` once built — no copy step is needed in dev.

### Cross-platform compilation

CI runs `cargo check --workspace --all-targets` on **Ubuntu, Windows, and macOS** for every push and pull request (see `.github/workflows/ci.yml`). Platform-specific code behind `#[cfg(target_os = "...")]` is only type-checked on the matching OS, so a change that compiles on one machine can still fail on another until CI (or a local build on that OS) runs.

When editing `cfg`-gated paths, prefer borrowing shared values (e.g. `PathBuf::from(&home)`) instead of moving them into the first use if a later branch may need the same value.

### Release build (no agent control socket)

Release binaries exclude the TCP control socket used for UI automation — that code is not compiled in at all:

```bash
cargo build --release -p tod --no-default-features
```

Use this for distributable installs. Dev builds keep `agent-socket` on by default.

## Installation and distribution

### What gets installed

| Component | Repo source | Installed layout |
|-----------|-------------|------------------|
| Application binary | `target/release/tod.exe` (or `tod`) | `{install_dir}/tod.exe` |
| CLI binary | `target/release/tod-cli.exe` (or `tod-cli`) | `{install_dir}/tod-cli.exe` |
| Process agent bundle | `assets/process/` | `{install_dir}/process/` |

**`tod-cli`** must be built and staged as a sibling of `tod` — agents shell out to it by that convention (see `crates/tod/media/context/app.md`). In a dev checkout the two binaries already land in the same `target/{debug,release}/` directory as a side effect of Cargo's shared workspace target dir, so no copy step is needed there; only a packaged install needs `tod-cli` copied in explicitly. Use `scripts/release.sh` / `scripts/release.ps1` to build both release binaries together.

**`assets/process/`** is the version-controlled source for agent behavior docs (SKILL, agents, manifest). At build time, `build.rs` copies it to `target/{debug,release}/process/` next to the binary so local runs match an install.

**At runtime**, the app resolves bundled docs via `TodInstallPaths`:

1. `TOD_PROCESS_ROOT` environment variable (override for testing)
2. `{executable_dir}/process/` — **installed layout** (binary and `process/` side by side)
3. Walk up from cwd for `assets/process/README.md` — **dev checkout fallback**

User data (obligations, interviews, scratchpads) lives under a **data root** — not in `assets/process/`.

### Data locations

On first launch, tod prompts for a **data root** (defaulting to the OS app data directory) and remembers it in `install.toml` under the platform config dir:

| OS | Bootstrap file |
|----|----------------|
| **macOS** | `~/Library/Application Support/tod/install.toml` |
| **Windows** | `%APPDATA%\tod\install.toml` |
| **Linux** | `~/.config/tod/install.toml` |

Resolution order: `--data-root` → `TOD_DATA_ROOT` → `install.toml`. If none are set, the first-run picker appears.

Set `TOD_CONFIG_DIR` to relocate the directory holding `install.toml` (tests use this to avoid touching the real file).

Everything lives flat under the chosen data root: `tod.db`, `tod.yml`, `task-list-working-set.json`, logs in `log/`.

**Dogfooding (daily use in this repo):**

```bash
export TOD_DATA_ROOT=.local/data   # add to shell profile for persistence
cargo run -p tod
```

**Fresh test sandbox:**

```bash
rm -rf .local/test/my-sandbox
cargo run -p tod -- --data-root .local/test/my-sandbox --agent mock --no-focus
```

`--data-root` overrides `TOD_DATA_ROOT` and `install.toml`, so tests stay isolated even when dogfooding env is set.

### Manual install (test or production)

```bash
# Release binary without agent-socket
cargo build --release -p tod --no-default-features

# Stage an install directory (example)
INSTALL=/tmp/tod-install
mkdir -p "$INSTALL"
cp target/release/tod.exe "$INSTALL/"    # or target/release/tod on Unix
cp -r target/release/process "$INSTALL/" # copied from assets/process at build time
```

Run from anywhere:

```bash
"$INSTALL/tod.exe" --verify-process-bundle
"$INSTALL/tod.exe" --data-root /path/to/your/data
```

`--verify-process-bundle` discovers bundled docs and validates agent paths without opening the UI — useful after a clean install.

### Optional: Cursor / Claude skill symlink

Bundled docs are **not** required in `~/.cursor/skills` or `~/.claude/skills` for tod agents. If you want the same process docs available when editing in Cursor manually:

```bash
# Windows (PowerShell, run as appropriate for your shell)
New-Item -ItemType SymbolicLink -Path "$env:USERPROFILE\.cursor\skills\process" -Target "C:\path\to\install\process"

# Unix
ln -s /path/to/install/process ~/.cursor/skills/process
```

Point the target at your **install** `process/` directory (or repo `assets/process/` during development).

## Navigation and shortcuts

- **`** opens the app nav (Settings, Database, Workbench, Pull requests);
  **Ctrl+1** goes to the Workbench, **Ctrl+,** to Settings.
- In multi-column views, **Left/Right** (or **Ctrl+Left/Right** where the panel
  uses plain arrows itself, like the task tree) move between panels.
- **Ctrl+J**, in the Workbench, toggles the chat drawer about the selected node.

Reversal never uses Ctrl+Z; it applies inverses from the conversation's own log,
and asks for confirmation when an item changed since or other changes depend on
it.

### Run TOD with Codex

Choose **OpenAI** under **Settings → Default Agent** for lifecycle work or
under **Chat Agent** for chat. The `auto` model uses the adapter's default.
TOD installs Codex ACP and the Codex CLI on demand on the desktop, in dev
containers, and in cloud sandboxes. It checks the managed install for updates
when opening a new session, at most once a day after a successful check.
The environment needs Node.js and npm. `TOD_CODEX_ACP_BIN` selects a custom
adapter executable for host runs.

Set a task's preference under its **Agent** capability, or choose a provider
for a specific step under **Lifecycle Config**. When Claude or Codex reports
that its usage limit has been reached, TOD continues the turn in a fresh
session on the other provider. The conversation and working files carry over.
TOD stores quota reset times in `provider-quota.json` under its data root and
skips an exhausted provider until its reset. Without an exact reset, TOD
checks hourly, or daily for Claude's weekly ACP usage window. If both
providers are exhausted, the runner waits until the earlier retry time.

Claude-to-Codex fallback looks up a model and effort by capability tier. For
example, Haiku at low effort maps to GPT-5.6 Luna at low effort, Sonnet at
high effort maps to GPT-5.6 Sol at medium effort, and Opus at high effort maps
to GPT-6 Astra at high effort. The destination's adapter must support the
selected model and effort. Both providers must be signed in to switch.

A node already parked on a quota wait from an older build can be resumed by
finding the wait with `tod-cli wait list --node <NODE>` and cancelling it with
`tod-cli wait cancel <WAIT_ID>` on the same data root.

Sign in to Codex as the user running the agent in each environment. In a dev
container or cloud sandbox, open its shell and run `codex login --device-auth`.
Use TOD's managed CLI if `codex` is absent from PATH:

```sh
"${XDG_DATA_HOME:-$HOME/.local/share}/tod/adapters/codex-acp/node_modules/.bin/codex" login --device-auth
```

Each sandbox has its own login and quota file. Build the Linux supervisor with
`scripts/build-sandbox-binaries.sh` for cloud use.

Gemini remains available for local chat through Gemini CLI.

## Agent control socket (dev / CI only)

Requires a build **with** the default features (`agent-socket`). Not present in `--no-default-features` release builds.

For automated UI dogfood without stealing OS focus, launch with a fixed size and TCP control socket:

```bash
# 1) Once: create isolated sandbox tree + VERIFY FIXTURE
python .local/agent/ui-smoke/setup_e2e_sandbox.py

# 2) Start the app (leave it running — socket must be up before step 3)
cargo run -p tod -- --width 1280 --height 768 \
  --data-root .local/test/e2e-sandbox \
  --agent mock \
  --no-focus \
  --agent-socket-port 9876

# Multiple parallel test instances: give each its own port (9876, 9877, …).

# 3) In another terminal: drive UI (mock = no screenshots, fast)
python .local/agent/ui-smoke/batch_verify.py --mode mock
```

| Flag | Effect |
|------|--------|
| `--data-root PATH` | Test sandbox: all durable state at PATH (`tod.db`, `tod.yml`, …); overrides `TOD_DATA_ROOT` |

**Fresh empty state:** `--data-root` is persistent — reusing the same path reloads existing lists. For a truly empty start, use a new path under `.local/test/` or delete the sandbox first:

```bash
rm -rf .local/test/my-sandbox
cargo run -p tod -- --data-root .local/test/my-sandbox --agent mock --no-focus
```
| `--agent mock` | In-process mock provider (instant; for almost all UI tests) |
| `--agent cursor` | Real Cursor Agent CLI over ACP (rare protocol smoke only; still use `--data-root`) |
| `--no-focus` | No window (main or secondary) takes OS keyboard focus; implied by `--agent-socket`/`--agent-socket-port` |
| `--agent-socket HOST:PORT` | Enable control socket on a specific address (dev builds only) |
| `--driven-idle-ms MS` | How long the user may sit idle in user mode before the agent gets the app back (default 5000) |
| `--agent-socket-port PORT` | Shorthand for `127.0.0.1:PORT` — use a different port per parallel test instance |

Line-oriented protocol (one command per line; reply `ok` or `err …`):

If the chosen port is already in use, startup fails immediately with a bind error — pick another port and retry.

| Command | Effect |
|---------|--------|
| `key <keystroke>` | GPUI `dispatch_keystroke` (e.g. `down`, `a`, `ctrl-1`) |
| `text <string>` | Insert into the focused GPUI input |
| `click <x> <y>` | Left click at logical client coords |
| `scroll <x> <y> <dx> <dy>` | Wheel/trackpad scroll at logical coords with pixel deltas. Convention: positive `dy` scrolls down (content moves up), positive `dx` scrolls right; negative values go the other way. |
| `rclick <x> <y>` | Right click at the same coords (opens a list row's context menu) |
| `drag <x1> <y1> <x2> <y2>` | Press at the first coords, move to the second, release (drags a list row to a new place) |
| `press` / `moveto` / `release` `<x> <y>` | The same gesture one step at a time, so a `shot` can catch what only exists mid-drag (a list's landing line) |
| `sync` | Wait one UI frame |
| `mode` | Reply `ok driven`, `ok user` or `ok idle`: who has the app (see below). Runs at once in any mode |
| `size` | Reply `ok W H`: the window's content size in logical pixels. Runs at once in any mode |
| `resize W H` | Set the window's content size. While driven the app owns that size (see below) |
| `test-user move X Y \| click X Y \| key K \| resize W H \| leave \| blur \| focus \| peek` | **Test-only seam, not for agents.** Plays an event of the *user's* through the same path real input takes (no synthetic guard; `resize` is not marked as the app's own), so the mode machine sees the user. It is not agent input: it never waits and does not keep the agent "driving". Needs no OS permission and never moves the real cursor. `move` needs two moves 2px or more apart to count (the first only anchors the pointer). `blur`/`focus` call what the window-activation observer calls (the observer itself cannot be triggered from here). `peek` replies the mode, window size, view state and the text of the focused input, whoever has the app. |
| `close` | Ask the window to close as the user would: the close guard runs (replies `ok blocked` while its toast is up). `click` also works on macOS. |
| `quit` | Close every window, so the app quits as if the user closed it |
| `shot <path>` | PNG screenshot of the window (Windows and macOS; on macOS the app reads its own window back from the window server, so no Screen Recording permission, and it works while the window is covered or unfocused) |
| `shot2 <path> [x0 y0 x1 y1]` | Like `shot`, but forces a fresh draw and renders the scene offscreen through gpui (`Window::render_to_image`, so it needs no window-server readback and is current while the window is covered or minimized). macOS-tested; dev builds only, since it turns on gpui's `test-support` |
| `tree [--role R] [--label S] [--within SEL] [--json]` | Force a fresh frame, then list the UI by identity: `ok N nodes` followed by N lines of `selector`, role, label, value, bounds (window coords), actions, `focused`/`disabled`. Every visible string is in it: text a button, list item or heading shows is that node's label, any other text is a `label` node (`label:"Which database should the cache use?"`), and a button's label does not include text of buttons inside it. Reads GPUI's accessibility tree (see `vendor/README.md`), so it needs no coordinates. |
| `act <selector> [click\|focus\|set-value <text>\|scroll-into-view]` | Do something to the one element a selector matches (default `click`). Goes through the accessibility action path; if the element has no handler for it, falls back to a synthetic click at its centre and says so. `set-value` also focuses the field, so a following `key enter` or `text` goes to it (the reply says `(focused)`). `err no-match <selector>` or `err ambiguous <n> matches: …` otherwise. Counts as input, like `click`. |
| `wait-for <selector> [--gone] [--timeout-ms N]` | Wait until the selector matches (or, with `--gone`, stops matching); `ok N matches` or `err timeout …`. Default timeout 5000 ms. |
| `coverage [frames N]` | Clickable elements this frame versus how many the tree exposes (`ok C clickable E exposed X explicit M missing`, then M lines naming each missing one). With `frames N`, first times N forced draws with the tree off and on. |
| `agent-platform get` / `cycle` / `set <platform>` | Read or change the default platform in Settings. Accepts `claude`, `cursor`, `openai`, or `google`. OpenAI's saved ID is `open_ai`. |

Selectors: `#<id-path-suffix>` (the tail of an element's id path, on a `/` boundary), `<role>:<label>` (case-insensitive substring of the label or value, e.g. `button:Start`; `textinput` also matches a multi-line input), and `<sel> >> <sel>` to scope the right one to what is inside the left one (`#unified-task-panel >> button:Start`). An element with no id of its own gets `#~file.rs:LINE` (where it was built; `#1`, `#2` for repeats). Double quotes protect spaces, `>>` and action words, in `act`, `tree --within`/`--label`/`--role` and `wait-for`: `act "textinput:Item title" set-value Cov node`, `act button:"Run now" click`, `"#a >> button:Go now"` (inside quotes `\"` is a quote); unquoted forms work as before. `tree` lists a node whose id path is a number or has a long run of digits (`#input-4294967389`) by `role:label` instead. A request's free-text answer field is a text input named `Answer: <question>`: `act "textinput:Answer: What retention" set-value 30 days`, then `key ctrl-enter` submits it. Run `tree` to see what is there. The tree is switched on by the first of these commands and off again after 30 s without one; with the socket idle it costs nothing.

### Sharing the app with the user: driven mode and user mode

The socket drives the same instance the user may be using, so the app tells
the two apart. The socket marks its own injected events; any other input is
the user's (`agent_socket/driver.rs`).

- **driven**: a socket client is driving, from its first input command until no
  command for 2 minutes (then `idle`: no indicator, no snapshots). Only a
  command that acts on the app counts: input, `act`, `quit`/`close`, the transcripts
  and agent-platform commands. Observing does not: `mode`, `sync`, `size`, `shot`,
  `shot2`, `tree`, `wait-for` and `coverage` neither keep the agent driven nor
  start driving from `idle`, so a client that polls them between actions goes
  idle on schedule. The title bar is violet and says "Shepherd driving".
- **user**: the user pressed a key or a mouse button, scrolled, or moved the
  pointer. Focus gained, or the pointer merely over the window, does not count
  (swiping through the macOS Space the window is on must not interrupt driven
  work). A key or click that switches is consumed, so it never lands in a field
  the agent focused. The agent's view state is saved and the user's own last
  view restored; the reverse when driven mode resumes. Only view state (window
  size, active view, tree selection, columns, pins, focused column, chat drawer,
  the text of the input being edited) is saved and restored, never data; menus
  are closed on a switch. Scroll offsets are not captured.
- Back to **driven** when the window loses key focus or the pointer leaves it
  for 300 ms, or after `--driven-idle-ms` (default 5000) without interaction.

An input command (`key`, `text`, `click`, `rclick`, `scroll`, `drag`,
`press`/`moveto`/`release`, `resize`) that arrives in user mode **waits**. Once
driven mode resumes and the agent's view is restored: if the data it depended on
is unchanged (the selected node still exists, its lifecycle state, who is next,
its pending request ids, and each open panel still applying) it runs and replies
`ok`; otherwise it replies `err user-conflict: request 3f2a answered; ENG-2925
pr_draft→approved` (what changed) without running. After 60 s it replies `err
user-mode` and the agent resends. `shot`/`shot2` in user mode reply `err
user-mode: …` since the screen shows the user's view.
`tree`, `wait-for` and `coverage` read the screen the user may be changing, so
they wait in user mode exactly as input does (same conflict and 60 s rules), but
they are not agent activity (see driven, above).

**Window size.** While driven the app owns the content size: what the agent's
`resize` set, or what the last restore put back. A size change the app did not
make (the user dragging an edge, a tiling tool, a display change) is user
activity like a key: the mode goes to `user`, the user's size is theirs while
they have the app, and when driven mode resumes the agent's size is put back
before any waiting command runs. Before every input command the app also checks
the window is at its size and puts it back if not. If the screen is too small
for the size (an OS clamp), the command still runs and its reply carries ` # note:
window is WxH, the size WxH did not fit` on the first line.

See `.local/agent/ui-smoke/BATCH.md` for the full smoke workflow.

The conversation view has its own self-contained smoke test, which builds
`tod` and `tod-cli`, launches its own mock instance on a random port with a
fresh data root under `.local/agent/scratchpad/tod/`, and stops only that
process:

```bash
python .local/agent/ui-smoke/conversation.py   # --no-build to skip cargo build
```

## Downloading problem journeys

When a user submits a problem journey, it arrives as an encrypted bundle on the
relay inbox. `scripts/pull-journeys.sh` (or `scripts\pull-journeys.ps1` on
Windows) runs `tod-journeys pull`, which downloads each bundle, decrypts it, and
saves it as `<bundle-id>.journey` in the `received/` folder of the
`tod-journeys` home (under the OS data dir by default).

```bash
# One-time: create the receiving identity and print the relay code to paste
# into tod's settings
cargo run -p tod-journeys -- init

# Fetch whatever is waiting, then exit
scripts/pull-journeys.sh

# Keep listening for new bundles
scripts/pull-journeys.sh --watch
```

```powershell
scripts\pull-journeys.ps1          # add -Watch to keep listening
```

Read one with `cargo run -p tod-journeys -- show <file>` (add `--full` for
everything), or summarize a folder with `... stats <dir>`. Pass `--home DIR`
(`-JourneysHome DIR` in PowerShell) to use a different home. Format and
protocol: [doc/journeys/spec.md](doc/journeys/spec.md).

## Cloud orchestrator

The cloud orchestrator is a separate sandbox that runs its own copy of
`tod-orchestrator`, `tod-cli`, and `tod-relay`. It does **not** update when
tod does, so after changing any of those crates (or when tod shows "The cloud
orchestrator is out of date", or the log says `orchestrator ...: 404 Not
Found`) redeploy it:

```bash
scripts/deploy.sh   # deploys the orchestrator and the watchdog when they are out of date
```

This builds the Linux binaries (needs Docker running, or zig +
cargo-zigbuild), uploads them, and restarts the server. Its data is kept.
Extra arguments go to `tod-sandbox orchestrator` (e.g. `--move-data`). The
app deploys an orchestrator on its own only when none exists. Details:
[doc/cloud-sandboxes/orchestrator.md](doc/cloud-sandboxes/orchestrator.md).

## Project layout

```text
Cargo.toml              # workspace root
assets/process/         # bundled agent docs (source; copied to target/.../process at build)
crates/tod/             # thin launcher binary
  build.rs              # installs process/, media/ and skills/ next to the executable
  media/context/        # agent context docs (copied to target/.../media at build)
crates/tod-ui/          # all GPUI code
  src/cli.rs            # LaunchOptions / CLI parsing
  src/app/              # GPUI application startup / window
  src/views/            # task list, obligations, agent panels, transcripts
  src/conversation/     # conversation view (transcript, change set, context panel)
  src/agent_socket/     # optional control socket (agent-socket feature)
crates/tod-cli/         # `tod-cli` binary that agents shell out to
crates/tod-core/        # policy + orchestration shared by UI and CLI
  src/conversation/     # conversation driver, agent context, mock agent
  src/interview/        # shared client, paths, and legacy interview data
  src/process_bundle/   # install discovery, manifest, scope export, prompts
  src/media.rs          # agent context doc resolution
  src/agent_context.rs  # assembles an agent chat's first message
crates/tod-agent/       # agent transport (leaf crate; no tod-* deps)
crates/tod-store/       # persistence (SQLite, credentials, settings, paths)
  src/conversation/     # conversation log: turns, recorded actions, flags, reversal
```

Crates are layered so each depends only on the ones below it: `tod` → `tod-ui`
→ `tod-core` → `tod-agent`, with `tod-store` alongside. The full picture, with
every executable and the rules that hold across them, is
[doc/architecture.md](doc/architecture.md); `.claude/CLAUDE.md` has the
crate-by-crate detail.

## Stack

- [GPUI](https://crates.io/crates/gpui) — GPU-accelerated UI framework
- [gpui-component](https://crates.io/crates/gpui-component) — UI components (Root, TitleBar)

Dependencies are pulled from **crates.io** at the latest published versions when added.
