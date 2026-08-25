# tod

Agent fleet management desktop application.

## Prerequisites

- **Rust:** stable toolchain (latest stable at clone time). Edition **2024**.
- **Windows:** GPUI requires a GPU and runtime support for the desktop window (typical Windows dev machine).
- **Other OS:** macOS and Linux are project targets but not verified for this scaffolding task.

## Build and run

From the repository root:

```bash
cargo run
```

This builds the `tod` binary (`crates/tod`) and opens a desktop application window.

### Agent control socket (optional)

For automated UI dogfood without stealing OS focus, launch with a fixed size and TCP control socket:

```bash
# 1) Once: create isolated sandbox tree + VERIFY FIXTURE
python .local/agent/ui-smoke/setup_e2e_sandbox.py

# 2) Start the app (leave it running — socket must be up before step 3)
cargo run -p tod -- --width 1280 --height 768 \
  --data-root .local/agent/ui-smoke/e2e-sandbox \
  --agent mock \
  --no-focus \
  --agent-socket 127.0.0.1:9876

# 3) In another terminal: drive UI (mock = no screenshots, fast)
python .local/agent/ui-smoke/batch_verify.py --mode mock
# Optional vision pack:  python .local/agent/ui-smoke/batch_verify.py --mode agent
```

| Flag | Effect |
|------|--------|
| `--data-root PATH` | All durable paths (`tod.db`, `tod.yml`, interview scratch) resolve under PATH |
| `--agent mock` | In-process mock provider (instant; for almost all UI tests) |
| `--agent cursor` | Real Cursor Agent CLI over ACP (rare protocol smoke only; still use `--data-root`) |
| `--no-focus` | Open without stealing OS keyboard focus (recommended for e2e while you work) |
| `--agent-socket HOST:PORT` | Enable control socket (off by default) |

Line-oriented protocol (one command per line; reply `ok` or `err …`):

| Command | Effect |
|---------|--------|
| `key <keystroke>` | GPUI `dispatch_keystroke` (e.g. `down`, `a`, `ctrl-1`); UI drain wakes immediately |
| `text <string>` | Insert into the focused GPUI input (use after `click` + `sync` on the field) |
| `click <x> <y>` | Left click at logical client coords (`SendMessage` to the app HWND; no focus steal, waits for handling) |
| `sync` | Wait one UI frame (use before a `shot` that must see post-input paint) |
| `shot <path>` | PNG of the window (scaled to `--width`×`--height`; crop-first, fast PNG encode) |
| `shot <path> <x0> <y0> <x1> <y1>` | Same, cropped in logical coords |

**Speed model:** treat local UI as synchronous (no decorative sleeps). Wait only on real async (queue poll, HTTP, files). Prefer mock agent + sandbox; optional one-shot ACP smoke — see `.local/agent/ui-smoke/BATCH.md`.

Example (Python):

```python
from pathlib import Path
import sys
sys.path.insert(0, str(Path(".local/agent/ui-smoke")))
from tod_client import Tod

with Tod() as t:
    t.key("down")
    t.sync()
    t.shot(".local/agent/ui-smoke/agent-shot.png")
```

Coords match the CLI window size. The socket is off unless `--agent-socket` is passed.

## Project layout

```text
Cargo.toml          # workspace root
crates/tod/         # desktop application binary
  src/main.rs       # CLI + App::run()
  src/app/          # GPUI application startup / window
  src/agent_socket/ # optional key/click/shot control socket
```

## Stack

- [GPUI](https://crates.io/crates/gpui) — GPU-accelerated UI framework
- [gpui-component](https://crates.io/crates/gpui-component) — UI components (Root, TitleBar)

Dependencies are pulled from **crates.io** at the latest published versions when added.
