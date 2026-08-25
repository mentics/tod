# Design interview — ui-scaffolding — 2026-08-23

## Session

**Agent:** Process agent bootstrapped design interview for task `ui-scaffolding`. Lifecycle state `design`, mode `interactive`.

**Prior session:** `task-requirements-interview-2026-08-23-0120.md` — requirements accepted; advance to design.

---

### q-001

**Question:** What Cargo workspace layout and runnable binary crate name should this greenfield repo use for the first GPUI slice?

**A)** Single package at repo root — binary crate named `tod`
**B)** Cargo workspace — binary crate `tod` under `crates/tod`, room for additional crates later
**C)** Different name or placement (describe)

**Recommend:** B unless you prefer zero workspace overhead for now.
Which do you want?

**Answer:** B — Cargo workspace; binary crate `tod` under `crates/tod`; room for additional crates later ("We will want more crates later")

---

### q-002

**Question:** How should the GPUI app entry point open the first window — what is the minimal root view pattern for this scaffold?

**A)** Inline in `main.rs` — `Application::new().run(...)` with a tiny anonymous `Render` impl
**B)** Dedicated `app` module — `main.rs` delegates to `App::run()` owning window open + root view
**C)** Follow a specific Zed file or module as the template (name the reference)

**Recommend:** B — keeps `main.rs` thin without premature architecture.
Which do you want?

**Answer:** B — Dedicated `app` module; `main.rs` delegates to `App::run()`

---

### q-003

**Question:** Task constraints require GPUI and gpui-component. With no placeholder UI surfaces, how much gpui-component should appear in the running window?

**A)** Dependency only — compile/link gpui-component but render a bare GPUI root (no component chrome visible)
**B)** Minimal chrome — use gpui-component for window/frame primitives (e.g. Root, TitleBar) with empty content
**C)** Plain GPUI window only — prove gpui-component integration in code, not visually in the window

**Recommend:** B — proves the Zed component stack in the actual window.
Which do you want?

**Answer:** B — Minimal chrome — gpui-component for window/frame primitives (Root, TitleBar) with empty content

---

### q-004

**Question:** How should `gpui` and `gpui-component` be sourced and pinned in `Cargo.toml` for this repo?

**A)** crates.io with exact semver pins
**B)** Git dependencies on `zed-industries/zed` (specific branch or rev)
**C)** Path dependencies to a local Zed checkout on the developer machine

**Recommend:** B unless crates.io publishes versions you already trust for this stack.
Which do you want?

**Answer:** A (modified) — crates.io dependencies for gpui and gpui-component; use latest published versions (not exact pins unless needed for reproducibility — "just get the latest versions")

---

### q-005

**Question:** For the scaffold window, what title, initial size, and quit behavior do you want?

Proposed defaults:
1. Title — `tod`
2. Initial size — 1024×768 (resizable)
3. Quit — window close button ends the process; no custom menu or shortcut yet

**A)** Accept all three defaults
**B)** Adjust — say which numbers change and how

**Recommend:** A for a dev-preview shell.
Which do you want?

**Answer:** A — Accept defaults: title `tod`, 1024×768 resizable, close button ends process

---

### q-006

**Question:** What verification checks must pass for this scaffold to count as done? (Your dev OS is Windows.)

Proposed checks:
1. `cargo run` from repo root exits 0 and shows a visible window within ~10s
2. Window title bar shows `tod`
3. Closing the window exits the process cleanly (no hang)
4. `cargo build` succeeds with warnings denied (`-D warnings`)

**A)** Require 1–3 only
**B)** Require all four
**C)** Adjust — name which checks to add, drop, or change

**Recommend:** A — aligned with task user.md success criteria.
Which do you want?

**Answer:** A — Require verification checks 1–3 only (NOT check 4 -D warnings)

---

### q-007

**Question:** When using Zed as a reference for this scaffold, how closely should we copy its structure now?

**A)** Minimal reference — GPUI/gpui-component docs and small snippets only; no Zed repo layout
**B)** Targeted copy — mirror dependency wiring and app-init patterns from Zed; defer theme, assets, and editor-specific crates
**C)** Broad copy — adopt Zed's workspace crate factoring for the desktop shell from the start

**Recommend:** B for a greenfield app that shares the stack but is not an editor fork.
Which do you want?

**Answer:** Custom — Avoid Zed-specific code due to GPL licensing concerns. Do NOT copy from Zed repo or reference GPL-licensed Zed code. Use GPUI/gpui-component public docs and non-GPL examples only. Record as design constraint.

---

### q-008

**Question:** Which design choices should this scaffolding task lock in now (hard to unwind later) vs leave explicitly open for later tasks?

