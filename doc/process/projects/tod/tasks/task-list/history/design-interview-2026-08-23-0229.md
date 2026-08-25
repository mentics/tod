# Design interview — task-list — 2026-08-23

## Session

**Agent:** Design agent bootstrapped design interview for task `task-list`. Lifecycle state `design`, mode `interactive`.

**Prior session:** `task-requirements-interview-2026-08-23-0219.md` — requirements accepted; human approved `user.md` with 8 requirements (task list + keyboard navigation + reusable `ListView<T>`).

**Context:** Builds on completed `ui-scaffolding` — GPUI + gpui-component desktop shell at `crates/tod` with minimal chrome and empty content area.

---

### q-001

**Question:** Where should the reusable list primitive and task-specific list live in the `crates/tod` source tree?

**A)** `src/ui/list/` for generic `ListView<T>` + `src/views/task_list/` for the task wrapper
**B)** Flat under `src/app/` alongside `window.rs` (list + task list modules)
**C)** Separate workspace crate (e.g. `crates/tod-ui`) for reusable list code

**Recommend:** A — keeps generic UI separate from shell wiring without a new crate yet.
Which do you want?

**Answer:** A — `src/ui/list/` for generic `ListView<T>` + `src/views/task_list/` for the task wrapper

---

### q-002

**Question:** What should the **list rendering foundation** be for `ListView<T>`? (gpui-component `List` ships up/down only; page/home/end would need custom handling either way.)

**A)** Custom `ListView<T>` on GPUI `list()` + `ListState` — own keyboard nav and focus
**B)** gpui-component `List` + `ListDelegate` for virtualization — supplement for page/home/end; disable search and Enter confirm
**C)** Non-virtualized scroll column of uniform rows — simplest; no GPUI list primitive

**Recommend:** A — full control for all required keys without fighting component defaults.
Which do you want?

**Answer:** B — gpui-component `List` + `ListDelegate` for virtualization; supplement page/home/end; disable search and Enter confirm

---

### q-003

**Question:** How should the task list mount inside the existing `Shell` window (TitleBar + empty content area from ui-scaffolding)?

**A)** Replace the empty `flex_1()` content div directly with the task list view
**B)** Mount a `TaskListView` entity from `Shell`; keep `Shell` as chrome-only wrapper
**C)** Introduce a minimal `AppView` router with a single route (task list only)

**Recommend:** B — extends the shell pattern without premature routing.
Which do you want?

**Answer:** B — Mount `TaskListView` entity from `Shell`; `Shell` stays chrome-only wrapper

---

### q-004

**Question:** How should mock fixture data be shaped for this slice?

**A)** ~10–15 hand-authored rows covering all 12 lifecycle states with varied tags and agent counts
**B)** ~30+ programmatically generated rows with pseudo-random tags/counts
**C)** Minimal ~5 rows — enough to exercise first/last boundaries only

**Recommend:** A — readable demo data; scale proof (~500 rows) is out of scope.
Which do you want?

**Answer:** A — ~10–15 hand-authored rows covering all 12 lifecycle states with varied tags and agent counts

---

### q-005

**Question:** How should each task row lay out title, lifecycle state, tags, and agent count?

**A)** Single line — title left; state badge + tag chips + agent count right
**B)** Two lines — title on line 1; state, tags, and count on line 2
**C)** Title in a fixed left column; state, tags, and count inline after title on one line

**Recommend:** A — dense single-line rows suit keyboard list navigation.
Which do you want?

**Answer:** B — Two lines: title on line 1; state, tags, and count on line 2

---

### q-006

**Question:** How should visible keyboard focus and row selection appear?

**A)** gpui-component theme focus ring on the selected row only
**B)** Custom selected-row background highlight only (no separate focus ring)
**C)** Both — background highlight for selection plus focus ring/outline for keyboard focus

**Recommend:** C — satisfies "visible keyboard focus and row selection" distinctly.
Which do you want?

**Answer:** C — Both background highlight for selection plus focus ring/outline for keyboard focus

---

### q-007

**Question:** When the window opens, should the task list receive keyboard focus automatically so arrow keys work without clicking first?

**A)** Yes — auto-focus the list on window open
**B)** No — user must click or tab to the list before keyboard navigation applies

**Recommend:** A — matches keyboard-first verification intent.
Which do you want?

**Answer:** A — Auto-focus list on window open

---

### q-008

**Question:** What should **Page Up / Page Down** move selection by?

**A)** Visible viewport row count (list height ÷ uniform row height, rounded)
**B)** Fixed row count (e.g. 10 rows per page regardless of window size)
**C)** Scroll-container page distance; selection jumps to the nearest row at the new scroll position

**Recommend:** A — matches "viewport/page" wording in requirements.
Which do you want?

**Answer:** A — Page Up/Down moves by visible viewport row count

---

### q-009

**Question:** When the user clicks a task row with the mouse, what should happen?

**A)** Single click selects that row and moves keyboard focus/selection to it (same state as arrow-key navigation)
**B)** No mouse row selection in this slice — keyboard-only; clicks on rows have no selection effect
**C)** Click focuses the list widget but does not change which row is selected until keyboard input

**Recommend:** A — standard list affordance; full keyboard-only operation remains possible without the mouse.
Which do you want?

**Answer:** A — Single click selects row and moves keyboard focus/selection (same as arrow-key navigation)

---

### q-010

**Question:** Proposed **Verification checks** section for `design.md` (manual keyboard walkthrough on one development OS):

