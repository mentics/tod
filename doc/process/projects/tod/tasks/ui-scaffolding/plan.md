# UI scaffolding — plan

## Goal (from user.md)

Deliver a runnable local desktop application shell for tod — no navigation chrome or placeholder pages in this task.

## Steps

1. **Workspace skeleton** — Create root `Cargo.toml` as a workspace with `crates/tod` as a member. Add `crates/tod/Cargo.toml` and `crates/tod/src/main.rs` with a minimal `fn main() {}`. Confirm `cargo check` passes from repo root.
2. **Add GPUI dependencies** — In `crates/tod/Cargo.toml`, add `gpui` and `gpui-component` from crates.io using the latest published versions at dependency-add time (no exact semver pin unless a build failure requires it). Confirm `cargo check` still passes.
3. **App module — startup** — Add `crates/tod/src/app/mod.rs` with `App::run()` owning GPUI application startup (`Application::new().run(...)` or equivalent per public docs). Keep `main.rs` thin: delegate to `App::run()`.
4. **App module — window and root view** — Add `crates/tod/src/app/window.rs` with window open configuration and root view: title `tod`, initial size 1024×768, resizable; minimal gpui-component chrome (`Root`, `TitleBar`) with empty content — no placeholder routes, lists, or navigation surfaces. Wire from `App::run()`.
5. **README — standard dev setup** — Add root `README.md` with Rust toolchain prerequisites (stable Rust, latest at implementation time; edition 2024 if the toolchain supports it, else 2021; no MSRV pin and no `rust-toolchain.toml`), clone/build/run steps (`cargo run` from repo root), and the three manual verification checks below.
6. **Manual verification on Windows** — On a Windows dev machine, run all three verification checks (see [Verification](#verification)). No CI in this task.

## Constructions (must match design / user constraints)

| Concern | Construction |
|--|--|
| Workspace layout | Cargo workspace at repo root; binary crate `tod` at `crates/tod/`; membership leaves room for additional crates under `crates/` |
| Application entry | Thin `main.rs` delegates to `App::run()` in dedicated `app` module |
| Module layout | `app/mod.rs` — `App::run()` (GPUI startup); `app/window.rs` — window config + root view |
| UI stack | `gpui` and `gpui-component` from crates.io; latest published versions at dependency-add time |
| Window chrome | Minimal gpui-component frame (`Root`, `TitleBar`) with empty content — no placeholder UI surfaces |
| Window defaults | Title `tod`; initial size 1024×768, resizable; close button ends the process (no custom menu or quit shortcut) |
| Dependency pinning | No exact semver pin unless reproducibility requires it later |
| Toolchain | Stable Rust (latest stable at implementation time); edition 2024 if supported, else 2021; no MSRV pin |
| Licensing | Do not copy from Zed repository or reference GPL-licensed Zed application code; use public GPUI/gpui-component docs and non-GPL examples only |
| CI | Deferred entirely — no GitHub Actions or other CI in this task |
| Dev preview | `cargo run` only; no packaged installer, code signing, or release artifact |

## Requirement traceability

| user.md requirement | Design / plan element | Implementation (to fill) | Check (statement or success criteria) |
|--|--|--|--|
| Runnable desktop shell — `cargo run` opens a visible desktop application window | Workspace + GPUI deps + `App::run()` + window/root view (Steps 1–4); Constructions: workspace layout, app entry, window chrome | `crates/tod/` — workspace, `app/mod.rs`, `app/window.rs`, GPUI deps in `Cargo.toml` | Manual: `cargo run` from repo root exits 0 and shows a visible window within ~10s (Windows) |
| No placeholder UI surfaces — no app shell navigation, task/agent lists, detail pages, status area, notifications queue, or settings as routes or views | Window chrome: `Root` + `TitleBar` with empty content only (Step 4) | `app/window.rs` — `Shell` view with TitleBar + empty flex area | Manual: window shows frame chrome only; no navigation, lists, or placeholder pages |
| Out of scope — agent launch/runtime, external integrations, fuzzy search, fleet persistence/JSON import, credential UI | Not implemented; plan steps omit these entirely | No stubs in codebase | N/A — not stubbed or present in codebase |
| Constraint: UI stack — GPUI and gpui-component | Step 2 dependencies; Constructions: UI stack | `Cargo.toml` — `gpui`, `gpui-component`, `gpui-component-assets` | `cargo check` / `cargo run` compile and link GPUI stack |
| Constraint: Dev preview only — no packaged installable build | Constructions: dev preview; no installer steps in plan | N/A | Manual: app runs via `cargo run` only |
| Constraint: Cross-platform — verification on one development OS sufficient | Manual verification on Windows only (Step 6); Assumption 4 | N/A | Windows manual checks pass; other OSes not verified in this task |
| Constraint: Keyboard operability deferred | Not in plan steps | N/A | N/A — deferred to later task |

## Assumptions

1. Developer has stable Rust and a Windows dev environment where GPUI can run (GPU/runtime prerequisites met locally).
2. `gpui` and `gpui-component` from crates.io at latest published versions at dependency-add time are acceptable (no exact semver pin unless build fails).
3. No Zed repository or GPL-licensed Zed application code is copied; only public GPUI/gpui-component docs and non-GPL examples are used.
4. Cross-platform correctness is not verified in this task — Windows proof is sufficient per user.md.
5. No packaged installer, code signing, or release artifact in this task (dev preview via `cargo run` only).

## Verification

Manual verification only on Windows. Run from repo root after Steps 1–5 are complete.

| # | Check | How to run |
|--|--|--|
| 1 | `cargo run` exits **0** and shows a visible window within **~10s** | Run `cargo run` from repo root on Windows; confirm process exit code 0 and window appears promptly |
| 2 | Window title bar shows **`tod`** | Visually inspect title bar after window opens |
| 3 | Closing the window exits the process cleanly (no hang) | Close via window close button; confirm process terminates without hanging |

**Out of scope for this task:** `cargo build` with `-D warnings`.

Document these checks in root `README.md` (Step 5).