Agent-proposed split:
1. **Lock now** — GPUI + gpui-component as the UI stack; dev-preview `cargo run` entrypoint
2. **Lock now** — dependency source policy (crates.io vs git vs path) once chosen
3. **Defer** — navigation/shell layout, view routing, keyboard bindings
4. **Defer** — theme/assets pipeline and persistent app state layout

**A)** Accept this lock/defer split
**B)** Adjust — move items between lock-now and defer (say which numbers)

**Recommend:** A unless you see another irreversible choice to lock in this slice.
Which do you want?

**Answer:** A — Accept lock/defer split:
Lock now: GPUI+gpui-component stack, cargo run entrypoint, dependency source policy (crates.io latest)
Defer: navigation/shell, view routing, keyboard, theme/assets, persistent state layout

---

### q-009

**Question:** Proposed **Workspace layout** section for design.md:

> **Cargo workspace** at repo root with binary crate **`tod`** at `crates/tod/`. Workspace membership leaves room for additional crates under `crates/` as the project grows.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** A — Accept workspace layout wording

---

### q-010

**Question:** Proposed **Application entry (`App::run`)** section for design.md:

> Dedicated **`app` module** owns GPUI startup, window open, and root view. `main.rs` is thin: delegate to `App::run()`.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** A — Accept application entry wording

---

### q-011

**Question:** Proposed **Window chrome** section for design.md:

> **Minimal gpui-component chrome:** use gpui-component window/frame primitives (`Root`, `TitleBar`) with **empty content** — no placeholder routes, lists, or navigation surfaces.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** A — Accept window chrome wording

---

### q-012

**Question:** Proposed **Dependency policy** section for design.md:

> - **`gpui`** and **`gpui-component`** from **crates.io**.
> - Use the **latest published versions** at dependency-add time; do not exact-pin semver unless reproducibility requires it later.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** A — Accept dependency policy wording

---

### q-013

**Question:** Proposed **Window defaults** and **Verification checks** sections for design.md:

**Window defaults**
1. Title — **`tod`**
2. Initial size — **1024×768**, resizable
3. Quit — window **close button** ends the process; no custom menu or shortcut in this task

**Verification checks** (Windows dev OS)
1. `cargo run` from repo root exits **0** and shows a visible window within **~10s**
2. Window title bar shows **`tod`**
3. Closing the window exits the process cleanly (no hang)

Out of scope for this task: `cargo build` with `-D warnings`.

**A)** Accept both sections as written
**B)** Modify — describe changes (say which section and what)
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** A — Accept window defaults + verification checks wording

---

### q-014

**Question:** Proposed **Licensing constraint (Zed / GPL)** and **Lock now vs defer** sections for design.md:

**Licensing constraint**
> Do **not** copy from the Zed repository or reference GPL-licensed Zed application code. Avoid Zed-specific code paths due to GPL licensing concerns. Use **GPUI and gpui-component public documentation** and **non-GPL examples** only when implementing this scaffold.

**Lock now**
1. UI stack — GPUI + gpui-component
2. Dev entrypoint — `cargo run` opens the desktop shell
3. Dependency source — crates.io, latest published versions
4. Workspace layout — Cargo workspace, `crates/tod` binary
5. App module pattern — `App::run()`
6. Window chrome level — minimal gpui-component frame with empty content

**Defer (later tasks)**
1. Navigation / app shell layout
2. View routing
3. Keyboard bindings and operability
4. Theme and assets pipeline
5. Persistent app state layout

**A)** Accept both sections as written
**B)** Modify — describe changes (by section or number)
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** A — Accept licensing constraint + lock/defer wording

---

### q-016

**Question:** I've reviewed the current **design** for this task. These top-level areas look complete and coherent for a first GPUI desktop shell with no UI surfaces — I don't see a compelling gap to propose next:

1. Workspace layout and crate structure (`crates/tod`)
2. GPUI app entry pattern (`App::run()` module)
3. Window chrome and defaults (minimal gpui-component frame, empty content)
4. Dependency sourcing (crates.io, latest published)
5. Verification checks (cargo run, title, clean quit; no `-D warnings`)
6. GPL / no-Zed-copy constraint
7. Lock-now vs defer split (irreversible choices vs later tasks)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** A — Design complete, nothing missing

---

### q-017

**Question:** Design for this scaffolding slice looks ready to close. Should we advance this task to planning?

**A)** Yes — start planning
**B)** Not yet — name what is still missing

**Recommend:** A if you accept the current design.md sections.
Which do you want?

**Answer:** A — Yes, advance to planning

---
