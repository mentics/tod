# Task requirements interview — task-list — 2026-08-23

## Session

**Agent:** Proposed agent bootstrapped task `task-list` requirements interview. Lifecycle state `proposed`, mode `interactive`. Human invoked `/process` to run requirements for this task.

**Human direction (chat, not yet in user.md):**

- Task is **task list** — NOT app-shell, NOT interview-ui-slice
- Focus: build task list UI, starting with keyboard navigation
- Goal: verify a reusable list component behaves as expected
- Prior task **ui-scaffolding** (done): runnable GPUI desktop shell, no lists/navigation

**Scoping questions raised in chat (awaiting answers):**

1. Mock vs real persisted data
2. Row content/fields
3. Which keyboard behaviors (arrows, enter, home/end, type-to-focus, etc.)
4. What's deferred (sort/filter/tags/fuzzy search/detail nav/full app keyboard)
5. Success bar for verification

---

## q-001

For this task-list slice, should rows be backed by **mock/static fixture data** or by **real local task persistence** (read/write that survives app restart)?

**A)** Mock/static only — fixed or generated in-memory fixture; no persistence layer in this task
**B)** Real persistence — minimal durable task store on disk (create/read/update enough to populate the list)
**C)** Hybrid — mock data now, but shaped like future persisted tasks (no save yet)

**Recommend:** A for a keyboard-navigation-first slice unless you want persistence wiring in the same task.
Which do you want?

**Answer:** A (mock/static only — fixed or generated in-memory fixture; no persistence layer)

<!-- answer-anchor: q-001 -->

---

## q-002

What should each **task row show** in the list for this slice? Name the numbers to include (or describe a different set):

1. Title
2. Slug
3. Lifecycle state (proposed → design → … → done)
4. Tags
5. Git repository / branch
6. Freeform notes preview
7. Associated agent count or fleet hint

**Recommend:** 1 and 3 for a readable task list with minimal chrome.
Which numbers (or what else)?

**Answer:** 1, 3, 4, 7 (Title, Lifecycle state, Tags, Associated agent count)

<!-- answer-anchor: q-002 -->

---

## q-003

Which **keyboard move-focus behaviors** must work in this task? Name the numbers to include:

1. Arrow Up / Down — move selection one row
2. Page Up / Page Down — move selection by viewport/page
3. Home / End — jump to first / last row
4. Wrap at list ends (last→first, first→last) when using arrows

**Recommend:** 1 and 3 for a solid navigation baseline; 2 optional; 4 usually off unless you want wrap.
Which numbers?

**Answer:** 1, 2, 3 (Arrow Up/Down, Page Up/Down, Home/End — NOT wrap at list ends)

<!-- answer-anchor: q-003 -->

---

## q-004

When a row is keyboard-focused, what should **Enter** do in this slice?

**A)** No action — focus/selection only in this task
**B)** Activate/open — navigate to a task detail view (even if detail is stub/minimal)
**C)** Toggle/select — mark row selected without leaving the list
**D)** Something else — describe briefly

**Recommend:** A if detail navigation is deferred; B only if a minimal detail stub is in scope.
Which do you want?

**Answer:** A (No action — focus/selection only; Enter does nothing in this task)

<!-- answer-anchor: q-004 -->

---

## q-005

Is **type-to-focus** (type characters to jump/filter rows by prefix, e.g. title) required in this task?

**A)** Yes — type-to-focus must work on the task list in this slice
**B)** No — defer type-to-focus to a later task (arrow/home navigation only for now)
**C)** Partial — visual focus ring/scroll-into-view only; no character matching yet

**Recommend:** B for a first keyboard slice; project fuzzy search (req 7) is a separate concern.
Which do you want?

**Answer:** B (Defer type-to-focus)

<!-- answer-anchor: q-005 -->

---

## q-006

Confirm these stay **explicitly out of scope** for this task (not even stubbed):

1. Sort, filter, and group controls (incl. sort/filter by tags)
2. Fuzzy search on the list
3. Task detail page / edit flows
4. App shell navigation chrome (sidebar, tabs, multi-pane layout)
5. Full-app keyboard coverage and customizable shortcuts (project reqs 28–29)
6. Agent list / fleet views

**A)** Accept all six as out of scope for this task
**B)** Adjust — name what should move in-scope or what to add as excluded

**Recommend:** A unless you need one of these for keyboard verification.
Which do you want?

**Answer:** A (Accept all six out-of-scope items)

<!-- answer-anchor: q-006 -->

---

## q-007

How should the **reusable list component** scope read for this task?

**A)** Build a generic list primitive (e.g. `ListView<T>`) and use it for the task list — reuse target includes future agent list
**B)** Task-specific list only — extract/reuse deferred until a second consumer exists
**C)** Generic primitive required, but task list may use a thin task-specific wrapper for row rendering

**Recommend:** C — generic focus/scroll/keyboard behavior with task-specific row content.
Which do you want?

**Answer:** C (Generic list primitive + thin task-specific wrapper for row rendering)

<!-- answer-anchor: q-007 -->

---

## q-008

What is the minimum **verification / done bar** for this task to count as complete?

1. Manual keyboard walkthrough on one dev OS (`cargo run`, exercise included keys, visible focus moves correctly)
2. Automated tests for list keyboard behavior (unit/integration)
3. List handles at least ~500 rows without obvious jank (project task scale hint)
4. Scroll-into-view when selection moves off-screen

