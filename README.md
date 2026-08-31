# tod

Task and agent management desktop application.

## Prerequisites

- **Rust:** stable toolchain (latest stable at clone time). Edition **2024**.
- **Windows:** GPUI requires a GPU and runtime support for the desktop window (typical Windows dev machine).
- **Other OS:** macOS and Linux are project targets but not verified for this scaffolding task.

## Build and run

From the repository root:

```bash
cargo run
```

This builds the `tod` binary (`crates/tod`) with the **agent-socket** feature enabled (default) and opens a desktop application window.

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
| Process agent bundle | `assets/process/` | `{install_dir}/process/` |

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

Bundled docs are **not** required in `~/.cursor/skills` or `~/.claude/skills` for tod interviews. If you want the same process docs available when editing in Cursor manually:

```bash
# Windows (PowerShell, run as appropriate for your shell)
New-Item -ItemType SymbolicLink -Path "$env:USERPROFILE\.cursor\skills\process" -Target "C:\path\to\install\process"

# Unix
ln -s /path/to/install/process ~/.cursor/skills/process
```

Point the target at your **install** `process/` directory (or repo `assets/process/` during development).

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
| `--no-focus` | Open without stealing OS keyboard focus (recommended for e2e while you work) |
| `--agent-socket HOST:PORT` | Enable control socket on a specific address (dev builds only) |
| `--agent-socket-port PORT` | Shorthand for `127.0.0.1:PORT` — use a different port per parallel test instance |

Line-oriented protocol (one command per line; reply `ok` or `err …`):

If the chosen port is already in use, startup fails immediately with a bind error — pick another port and retry.

| Command | Effect |
|---------|--------|
| `key <keystroke>` | GPUI `dispatch_keystroke` (e.g. `down`, `a`, `ctrl-1`) |
| `text <string>` | Insert into the focused GPUI input |
| `click <x> <y>` | Left click at logical client coords |
| `sync` | Wait one UI frame |
| `shot <path>` | PNG screenshot of the window |

See `.local/agent/ui-smoke/BATCH.md` for the full smoke workflow.

## Project layout

```text
Cargo.toml              # workspace root
assets/process/         # bundled agent docs (source; copied to target/.../process at build)
crates/tod/             # desktop application binary
  src/main.rs           # CLI + App::run()
  src/cli.rs            # LaunchOptions / CLI parsing
  src/app/              # GPUI application startup / window
  src/agent_socket/     # optional control socket (agent-socket feature)
  src/process_bundle/   # install discovery, manifest, scope export, prompts
```

## Stack

- [GPUI](https://crates.io/crates/gpui) — GPU-accelerated UI framework
- [gpui-component](https://crates.io/crates/gpui-component) — UI components (Root, TitleBar)

Dependencies are pulled from **crates.io** at the latest published versions when added.
