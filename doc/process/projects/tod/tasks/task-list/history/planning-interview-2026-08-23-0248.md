# Planning interview — task-list — 2026-08-23

## Session

**Agent:** Design agent advanced task `task-list` to planning and bootstrapped planning interview. Lifecycle state `planning`, mode `interactive`.

**Prior sessions:** Requirements interview (`task-requirements-interview-2026-08-23-0219.md`) and design interview (`design-interview-2026-08-23-0229.md`) complete; `design.md` approved.

**Context:** Builds on completed `ui-scaffolding` — GPUI + gpui-component desktop shell at `crates/tod`. Design locks generic `ListView<T>` under `src/ui/list/` and task wrapper under `src/views/task_list/`.

---

## q-001

**Question:** When the window opens and the task list auto-focuses, which row should start selected?

**A)** First row — selected and focused immediately
**B)** No row selected until the first Arrow Up/Down (list focused, no highlight)
**C)** Last row selected (End-like default)

**Recommend:** A — standard list open behavior; matches Home/End walkthrough starting from a visible selection.

**Answer:** A — First row selected and focused immediately on window open

---

## q-002

**Question:** How should each row show lifecycle state on line 2?

**A)** Raw slug from the project model (`planning`, `ready`, `done`, …)
**B)** Title-case label (`Planning`, `Ready`, `Done`, …)
**C)** Short badge/chip with title-case label inside

**Recommend:** B — readable demo data without extra UI chrome.

**Answer:** A — Raw lifecycle slug on line 2 (planning, ready, done, …)

---

## q-003

**Question:** How should tags appear on line 2?

**A)** Comma-separated plain text after the state (e.g. `ui, keyboard`)
**B)** Small badge/chip per tag on the same line
**C)** Tags omitted when a task has none; single `#tag` prefix style when present

**Recommend:** A — simplest two-line layout for this slice.

**Answer:** B — Small badge/chip per tag on line 2

---

## q-004

**Question:** How should line 2 show associated agents — numeric count vs fleet hint?

**A)** Always numeric — e.g. `3 agents`, or `0 agents` when none
**B)** Numeric when count > 0 (`3 agents`); omit agent info when count is 0
**C)** Fleet hint phrasing when count > 0 (e.g. `3 agents running`); `no agents` when 0

**Recommend:** A — clearest mapping to the requirement field "associated agent count."

**Answer:** A — Always numeric agent count (e.g. 3 agents, 0 agents)

---

## q-005

**Question:** Which development OS should the manual keyboard walkthrough use for verification?

**A)** Windows — same as ui-scaffolding initial verification
**B)** macOS
**C)** Linux

**Recommend:** A unless you actively develop on another OS now.

**Answer:** A — Windows for manual keyboard walkthrough verification

---

## q-006

**Question:** Approve this implementation step order for `plan.md`:

1. Add module skeleton — `src/ui/list/` (`ListView<T>`) and `src/views/task_list/`
2. Define `TaskItem` model and ~10–15 hand-authored fixture rows covering all 12 lifecycle states
3. Implement generic `ListView<T>` wrapping gpui-component `List` + `ListState` (search off, uniform row height)
4. Implement `TaskListDelegate` with two-line row renderer and selection/focus visuals
5. Register custom Page Up/Down/Home/End actions on the list (viewport row count for page keys)
6. Implement `TaskListView` GPUI entity with auto-focus on mount
7. Mount `TaskListView` from `Shell`, replacing the empty content area
8. Manual verification — `cargo run` + keyboard walkthrough per design checklist

**A)** Accept — use this order in `plan.md`
**B)** Modify — specify step changes
**C)** Reject — propose a different breakdown

**Recommend:** A unless a step should split or reorder.

**Answer:** A — Accept 8-step implementation order as proposed

---

## q-007

**Question:** Accept these planning assumptions for details design did not lock?

1. Mock fixtures live in `src/views/task_list/fixtures.rs`
2. Uniform row height is measured at runtime from the first rendered row (gpui-component list measurement)
3. Enter confirm is disabled via no-op `confirm()` on the delegate; `searchable(false)` on list state
4. Mouse single-click selection uses gpui-component list built-in click handling
5. Page/Home/End custom keys attach on the list focus context via GPUI actions, not a fork of gpui-component
6. `ListView<T>` exposes a thin generic wrapper (items + render callback + keyboard extensions); task wrapper supplies `TaskItem` rendering only

**A)** Accept all
**B)** Modify — specify which numbers to change
**C)** Reject — name assumptions that must become explicit requirements instead

**Recommend:** A unless an assumption conflicts with your intent.

**Answer:** A — Accept all 6 planning assumptions

---

## q-008

**Question:** Approve requirement-to-verification traceability for `plan.md`:

