# Task list

Project: `doc/process/projects/tod/`

## Goal

Provide the primary task list pane for tod: scannable two-line rows, keyboard-first selection, and per-row actions to launch or jump to agents and shells — as the left half of the main situational chrome.

## Requirements

### Pane and chrome

1. **Task list pane** — Primary task list on the left; agent fleet shares the window on the right (situational-ui).

2. **List action bar** — **New task** only. No global Agent or Terminal controls on this bar. No separate **From Linear** control — ticket import happens through inline New task compose (req 32).

3. **Empty and no-matches** — Zero tasks: show the action bar and a minimal empty message (e.g. “No tasks”). Tasks exist but filter/search hides all rows: show a distinct “no matches” message and a control to clear active filter and search together.

### Row layout and live updates

4. **Two-line rows** — Line 1: ticket id when present and title. Line 2: lifecycle state, agents control, shells control, and tags in case-insensitive alphabetical order.

5. **Live row fields** — Title, ticket id, lifecycle state, tags, agents count, and shells count on visible rows update while the list is open when those values change, without relaunch.

6. **Agents count** — Number of agents associated with that task.

7. **Shells count** — Number of open shell sessions into environments of agents associated with that task.

### Selection and navigation

8. **Selection visuals and bounds** — One selected row at a time (see Constraints). Selected row is visually distinct (accent + focus treatment).

9. **Keyboard navigation** — Arrow Up/Down move selection without leaving the list at ends; Page Up, Page Down, Home, and End also move selection.

10. **Selection when filtered out** — When the selected task leaves the visible set (tag filter or search), selection moves to the nearest visible row.

11. **Selection after delete** — When the selected task is permanently deleted, selection moves to the nearest remaining visible row; if none remain visible, no selection.

12. **Selection preserved on sort change** — Changing sort key or direction keeps the current task selected when it remains visible.

13. **Initial selection** — Cold open with tasks (or missing restored selection): automatically select and keyboard-focus a visible row.

### Row activation — lifecycle next

14. **Row activation** — Enter or activating non-actionable row chrome runs that task’s lifecycle next action (req 15). Excludes the title edit affordance (req 16). On the selected row, Enter runs lifecycle next without changing selection. Pointer-click on an unselected row’s non-actionable chrome (including title) selects that row and runs lifecycle next in one activation. The task list stays visible.

15. **Lifecycle next routing** — Lifecycle next is the primary action for a task given its current lifecycle state. Row activation (req 14) and lifecycle chip activation (per row-control activation constraints) both use this routing:
   - **proposed / design / planning** — Open the interview when interview work remains (open questions, active unbound session, or no session yet). When the interview for that phase is **complete** (SQLite `complete`, empty queue), lifecycle next opens the **lifecycle transition panel** with transition actions (forward / go back / related) instead of re-opening or re-kickoffing the interview.
   - **ready / active** — Lifecycle panel: on **ready**, optional notes and launch implementation agent (submit advances to **active**); on **active**, in-progress status (not kickoff-only).
   - **verifying** — Verification panel: launch agent if none; else show status.
   - **review** — Open/view associated PR when present; else prompt an agent to open a PR.
   - **learn** — Open or focus learn retrospective.
   - **approved / merged / released / done** — Lifecycle panel with transition actions.

### Open task edit

16. **Edit control** — Selected row only: shortcut badge **E** on the lower-right of the title text (same placement/style as agents/shells badges). Interim entry point until a dedicated edit affordance is designed.

17. **Open edit from selected row** — **E** or activating the title on the selected row opens the task edit slide-over (task-crud); the task list stays visible.

18. **Edit follows list selection** — While task edit is open from the list, selecting a different task switches edit to that task.

19. **Edit closes for compose** — When inline New task compose opens (task-crud), any open task edit closes first.

### Row controls — agents, shells, lifecycle

20. **Agents control** — Per row-control activation constraints: pointer on any row; badge on selected row only. Count 0: create agent for that task and open/focus its detail. Count > 0: menu of existing agents plus create new; choosing existing or create opens/focuses that agent’s detail.

21. **Shells control** — Same pattern as agents; UI label **shells** (not terminals/terms). When the task has multiple associated agents and zero shells, count-0/create prompts which agent environment first. Create or menu choice opens/focuses the shell session.

