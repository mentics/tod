# Design — ui-scaffolding

Task: `doc/process/projects/tod/tasks/ui-scaffolding/`

## Workspace layout

**Cargo workspace** at repo root with binary crate **`tod`** at `crates/tod/`. Workspace membership leaves room for additional crates under `crates/` as the project grows.

## Application entry (`App::run`)

Dedicated **`app` module** owns GPUI startup, window open, and root view. `main.rs` is thin: delegate to `App::run()`.

## Window chrome

**Minimal gpui-component chrome:** use gpui-component window/frame primitives (`Root`, `TitleBar`) with **empty content** — no placeholder routes, lists, or navigation surfaces.

## Dependency policy

- **`gpui`** and **`gpui-component`** from **crates.io**.
- Use the **latest published versions** at dependency-add time; do not exact-pin semver unless reproducibility requires it later.

## Window defaults

1. Title — **`tod`**
2. Initial size — **1024×768**, resizable
3. Quit — window **close button** ends the process; no custom menu or shortcut in this task

## Verification checks

On the developer OS (Windows for initial verification), this scaffold is done when:

1. `cargo run` from repo root exits **0** and shows a visible window within **~10s**
2. Window title bar shows **`tod`**
3. Closing the window exits the process cleanly (no hang)

**Out of scope for this task:** `cargo build` with `-D warnings`.

## Licensing constraint (Zed / GPL)

Do **not** copy from the Zed repository or reference GPL-licensed Zed application code. Avoid Zed-specific code paths due to GPL licensing concerns. Use **GPUI and gpui-component public documentation** and **non-GPL examples** only when implementing this scaffold.

## Lock now vs defer

### Lock now

1. **UI stack** — GPUI + gpui-component
2. **Dev entrypoint** — `cargo run` opens the desktop shell
3. **Dependency source** — crates.io, latest published versions (see [Dependency policy](#dependency-policy))
4. **Workspace layout** — Cargo workspace, `crates/tod` binary (see [Workspace layout](#workspace-layout))
5. **App module pattern** — `App::run()` (see [Application entry](#application-entry-apprun))
6. **Window chrome level** — minimal gpui-component frame with empty content (see [Window chrome](#window-chrome))

### Defer (later tasks)

1. Navigation / app shell layout
2. View routing
3. Keyboard bindings and operability
4. Theme and assets pipeline
5. Persistent app state layout
