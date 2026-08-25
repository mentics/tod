# Design — task-list

Task: `doc/process/projects/tod/tasks/task-list/`

Builds on [ui-scaffolding design](../ui-scaffolding/design.md) — GPUI + gpui-component desktop shell at `crates/tod` with minimal chrome.

## Module layout

**Generic list primitive** lives under `crates/tod/src/ui/list/` as `ListView<T>`.

**Task-specific wrapper** lives under `crates/tod/src/views/task_list/` — thin adapter for task row rendering and fixture wiring.

Reuse target (future agent list) consumes the generic `ListView<T>` module; task wrapper stays task-specific.

## List rendering foundation

**gpui-component `List` + `ListDelegate`** provides virtualization and arrow Up/Down navigation.

Custom handling supplements **Page Up**, **Page Down**, **Home**, and **End** (not provided by gpui-component `List` alone).

Disable gpui-component list **search** and **Enter confirm** behaviors for this slice.

## Shell integration

**Mount `TaskListView` as a GPUI entity from `Shell`.** `Shell` remains a chrome-only wrapper (TitleBar + content slot); it does not embed list rendering inline.

Replaces the empty `flex_1()` content area from ui-scaffolding without introducing routing.

## Mock fixture data

**~10–15 hand-authored rows** in memory.

Rows cover **all 12 project lifecycle states** (`proposed` → `design` → `planning` → `ready` → `active` → `verifying` → `review` → `approved` → `merged` → `released` → `learn` → `done`) with **varied tags and agent counts**.

No programmatic random generation; scale proof (~500 rows) is out of scope per requirements.

## Task row layout

**Two lines per row:**

1. **Line 1** — task title
2. **Line 2** — lifecycle state, tags, and associated agent count (or fleet hint)

Uniform row height for viewport/page calculations.

## Focus and selection

**Both** visual cues on the selected row:

1. **Background highlight** — row selection
2. **Focus ring or outline** — keyboard focus (gpui-component theme or equivalent)

Satisfies requirement for visible keyboard focus and row selection as distinct signals.

## Keyboard focus on open

**Auto-focus the task list on window open** so arrow keys work without a prior click or tab.

## Page Up / Page Down

Move keyboard selection by **visible viewport row count** — list height ÷ uniform row height, rounded.

Matches "viewport/page" wording in requirements; not a fixed row count and not scroll-position-only without selection update.

## Mouse click selection

**Single click** on a task row selects that row and moves keyboard focus/selection to it — same state as arrow-key navigation.

Full keyboard-only operation remains possible without the mouse.

## Lock now vs defer

### Lock now

1. **Module layout** — `src/ui/list/` + `src/views/task_list/` (see [Module layout](#module-layout))
2. **List foundation** — gpui-component `List` + `ListDelegate`; custom page/home/end; search and Enter confirm disabled (see [List rendering foundation](#list-rendering-foundation))
3. **Shell integration** — `TaskListView` entity mounted from chrome-only `Shell` (see [Shell integration](#shell-integration))
4. **Fixture shape** — ~10–15 hand-authored rows, all lifecycle states (see [Mock fixture data](#mock-fixture-data))
5. **Row layout** — two-line rows (see [Task row layout](#task-row-layout))
6. **Focus visuals** — background highlight + focus ring/outline (see [Focus and selection](#focus-and-selection))
7. **Auto-focus on open** (see [Keyboard focus on open](#keyboard-focus-on-open))
8. **Page Up/Down semantics** — viewport row count (see [Page Up / Page Down](#page-up--page-down))
9. **Mouse click selection** — single click selects row and moves focus (see [Mouse click selection](#mouse-click-selection))

## Verification checks

On the developer OS, this task is done when `cargo run` from repo root and a manual keyboard walkthrough confirm:

1. App window opens with the task list visible within **~10s** (same bar as ui-scaffolding)
2. Without clicking, the list shows keyboard focus (focus ring/outline) on a row
3. **Arrow Down** moves selection highlight and focus to the next row; **Arrow Up** to the previous row
4. At the first row, **Arrow Up** leaves selection on the first row; at the last row, **Arrow Down** leaves selection on the last row
5. **Page Down** moves selection down by approximately the visible viewport row count; **Page Up** up by the same
6. **Home** moves selection to the first row; **End** to the last row
7. Selected row shows both background highlight and focus ring/outline at the same time
8. Rows show title on line 1 and lifecycle state, tags, and agent count on line 2