22. **Lifecycle chip** — Per row-control activation constraints. Activating runs lifecycle next (req 15).

### Tag filter

23. **Tag filter** — Activating a tag chip (any row) or matching digit badge (selected row, tags 1–9 → digits 1–9, tenth → 0) filters to that tag. Active filter shows which tag in list chrome with dismissible clear (pointer and keyboard). Re-activating the same tag clears the filter; activating a different tag switches filter (no stacking). Matching tags on visible rows show active highlight.

24. **Filter and search stack** — Active tag filter and fuzzy search combine (both must match). Clearing either leaves the other in effect.

25. **Create selection respects filter** — After New task creates a task (title or ticket import), selection moves to it only when it matches active filter/search; otherwise selection stays unchanged.

### Fuzzy search and sort

26. **Fuzzy search** — Smart fuzzy search over title, ticket id, tags, and state. Search field is keyboard-focusable; user can clear the query from keyboard (exact shortcuts in design/settings).

27. **Sort keys and direction** — Sort by interaction timestamp (default), title (case-insensitive), lifecycle state (project sequence), or ticket id (lexicographic on displayed id; tasks without ticket id after those with one). Each key supports ascending and descending. Initial direction when switching key: timestamp → newest-first; title → ascending; lifecycle → descending; ticket id → descending.

28. **Sort chrome** — Sort control on the list header shows active key and direction with a keyboard badge. Activating it (pointer or shortcut) cycles sort keys and opens an informational dropdown of all keys; other input dismisses the dropdown. No separate reset control.

29. **Interaction timestamp** — Default order uses per-task interaction timestamp, updated on row activation (lifecycle next), open task edit, agents/shells interaction, and lifecycle chip activation (not selection/focus alone). Successful New task create (title or ticket import) sets timestamp at create time.

### Working-set restore

30. **List working-set restore** — After relaunch: restore selected task (when it still exists), active tag filter, and sort key plus direction. Fuzzy search query does not restore.

### New task and ticket import via compose

31. **New task** — **New task** on the action bar opens inline compose per task-crud (`Compose/edit ownership`). List action bar and keyboard access for New task are owned here; compose fields, validation, and dismiss semantics are owned by task-crud.

32. **Ticket id in compose** — When the user submits the compose title, tod first checks whether the trimmed value matches the ticket-id pattern (e.g. `TOD-142`). When it matches, tod runs **from ticket** import: call the configured issue tracker (Linear by default), fetch title and other issue fields, and create or select the task. When the ticket already has a task: do not duplicate; dismiss compose and select the existing task when visible under current filter/search. Fetch failures show toast/banner and keep compose open with focus on the title input. When the value does not match the ticket-id pattern, create proceeds as a normal title (task-crud).

### List chrome keyboard access

33. **Keyboard reachability** — New task (**N**), search focus (**/**), sort (**S**), tag-filter clear, and edit (**E** on selected row) are keyboard-reachable with visible shortcut badges per project keyboard-badge constraints.

## Constraints

1. UI stack — GPUI and gpui-component (project shell).

2. Reuse list primitive — Prefer the generic list primitive for virtualization and keyboard navigation.

3. Visual package — Match [`artifacts/visual/task-list/`](artifacts/visual/task-list/) (**Accepted** 2026-08-27).

4. List scale — Usable at the project ~500-task UI target (virtualization or equivalent).

5. Cross-task ownership — Inline New task compose and task edit slide-over are owned by task-crud; this task owns list pane layout, action bar, row controls (including interim edit entry), filter/sort/search, lifecycle routing from rows, ticket import via compose title, and list-level selection semantics.

6. Situational chrome — Follow situational-ui for main split and shortcut-badge placement; follow project-adopted actionable-affordance and keyboard-badge shared constraints.

7. Shared practices — Follow project-adopted shared constraints: focus return after overlay, invalid field submit feedback, selectable data, logging, resizable dividers, list selection scroll-into-view, singleton entry surface, row control activation, actionable affordance, and keyboard badges.

8. Tag count bound — At most 10 tags per task (project constraint); list shows digit badges for up to ten tags on the selected row.
