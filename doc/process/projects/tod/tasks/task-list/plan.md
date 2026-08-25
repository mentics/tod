# Task list — plan

## Goal (from user.md)

Build the task list UI and verify keyboard navigation for a reusable list component — starting with list behavior before broader task-management features.

## Steps

1. **Module skeleton** — Add `crates/tod/src/ui/list/` (`ListView<T>`) and `crates/tod/src/views/task_list/` with module stubs and exports. Confirm `cargo check` passes.
2. **TaskItem model and fixtures** — Define `TaskItem` (title, lifecycle slug, tags, agent count) and ~10–15 hand-authored fixture rows in `src/views/task_list/fixtures.rs` covering all 12 project lifecycle states with varied tags and agent counts.
3. **Generic `ListView<T>`** — Implement thin wrapper around gpui-component `List` + `ListState`: items + render callback + keyboard extension hooks; `searchable(false)`; uniform row height measured at runtime from the first rendered row.
4. **`TaskListDelegate` row renderer** — Two-line row layout with selection/focus visuals (background highlight + focus ring/outline). Line 2: raw lifecycle slug, small badge/chip per tag, always-numeric agent count (`N agents`, including `0 agents`).
5. **Custom keyboard actions** — Register Page Up, Page Down, Home, and End on the list focus context via GPUI actions (viewport row count for page keys); no fork of gpui-component.
6. **`TaskListView` entity** — GPUI entity wiring delegate + list; auto-focus on mount with **first row selected and focused immediately**.
7. **Shell integration** — Mount `TaskListView` from `Shell`, replacing the empty content area from ui-scaffolding.
8. **Manual verification on Windows** — `cargo run` from repo root + keyboard walkthrough per design verification checklist (see [Verification](#verification)).

## Constructions (must match design / user constraints)

| Concern | Construction |
|--|--|
| Module layout | Generic `ListView<T>` under `src/ui/list/`; task wrapper under `src/views/task_list/` |
| List foundation | gpui-component `List` + `ListDelegate`; custom Page Up/Down/Home/End; search and Enter confirm disabled |
| Shell integration | `TaskListView` GPUI entity mounted from chrome-only `Shell`; replaces empty `flex_1()` content |
| Fixture data | ~10–15 hand-authored rows in `fixtures.rs`; all 12 lifecycle states; varied tags and agent counts |
| Row layout | Two lines: line 1 title; line 2 lifecycle slug + tag chips + agent count |
| Lifecycle state display | Raw slug from project model on line 2 (`planning`, `ready`, `done`, …) |
| Tags display | Small badge/chip per tag on line 2 |
| Agent count display | Always numeric — e.g. `3 agents`, `0 agents` when none |
| Initial selection | First row selected and focused immediately on window open |
| Focus visuals | Background highlight (selection) + focus ring/outline (keyboard focus) on selected row |
| Page Up/Down | Move selection by visible viewport row count (list height ÷ uniform row height, rounded) |
| Mouse selection | Single click selects row and moves keyboard focus (gpui-component built-in click handling) |
| Enter confirm | Disabled via no-op `confirm()` on delegate |
| List search | Disabled via `searchable(false)` on list state |
| Generic wrapper API | `ListView<T>`: items + render callback + keyboard extensions; task wrapper supplies `TaskItem` rendering only |
| UI stack | GPUI and gpui-component (same as ui-scaffolding) |
| Cross-platform | Verification on Windows dev OS only; other OSes not verified in this task |

## Requirement traceability

| user.md requirement | Plan element | Implementation (to fill) | Check |
|--|--|--|--|
| 1. Mock/static fixture data | Step 2 fixture module | `src/views/task_list/fixtures.rs` | Pass — fixtures on open |
| 2. Row fields (title, state, tags, agent count) | Step 4 row renderer | `TaskListDelegate` two-line renderer | Pass — human visual check |
| 3. Arrow Up/Down — one row; boundary hold | gpui-component list + clamped arrows (Steps 3–5) | `ListArrow*` + wrap shadow / revert scroll | Pass — human 2026-08-24 |
| 4. Page Up/Down — viewport/page | Step 5 custom actions | GPUI actions on list focus context | Pass — human walkthrough |
| 5. Home/End — first/last row | Step 5 custom actions | GPUI actions on list focus context | Pass — human walkthrough |
| 6. Focus and selection visuals | Step 4 styling; Step 6 first-row selection on mount | Delegate styling + `TaskListView` auto-focus | Pass — human visual check |
| 7. Reusable `ListView<T>` | Steps 1 and 3 generic module | `src/ui/list/` | Pass — module present |
| 8. Keyboard walkthrough | Step 8 manual pass on Windows | N/A | Pass — human 2026-08-24 |

## Assumptions

1. Mock fixtures live in `src/views/task_list/fixtures.rs`.
2. Uniform row height is measured at runtime from the first rendered row (gpui-component list measurement).
3. Enter confirm is disabled via no-op `confirm()` on the delegate; `searchable(false)` on list state.
4. Mouse single-click selection uses gpui-component list built-in click handling.
5. Page/Home/End custom keys attach on the list focus context via GPUI actions, not a fork of gpui-component.
6. `ListView<T>` exposes a thin generic wrapper (items + render callback + keyboard extensions); task wrapper supplies `TaskItem` rendering only.
7. Developer has a Windows dev environment where GPUI can run (same bar as ui-scaffolding).
8. Cross-platform correctness is not verified in this task — Windows proof is sufficient per user.md constraint #3.

## Verification

Manual verification only on **Windows**. Run from repo root after Steps 1–7 are complete.

| # | Check | How to run |
|--|--|--|
| 1 | App window opens with the task list visible within **~10s** | Run `cargo run` from repo root; confirm task list appears promptly |
| 2 | Without clicking, the list shows keyboard focus (focus ring/outline) on the **first row**, which is also selected | Observe on open — no prior click or tab required |
| 3 | **Arrow Down** moves selection highlight and focus to the next row; **Arrow Up** to the previous row | Exercise arrow keys through several rows |
| 4 | At the first row, **Arrow Up** leaves selection on the first row; at the last row, **Arrow Down** leaves selection on the last row | Test boundary rows |
| 5 | **Page Down** moves selection down by approximately the visible viewport row count; **Page Up** up by the same | Resize window if needed to observe viewport-sized jumps |
| 6 | **Home** moves selection to the first row; **End** to the last row | Exercise Home and End keys |
| 7 | Selected row shows both background highlight and focus ring/outline at the same time | Visually inspect selected row |
| 8 | Rows show title on line 1; raw lifecycle slug, tag badge/chips, and numeric agent count (`N agents`) on line 2 | Inspect several fixture rows |

**Out of scope for this task:** automated tests for list keyboard behavior; verification on macOS or Linux.
