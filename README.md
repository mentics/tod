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
cargo run -p tod -- --width 1280 --height 768 --agent-socket 127.0.0.1:9876
```

Line-oriented protocol (one command per line; reply `ok` or `err …`):

| Command | Effect |
|---------|--------|
| `key <keystroke>` | GPUI `dispatch_keystroke` (e.g. `down`, `a`, `ctrl-1`) |
| `click <x> <y>` | Left click at logical client coords (`PostMessage` to the app HWND; no focus steal) |
| `shot <path>` | PNG of the window (scaled to `--width`×`--height`) |
| `shot <path> <x0> <y0> <x1> <y1>` | Same, cropped in logical coords |

Example (Python):

```python
import socket
s = socket.create_connection(("127.0.0.1", 9876))
s.sendall(b"shot .local/agent/ui-smoke/agent-shot.png\n")
print(s.recv(256))
s.sendall(b"key down\n")
print(s.recv(256))
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