Name the numbers that must pass (or describe a different bar).

**Recommend:** 1 and 4 for this slice; 2 optional; 3 only if you want scale proof now.
Which numbers?

**Answer:** 1 only (Manual keyboard walkthrough on one dev OS — NOT scroll-into-view, NOT automated tests, NOT 500-row scale)

<!-- answer-anchor: q-008 -->

---

## q-009

Approve this **Goal** wording for task `user.md`?

> Build the task list UI and verify keyboard navigation for a reusable list component — starting with list behavior before broader task-management features.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — explain why

**Recommend:** Accept unless the scope emphasis should shift.
Which do you want?

**Answer:** A (Accept Goal wording)

<!-- answer-anchor: q-009 -->

---

## q-010

Approve this **Task list data** requirement wording?

> Mock/static fixture data — Task list rows use fixed or generated in-memory fixture data.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — explain why

**Recommend:** Accept — matches mock-only scope with no persistence layer.
Which do you want?

**Answer:** A (Accept Task list data requirement)

<!-- answer-anchor: q-010 -->

---

## q-011

Approve this **Task row display** requirement wording?

> Row fields — Each task row displays title, lifecycle state, tags, and associated agent count (or fleet hint).

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — explain why

**Recommend:** Accept unless a field should be added, removed, or renamed.
Which do you want?

**Answer:** A (Accept Task row display requirement)

<!-- answer-anchor: q-011 -->

---

## q-012

Approve this **Keyboard navigation** wording (requirements 3–6)?

3. Arrow Up/Down — Moves keyboard selection one row; at the first or last row, selection remains on that row.
4. Page Up/Down — Moves keyboard selection by viewport/page.
5. Home/End — Home jumps selection to the first row; End jumps to the last row.
6. Focus and selection — The task list provides visible keyboard focus and row selection.

**A)** Accept all four
**B)** Modify — describe changes (by number)
**C)** Reject — explain why

**Recommend:** Accept — covers included keys, no wrap, and visible focus without Enter activation.
Which do you want?

**Answer:** A (Accept all four keyboard navigation requirements)

<!-- answer-anchor: q-012 -->

---

## q-013

Approve this **List component** requirement wording?

> Reusable list primitive — Build a generic list primitive (e.g. `ListView<T>`) with a thin task-specific wrapper for row rendering; reuse target includes future agent list.

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — explain why

**Recommend:** Accept — generic primitive plus task wrapper, reuse aimed at future agent list.
Which do you want?

**Answer:** A (Accept List component requirement)

<!-- answer-anchor: q-013 -->

---

## q-014

Approve this **Verification** requirement wording?

> Keyboard walkthrough — Manual keyboard walkthrough on one development OS verifies included keys move visible focus correctly (`cargo run`, exercise included keys).

**A)** Accept
**B)** Modify — describe changes
**C)** Reject — explain why

**Recommend:** Accept — manual walkthrough only; no automated tests, scroll-into-view, or scale bar.
Which do you want?

**Answer:** A (Accept Verification requirement)

<!-- answer-anchor: q-014 -->

---

## q-015

Approve this **Constraints** wording (all three)?

1. UI stack — GPUI and gpui-component (same as ui-scaffolding).
2. Builds on ui-scaffolding — Extends existing `crates/tod` GPUI app.
3. Cross-platform — Verification on one development OS is sufficient.

**A)** Accept all three
**B)** Modify — describe changes (by number)
**C)** Reject — explain why

**Recommend:** Accept unless a binding limit is missing for this slice.
Which do you want?

**Answer:** A (Accept all three Constraints)

<!-- answer-anchor: q-015 -->

---

## q-016

Approve this **Out of scope** wording? (Adds deferred items from earlier answers not yet listed.)

Explicitly excluded (not even stubbed):

1. Sort, filter, and group controls (incl. sort/filter by tags)
2. Fuzzy search on the list
3. Task detail page / edit flows
4. App shell navigation chrome (sidebar, tabs, multi-pane layout)
5. Full-app keyboard coverage and customizable shortcuts (project reqs 28–29)
6. Agent list / fleet views
7. Type-to-focus on the list (character jump/filter)
8. Scroll-into-view when selection moves off-screen
9. Automated tests for list keyboard behavior

**A)** Accept all nine
**B)** Modify — describe changes (by number)
**C)** Reject — explain why

**Recommend:** Accept — closes gaps from deferrals on type-to-focus, scroll-into-view, and automated tests.
Which do you want?

**Answer:** A (Accept all nine Out of scope items)

<!-- answer-anchor: q-016 -->

---

## q-017

I've reviewed the current requirements for this phase. These top-level areas look complete and coherent for a keyboard-navigation-first task list slice — I don't see a compelling gap to propose next:

1. Task list data (mock fixtures)
2. Task row display (title, state, tags, agent count)
3. Keyboard navigation (arrows, page, home/end, visible focus — no wrap, no Enter action)
4. Reusable list primitive with task wrapper
5. Manual keyboard verification on one dev OS
6. Constraints (GPUI stack, ui-scaffolding base, single-OS verification)
7. Out-of-scope exclusions (sort/filter/search/detail/shell/fleet/type-to-focus/etc.)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** A (Completeness confirmed — enough for now)

<!-- answer-anchor: q-017 -->

---