On the developer OS, this task is done when `cargo run` from repo root and a manual keyboard walkthrough confirm:

1. App window opens with the task list visible within ~10s (same bar as ui-scaffolding)
2. Without clicking, the list shows keyboard focus (focus ring/outline) on a row
3. Arrow Down moves selection highlight and focus to the next row; Arrow Up to the previous row
4. At the first row, Arrow Up leaves selection on the first row; at the last row, Arrow Down leaves selection on the last row
5. Page Down moves selection down by approximately the visible viewport row count; Page Up up by the same
6. Home moves selection to the first row; End to the last row
7. Selected row shows both background highlight and focus ring/outline at the same time
8. Rows show title on line 1 and lifecycle state, tags, and agent count on line 2

**A)** Accept — use as written in `design.md`
**B)** Modify — specify changes
**C)** Reject — drop or replace the checklist approach

**Recommend:** A unless a check is wrong or missing for this slice.
Which do you want?

**Answer:** A — Accept verification checks checklist as written in design.md

---

### q-011

**Question:** Approve **Module layout** wording for `design.md`:

> **Generic list primitive** lives under `crates/tod/src/ui/list/` as `ListView<T>`.
>
> **Task-specific wrapper** lives under `crates/tod/src/views/task_list/` — thin adapter for task row rendering and fixture wiring.
>
> Reuse target (future agent list) consumes the generic `ListView<T>` module; task wrapper stays task-specific.

**A)** Accept
**B)** Modify — specify changes
**C)** Reject

Which do you want?

**Answer:** A — Accept Module layout section wording

---

### q-012

**Question:** Approve **List rendering foundation** wording for `design.md`:

> **gpui-component `List` + `ListDelegate`** provides virtualization and arrow Up/Down navigation.
>
> Custom handling supplements **Page Up**, **Page Down**, **Home**, and **End** (not provided by gpui-component `List` alone).
>
> Disable gpui-component list **search** and **Enter confirm** behaviors for this slice.

**A)** Accept
**B)** Modify — specify changes
**C)** Reject

Which do you want?

**Answer:** A — Accept List rendering foundation section

---

### q-013

**Question:** Approve **Shell integration** and **Mock fixture data** wording for `design.md`:

> **Mount `TaskListView` as a GPUI entity from `Shell`.** `Shell` remains a chrome-only wrapper (TitleBar + content slot); it does not embed list rendering inline.
>
> Replaces the empty `flex_1()` content area from ui-scaffolding without introducing routing.
>
> **~10–15 hand-authored rows** in memory.
>
> Rows cover **all 12 project lifecycle states** (`proposed` → `design` → `planning` → `ready` → `active` → `verifying` → `review` → `approved` → `merged` → `released` → `learn` → `done`) with **varied tags and agent counts**.
>
> No programmatic random generation; scale proof (~500 rows) is out of scope per requirements.

**A)** Accept
**B)** Modify — specify changes
**C)** Reject

Which do you want?

**Answer:** A — Accept Shell integration + Mock fixture data sections

---

### q-014

**Question:** Approve **Task row layout** wording for `design.md`:

> **Two lines per row:**
>
> 1. **Line 1** — task title
> 2. **Line 2** — lifecycle state, tags, and associated agent count (or fleet hint)
>
> Uniform row height for viewport/page calculations.

**A)** Accept
**B)** Modify — specify changes
**C)** Reject

Which do you want?

**Answer:** A — Accept Task row layout section

---

### q-015

**Question:** Approve **Focus and selection** and **Keyboard focus on open** wording for `design.md`:

> **Both** visual cues on the selected row:
>
> 1. **Background highlight** — row selection
> 2. **Focus ring or outline** — keyboard focus (gpui-component theme or equivalent)
>
> Satisfies requirement for visible keyboard focus and row selection as distinct signals.
>
> **Auto-focus the task list on window open** so arrow keys work without a prior click or tab.

**A)** Accept
**B)** Modify — specify changes
**C)** Reject

Which do you want?

**Answer:** A — Accept Focus and selection + Keyboard focus on open sections

---

### q-016

**Question:** Approve **Page Up / Page Down** wording for `design.md`:

> Move keyboard selection by **visible viewport row count** — list height ÷ uniform row height, rounded.
>
> Matches "viewport/page" wording in requirements; not a fixed row count and not scroll-position-only without selection update.

**A)** Accept
**B)** Modify — specify changes
**C)** Reject

Which do you want?

**Answer:** A — Accept Page Up / Page Down section

---

### q-017

**Question:** I've reviewed the current **design** for this task. These top-level areas look complete and coherent for a keyboard-navigation-first task list slice — I don't see a compelling gap to propose next:

1. Module layout — generic `ListView<T>` under `src/ui/list/` + task wrapper under `src/views/task_list/`
2. List rendering foundation — gpui-component `List`/`ListDelegate`; custom Page/Home/End; search and Enter disabled
3. Shell integration — `TaskListView` entity mounted from chrome-only `Shell`
4. Mock fixture data — ~10–15 hand-authored rows with all lifecycle states, varied tags and agent counts
5. Task row layout — two-line rows with uniform height
6. Focus and selection — background highlight + focus ring; auto-focus list on window open
7. Page Up/Down and mouse interaction — viewport row count; single-click row selection
8. Verification checks — manual keyboard walkthrough checklist on one dev OS
9. Lock-now decisions — irreversible choices recorded; no open defer items

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** A — No, design is enough for now. That's great. Proceed.

---
