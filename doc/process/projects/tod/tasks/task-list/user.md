# Task list

Project: `doc/process/projects/tod/`

## Goal

Build the task list UI and verify keyboard navigation for a reusable list component — starting with list behavior before broader task-management features.

## Requirements

### Task list data

1. Mock/static fixture data — Task list rows use fixed or generated in-memory fixture data.

### Task row display

2. Row fields — Each task row displays title, lifecycle state, tags, and associated agent count (or fleet hint).

### Keyboard navigation

3. Arrow Up/Down — Moves keyboard selection one row; at the first or last row, selection remains on that row.
4. Page Up/Down — Moves keyboard selection by viewport/page.
5. Home/End — Home jumps selection to the first row; End jumps to the last row.
6. Focus and selection — The task list provides visible keyboard focus and row selection.

### List component

7. Reusable list primitive — Build a generic list primitive (e.g. `ListView<T>`) with a thin task-specific wrapper for row rendering; reuse target includes future agent list.

### Verification

8. Keyboard walkthrough — Manual keyboard walkthrough on one development OS verifies included keys move visible focus correctly (`cargo run`, exercise included keys).

## Constraints

1. UI stack — GPUI and gpui-component (same as ui-scaffolding).

2. Builds on ui-scaffolding — Extends existing `crates/tod` GPUI app.

3. Cross-platform — Verification on one development OS is sufficient.