1. Mock/static fixture data → step 2 fixture module; verify rows visible on open (checklist item 1)
2. Row fields (title, state, tags, agent count) → step 4 row renderer; verify checklist item 8
3. Arrow Up/Down boundary behavior → gpui-component list + delegate; verify checklist items 3–4
4. Page Up/Down viewport semantics → step 5 custom actions; verify checklist item 5
5. Home/End → step 5 custom actions; verify checklist item 6
6. Focus and selection visuals → step 4 styling; verify checklist items 2 and 7
7. Reusable `ListView<T>` → steps 1 and 3 generic module; future agent list reuses same module (no separate test this slice)
8. Keyboard walkthrough → step 8 manual pass on chosen dev OS; full design checklist

**A)** Accept — use this traceability map in `plan.md`
**B)** Modify — specify mapping changes
**C)** Reject

**Recommend:** A unless a requirement needs a different verification hook.

**Answer:** A — Accept requirement-to-verification traceability map (user typed "except" — treat as Accept)

---

## q-009

**Question:** Approve this Goal wording for `plan.md`?

Build the task list UI and verify keyboard navigation for a reusable list component — starting with list behavior before broader task-management features.

**A)** Accept
**B)** Modify — provide revised wording
**C)** Reject

**Recommend:** A — matches approved `user.md` goal.

**Answer:** A — Accept Goal wording

---

## q-010

**Question:** Approve these eight implementation steps for `plan.md`?

1. **Module skeleton** — Add `crates/tod/src/ui/list/` (`ListView<T>`) and `crates/tod/src/views/task_list/` with module stubs and exports. Confirm `cargo check` passes.
2. **TaskItem model and fixtures** — Define `TaskItem` (title, lifecycle slug, tags, agent count) and ~10–15 hand-authored fixture rows in `src/views/task_list/fixtures.rs` covering all 12 project lifecycle states with varied tags and agent counts.
3. **Generic `ListView<T>`** — Implement thin wrapper around gpui-component `List` + `ListState`: items + render callback + keyboard extension hooks; `searchable(false)`; uniform row height measured at runtime from the first rendered row.
4. **`TaskListDelegate` row renderer** — Two-line row layout with selection/focus visuals (background highlight + focus ring/outline). Line 2: raw lifecycle slug, small badge/chip per tag, always-numeric agent count (`N agents`, including `0 agents`).
5. **Custom keyboard actions** — Register Page Up, Page Down, Home, and End on the list focus context via GPUI actions (viewport row count for page keys); no fork of gpui-component.
6. **`TaskListView` entity** — GPUI entity wiring delegate + list; auto-focus on mount with first row selected and focused immediately.
7. **Shell integration** — Mount `TaskListView` from `Shell`, replacing the empty content area from ui-scaffolding.
8. **Manual verification on Windows** — `cargo run` from repo root + keyboard walkthrough per design verification checklist.

**A)** Accept
**B)** Modify — specify step changes
**C)** Reject

**Recommend:** A — same order accepted in q-006, now with full module paths and step detail.

**Answer:** A — Accept 8 implementation steps

---

## q-011

**Question:** Approve these Constructions for `plan.md` (must match design / user constraints)?

1. Module layout — Generic `ListView<T>` under `src/ui/list/`; task wrapper under `src/views/task_list/`
2. List foundation — gpui-component `List` + `ListDelegate`; custom Page Up/Down/Home/End; search and Enter confirm disabled
3. Shell integration — `TaskListView` GPUI entity mounted from chrome-only `Shell`; replaces empty `flex_1()` content
4. Fixture data — ~10–15 hand-authored rows in `fixtures.rs`; all 12 lifecycle states; varied tags and agent counts
5. Row layout — Two lines: line 1 title; line 2 lifecycle slug + tag chips + agent count
6. Lifecycle state display — Raw slug from project model on line 2 (`planning`, `ready`, `done`, …)
7. Tags display — Small badge/chip per tag on line 2
8. Agent count display — Always numeric — e.g. `3 agents`, `0 agents` when none
9. Initial selection — First row selected and focused immediately on window open
10. Focus visuals — Background highlight (selection) + focus ring/outline (keyboard focus) on selected row
11. Page Up/Down — Move selection by visible viewport row count (list height ÷ uniform row height, rounded)
12. Mouse selection — Single click selects row and moves keyboard focus (gpui-component built-in click handling)
13. Enter confirm — Disabled via no-op `confirm()` on delegate
14. List search — Disabled via `searchable(false)` on list state
15. Generic wrapper API — `ListView<T>`: items + render callback + keyboard extensions; task wrapper supplies `TaskItem` rendering only
16. UI stack — GPUI and gpui-component (same as ui-scaffolding)
17. Cross-platform — Verification on Windows dev OS only; other OSes not verified in this task

