# Task requirements interview — ui-scaffolding — 2026-08-23

## Session

**Agent:** Process agent scaffolded task `ui-scaffolding` under project `tod`. Lifecycle state `proposed`, mode `interactive`. Beginning task requirements interview.

**User request:** Start a new task to implement scaffolding — first task is building the scaffolding for the UI.

---

### q-001

**Question:** For this task, does "UI scaffolding" mean a runnable desktop app shell with navigation and placeholder pages for the main views — but no real task/agent operations or integrations yet?

**Answer:** just runnable desktop

---

### q-002

**Question:** Which primary UI surfaces must exist as at least placeholder routes when this scaffolding task is done?

1. App shell / navigation chrome
2. Task list
3. Agent list / fleet view
4. Task detail page
5. Agent detail page
6. Status area
7. Notifications queue
8. Settings

Name the numbers that must be included (or describe a different set).

**Answer:** none

---

### q-003

**Question:** For scaffolded views in this task, should data be static/mock only, or should the UI wire to real local persistence stubs (read/write that survives restart)?

**Answer:** n/a

---

### q-004

**Question:** Which tech choices should this task lock in task user.md constraints vs leave to design?

1. Desktop shell framework (Tauri, Electron, etc.)
2. Frontend UI library (React, Svelte, etc.)
3. Component / styling approach
4. Client state management approach

**A)** None — defer all to design
**B)** Some — which numbers (and name choices if already decided)

**Recommend:** A unless you already have firm preferences to record now.
Which do you want?

**Answer:** GPUI and gpui-component (same as what zed uses)

---

### q-005

**Question:** What is the minimum "runnable" bar for this scaffolding task to count as done?

**A)** Local dev preview (unpackaged app window or dev server)
**B)** Packaged installable desktop build for at least one OS
**C)** Both dev preview and a packaged build

**Recommend:** A for a first greenfield scaffolding slice.
Which do you want?

**Answer:** just cargo run

---

### q-006

**Question:** Project tod requires full keyboard operability eventually. For this scaffolding task, how much keyboard support is required?

**A)** Full keyboard navigation across all scaffolded views
**B)** Basic shell navigation only; per-view keyboard coverage deferred
**C)** Defer keyboard requirements to a later task

**Recommend:** B — routable shell without full feature parity yet.
Which do you want?

**Answer:** no kb yet

---

### q-007

**Question:** Project tod requires Windows, macOS, and Linux. For this scaffolding task, how much cross-platform verification is required before done?

**A)** Must run on all three OSes in this task
**B)** One development OS is enough; multi-OS proof deferred
**C)** Must build for all three; manual verify on one OS is enough

**Recommend:** B for first scaffolding unless multi-OS CI is already cheap.
Which do you want?

**Answer:** just current os

---

### q-008

**Question:** For this scaffolding task, I propose these stay explicitly out of scope (not even stubbed):

1. Agent launch and runtime operations
2. External integrations (Slack, Linear, GitHub)
3. Fuzzy search on lists
4. Real fleet persistence and JSON import
5. Credential management UI

**A)** Accept as out of scope for this task
**B)** Adjust — name what should move in-scope or what to add as excluded

**Recommend:** A unless you need a stub for a specific flow.
Which do you want?

**Answer:** yes, out of scope

---

### q-009

**Question:** Proposed **goal** for task user.md:

> Deliver a runnable local desktop application shell for tod — no navigation chrome or placeholder pages in this task.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** Accept goal wording

---

### q-010

**Question:** Proposed **requirement 1** (runnable desktop shell) for task user.md:

> Runnable desktop shell — `cargo run` opens a desktop application window on the developer's OS.
>
> Success criteria:
> - Running `cargo run` from the project produces a visible desktop application window

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** Accept requirement 1

---

### q-011

**Question:** Proposed **requirement 2** (no placeholder UI surfaces) for task user.md:

> No placeholder UI surfaces — This task does not require app shell navigation, task/agent lists, detail pages, status area, notifications queue, or settings as routes or views.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** Accept requirement 2

---

### q-012

**Question:** Proposed **requirement 3** (out of scope) for task user.md:

> Out of scope — The following are explicitly excluded (not even stubbed): agent launch and runtime operations; external integrations (Slack, Linear, GitHub); fuzzy search on lists; real fleet persistence and JSON import; credential management UI.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** Accept requirement 3

---

### q-013

**Question:** Proposed **constraints** for task user.md:

1. UI stack — Use GPUI and gpui-component (same stack as Zed).
2. Dev preview only — No packaged installable desktop build is required in this task.
3. Cross-platform — Verification on one development OS is sufficient; multi-OS proof is deferred.
4. Keyboard — Keyboard operability requirements are deferred to a later task.

**A)** Accept all four
**B)** Modify — describe changes (by number)
**C)** Reject — describe what to use instead

Which do you want?

**Answer:** Accept all four constraints

---

### q-014

**Question:** I've reviewed the current **requirements** for this task. These top-level areas look complete and coherent for a runnable GPUI desktop shell with no UI surfaces yet — I don't see a compelling gap to propose next:

1. Runnable desktop shell (`cargo run` → visible window)
2. No placeholder UI surfaces (navigation, lists, detail pages, status, notifications, settings)
3. Explicit out-of-scope boundaries (agent ops, integrations, search, persistence/import, credentials UI)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** Requirements complete, nothing missing

---

### q-015

**Question:** I've reviewed the current **constraints** for this task. These areas look complete and coherent for a first dev-preview scaffolding slice — I don't see a compelling gap to propose next:

1. UI stack (GPUI / gpui-component)
2. Dev preview only (no packaged install)
3. Cross-platform verification scope (one dev OS; multi-OS deferred)
4. Keyboard operability (deferred to later task)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** Constraints complete, nothing missing

---

### q-016

**Question:** Task requirements gathering looks ready to close for this scaffolding slice. Should we advance this task to design?

**A)** Yes — start design
**B)** Not yet — name what is still missing

**Recommend:** A if you accept the current goal, requirements, and constraints.
Which do you want?

**Answer:** Yes, advance to design

---
