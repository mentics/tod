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

## Project layout

```text
Cargo.toml          # workspace root
crates/tod/         # desktop application binary
  src/main.rs       # thin entry — delegates to App::run()
  src/app/mod.rs    # GPUI application startup
  src/app/window.rs # window config and root view (TitleBar + empty content)
```

## Stack

- [GPUI](https://crates.io/crates/gpui) — GPU-accelerated UI framework
- [gpui-component](https://crates.io/crates/gpui-component) — UI components (Root, TitleBar)

Dependencies are pulled from **crates.io** at the latest published versions when added.