**A)** Accept all
**B)** Modify — specify which numbers to change
**C)** Reject

**Recommend:** A — names every design-mandated construction for the planning gate.

**Answer:** A — Accept Constructions table

---

## q-012

**Question:** Approve this requirement traceability table for `plan.md`?

1. Mock/static fixture data → Step 2 fixture module → `fixtures.rs` → Design checklist #1 (task list visible on open)
2. Row fields (title, state, tags, agent count) → Step 4 row renderer → `TaskListDelegate` two-line renderer → Checklist #8
3. Arrow Up/Down boundary behavior → Steps 3–4 `ListView<T>` + delegate → Checklist #3–4
4. Page Up/Down viewport semantics → Step 5 custom actions → GPUI actions on list focus context → Checklist #5
5. Home/End → Step 5 custom actions → GPUI actions on list focus context → Checklist #6
6. Focus and selection visuals → Step 4 styling + Step 6 first-row selection on mount → Checklist #2 and #7
7. Reusable `ListView<T>` → Steps 1 and 3 generic module → `src/ui/list/` → future agent list reuses (no separate test this slice)
8. Keyboard walkthrough → Step 8 manual pass on Windows → full design verification checklist

**A)** Accept
**B)** Modify — specify mapping changes
**C)** Reject

**Recommend:** A — same mapping accepted in q-008, now in plan table form.

**Answer:** A — Accept requirement traceability table

---

## q-013

**Question:** Approve these Assumptions for `plan.md`?

1. Mock fixtures live in `src/views/task_list/fixtures.rs`.
2. Uniform row height is measured at runtime from the first rendered row (gpui-component list measurement).
3. Enter confirm is disabled via no-op `confirm()` on the delegate; `searchable(false)` on list state.
4. Mouse single-click selection uses gpui-component list built-in click handling.
5. Page/Home/End custom keys attach on the list focus context via GPUI actions, not a fork of gpui-component.
6. `ListView<T>` exposes a thin generic wrapper (items + render callback + keyboard extensions); task wrapper supplies `TaskItem` rendering only.
7. Developer has a Windows dev environment where GPUI can run (same bar as ui-scaffolding).
8. Cross-platform correctness is not verified in this task — Windows proof is sufficient per `user.md` constraint #3.

**A)** Accept all
**B)** Modify — specify which numbers to change
**C)** Reject — name assumptions that must become explicit requirements instead

**Recommend:** A — extends q-007 acceptance with Windows dev environment and cross-platform verification scope.

**Answer:** A — Accept Assumptions (8 items)

---

## q-014

**Question:** Approve this Verification section for `plan.md`?

Manual verification only on **Windows**. Run from repo root after Steps 1–7 are complete.

1. App window opens with the task list visible within **~10s** — `cargo run` from repo root; confirm task list appears promptly
2. Without clicking, keyboard focus (focus ring/outline) on the **first row**, which is also selected — observe on open, no prior click or tab
3. **Arrow Down** moves selection and focus to the next row; **Arrow Up** to the previous row — exercise through several rows
4. At first row **Arrow Up** holds selection; at last row **Arrow Down** holds selection — test boundary rows
5. **Page Down** moves selection down by approximately visible viewport row count; **Page Up** up by the same — resize window if needed
6. **Home** moves selection to first row; **End** to last row
7. Selected row shows both background highlight and focus ring/outline at the same time
8. Rows show title on line 1; raw lifecycle slug, tag badge/chips, and numeric agent count (`N agents`) on line 2

Out of scope for this task: automated tests for list keyboard behavior; verification on macOS or Linux.

**A)** Accept
**B)** Modify — specify check or out-of-scope changes
**C)** Reject

**Recommend:** A — observable checks tied to Windows manual walkthrough per design.

**Answer:** A — Accept Verification section

---

## q-015

**Question:** I've reviewed the draft `plan.md` for this phase. These sections look complete and coherent for an actionable implementation plan — I don't see a compelling gap:

1. Goal — aligned with `user.md` scope (list UI + keyboard nav before broader task management)
2. Steps — eight ordered build steps from skeleton through shell integration and Windows manual verification
3. Constructions — design-mandated choices named (module layout, list foundation, row layout, keyboard semantics, shell integration, etc.)
4. Requirement traceability — each `user.md` requirement maps through plan elements to a verification check
5. Assumptions — eight implementation details design did not lock (fixtures path, row height measurement, disabled search/confirm, Windows verification scope, etc.)
6. Verification — Windows manual checklist with eight observable checks and explicit out-of-scope notes

Did we miss anything important for `plan.md`?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.

**Answer:** A — Completeness confirmed, enough to advance

---
