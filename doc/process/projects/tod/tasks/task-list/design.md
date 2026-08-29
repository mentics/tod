# Design — task-list

Task: `doc/process/projects/tod/tasks/task-list/`

## Intention

Primary left-pane task fleet for situational main chrome: scannable two-line rows, keyboard-first selection, per-row agents/shells/lifecycle controls, and list chrome for fuzzy search, tag filter, and sort — without global Agent/Terminal on the list action bar. Inline New task compose and task edit slide-over field semantics are owned by task-crud; this task owns pane layout, list chrome, row controls, selection semantics, ticket import via compose title, and lifecycle routing from rows.

## Constructions

| Concern | Construction |
|--|--|
| Pane role | Left pane of main split (~58% width in visual package); right pane is agent fleet (~42%; placeholder in visual package — real fleet owned by agent-list-detail) ([`situational-ui`](../situational-ui/design.md)) |
| Action bar | **New task** — no global Agent or Terminal controls; no separate From Linear control |
| List header | Primary action line: New task; search · active tag filter (when set) · sort control — all actionable items use button/link/chip affordance and keyboard badges |
| Row adjacency | Two-line rows stack with no vertical gap between rows; row separators only |
| Empty list | Action bar remains; body shows minimal empty message (e.g. “No tasks”) |
| No matches | When tasks exist but tag filter and/or fuzzy search hide every row, show “No tasks match.” (or equivalent) and a **Clear filters** control that clears both active tag filter and search query |
| Row line 1 | Ticket id when present (link color, semibold), then title |
| Row line 2 | Lifecycle chip · agents control · shells control · tags in case-insensitive alphabetical order |
| Uniform row height | Fixed two-line row height for viewport/page calculations and virtualization |
| Live row fields | Title, ticket id, lifecycle state, tags, agents count, and shells count on visible rows update while the list is open when those values change, without relaunch |
| Agents count | Number of agents associated with that task |
| Shells count | Number of open shell sessions into environments of agents associated with that task; UI label **shells** (not terminals/terms) |
| Selection visuals | One selected row at a time; selected row shows 3px left accent border, tertiary fill, and 1px accent outline (background highlight + focus ring) |
| Keyboard navigation | Arrow Up/Down move selection; at first/last row the corresponding arrow leaves selection unchanged; Page Up/Down by visible viewport row count; Home/End to first/last row |
| Scroll into view | On keyboard selection moves, working-set restore, and post-create selection moves — per shared list-selection-scroll-into-view constraint |
| Selection when filtered out | When the selected task leaves the visible set (tag filter or search), selection moves to the nearest visible row |
| Selection after delete | When the selected task is permanently deleted, selection moves to the nearest remaining visible row; if none remain visible, no selection |
| Selection on sort change | Changing sort key or direction keeps the current task selected when it remains visible |
| Initial selection | Cold open with tasks (or missing restored selection): automatically select and keyboard-focus a visible row |
| Pointer row select | Single click on non-actionable row chrome selects that row and moves keyboard focus (same as arrow navigation) |
| Row activation | Enter or activating non-actionable row chrome runs lifecycle next (table below). Excludes title edit affordance. On the selected row, Enter runs lifecycle next without changing selection. Pointer-click on an unselected row’s non-actionable chrome (including title) selects that row and runs lifecycle next in one activation; task list stays visible |
| Lifecycle next routing | Row activation and lifecycle chip activation both run the task’s lifecycle next action — the primary action given current lifecycle state (see table below) |
| Edit control | Selected row only: shortcut badge **E**, lower-right on title text, ~50% translucent background (same placement/style as agents/shells). Interim entry point until a dedicated edit affordance is designed |
| Open edit — selected row | **E** or activating title on the **selected** row opens task edit slide-over ([`task-crud`](../task-crud/design.md)); task list stays visible |
| Edit follows selection | While task edit is open from the list, selecting a different task switches edit to that task |
| Edit closes for compose | When inline New task compose opens (task-crud), any open task edit closes first |
| Agents control label | Chip text `agents {N}` (lowercase, count in label) |
| Agents control badge | Selected row only: shortcut badge **A**, lower-right on chip, ~50% translucent background (per situational-ui shortcut-badge principle) |
| Agents control menu | Count > 0: dropdown below chip; each existing agent as `{label} · {runtime status}`; trailing **New agent…** (accent). Re-activating chip while menu is open closes menu. Count 0 → create immediately and open/focus detail |
| Shells control label | Chip text `shells {N}` (not terminals/terms) |
| Shells control badge | Selected row only: shortcut badge **T**, same badge placement/style as agents |
| Shells control menu | Same dropdown pattern as agents with **New shell…** trailing item. Count 0 → create (prompt agent environment when multiple agents); choice opens/focuses shell session |
| Lifecycle chip | Per row-control activation constraints. Activating runs lifecycle next (same routing as row activation; see table below) |
| Tag filter activate | Activating a tag chip (any row) or matching digit badge on the **selected** row (tags 1–9 → digits 1–9, tenth tag → digit 0) filters the list to that tag |
| Tag filter chrome | Inline on list header when active: tag label chip (click clears filter) + **Clear tag** with keyboard badge |
| Tag filter toggle | Re-activating the same tag clears the filter; activating a different tag switches filter (no stacking) |
| Tag filter highlight | Matching tags on visible rows show active highlight while filter is on |
| Filter + search stack | Active tag filter and fuzzy search combine — both must match |
| Create selection vs filter | After New task creates a task (title or ticket import), selection moves to it only when it matches active filter/search; otherwise selection stays unchanged |
| Fuzzy search | Smart fuzzy search over title, ticket id, tags, and lifecycle state; search field is keyboard-focusable; user can clear the query from keyboard (exact shortcuts in design/settings) |
| Sort keys | Interaction timestamp (default), title (case-insensitive), lifecycle state (project sequence), ticket id (lexicographic on displayed id; tasks without ticket id sort after those with one) |
| Sort direction | Each key supports ascending and descending. Initial direction when switching key: timestamp → newest-first; title → ascending; lifecycle → descending; ticket id → descending |
| Sort chrome | Sort control on list header with keyboard badge. Activating (pointer or shortcut) cycles sort key and opens an informational dropdown listing all keys with direction; highlighted row matches current sort. Repeated activation cycles keys while dropdown stays open until any other user input dismisses it. Pointer may pick a listed key. No separate reset control — cycle back to default key |
| Interaction timestamp | Default sort uses per-task interaction timestamp, updated on row activation (lifecycle next), open task edit, agents/shells control interaction, and lifecycle chip activation — not on selection/focus alone. Successful New task create (title or ticket import) sets timestamp at create time |
| Working-set restore | After relaunch: restore selected task (when it still exists), active tag filter, and sort key plus direction. Fuzzy search query does **not** restore |
| Ticket id in compose | On compose submit, trimmed title checked against ticket-id pattern (e.g. `TOD-142`). Match → **from ticket** import via configured issue tracker (Linear default): fetch issue fields, create task, or select existing when duplicate. No match → normal title create (task-crud). Fetch failure: toast/banner, compose stays open, focus on title input. Browseable ticket list out of scope |
| New task | **New task** on the action bar opens inline compose per task-crud; list owns action bar placement and keyboard access (**N**); compose fields, validation, and dismiss semantics owned by task-crud |
| List chrome keyboard | New task (**N**), search focus (**/**), sort (**S**), tag-filter clear, and edit (**E** on selected row) are keyboard-reachable with visible badges per keyboard-badge constraints |
| List foundation | gpui-component `List` + `ListDelegate` for virtualization and arrow navigation; custom Page Up/Down/Home/End; disable gpui-component list search and Enter confirm |
| Module layout | Generic `ListView<T>` under `crates/tod/src/ui/list/`; task wrapper under `crates/tod/src/views/task_list/` |
| Shell integration | Mount `TaskListView` entity from `Shell`; shell stays chrome-only wrapper |
| Mock / live data | Hand-authored fixtures covering lifecycle variety until persistence supplies live data; list must remain usable at project ~500-task UI target |
| Cross-task ownership | Inline compose and task edit slide-over owned by task-crud; situational split and shortcut-badge principle owned by situational-ui |
| Actionable chrome | Row and list controls use chip/button affordances per actionable-affordance, keyboard-badge, and situational-ui constraints |
| Row action feedback | Agents/shells/lifecycle/tag actions surface outcome text in the bottom status area (situational-ui status area) |

### Visual package scope (Accepted canvas)

Binding layout/interaction from [`artifacts/visual/task-list/source.canvas.tsx`](artifacts/visual/task-list/source.canvas.tsx) (live authoring copy documented in [`notes.md`](artifacts/visual/task-list/notes.md)). **In scope for this package:** task pane action bar, search + tag-filter chrome, two-line rows, agents/shells chips and dropdowns, lifecycle chip activation entry, tag chips, selection treatment, no-matches empty body. **Out of scope in canvas (owned elsewhere):** sort chrome (design above), lifecycle panel contents (routing table), inline New task compose row (task-crud), real agent fleet pane (agent-list-detail), digit tag badges (user.md; not shown in canvas mock).

### Lifecycle next routing

Row activation (Enter / row chrome / title) and lifecycle chip activation both use this table.

| Lifecycle state | Lifecycle next action |
|--|--|
| proposed / design / planning | Open interview when interview work remains (open questions, active unbound session, or no session yet). When the phase interview is **complete** (SQLite `complete`, empty queue), lifecycle next opens the lifecycle transition panel instead of re-kickoff |
| ready / active | Lifecycle panel: on **ready**, optional notes and launch implementation agent (submit advances to **active**); on **active**, in-progress status (not kickoff-only) |
| verifying | Verification panel: launch agent if none; else show status |
| review | Open/view associated PR when present; else prompt an agent to open a PR |
| learn | Open or focus learn retrospective |
| approved / merged / released / done | Lifecycle panel with transition actions |

## Lock now vs defer

### Lock now

1. Two-line row layout, live field updates, and shells naming
2. Action bar scope (New task only; ticket import via compose title)
3. Selection semantics (filter-out, delete, sort-preserve, initial, scroll-into-view)
4. Row activation runs lifecycle next (same routing as lifecycle chip)
5. Interim edit control on selected-row title (**E** badge) and edit handoff with task-crud
6. Agents/shells control semantics including multi-agent shell-create prompt
7. Tag filter behavior (chip + digit badges, stack with search, chrome clear)
8. Sort keys, directions, chrome, interaction timestamp rules, and working-set restore scope
9. Ticket import via compose title, duplicate handling, and error feedback
10. Lifecycle next routing table per state
11. List foundation (gpui-component List + delegate, module layout, ~500-task scale target)
12. Visual package [`artifacts/visual/task-list/`](artifacts/visual/task-list/) **Accepted** 2026-08-27

### Defer

1. **Browseable Linear ticket list** — out of scope this phase (ticket id via compose title only)
2. **Project grouping in list UI** — project association owned by task-crud / project requirements
3. **Post-reopen design interview transcript** — expanded scope encoded directly from reconciled `user.md`; no new Q&A session recorded after 2026-08-25 reopen (see gate assessment)

#### Deferred spike decision tree

| Spike / defer | If outcome → action |
|--|--|
| Visual Accept pending | ~~Human Accepts canvas + `preview.png` captured~~ → **Done** 2026-08-27 |
| Shortcut bindings unset | Settings task defines bindings → task-list reads bindings at runtime; until then use provisional defaults documented in planning |
| Persistence not ready | Continue mock/hand-authored fixtures → swap data source when persistence task lands without changing list chrome constructions |

## Verification (manual)

On one development OS, `cargo run` from repo root and a manual walkthrough confirm observable behavior for each `user.md` requirement group: pane chrome and empty/no-matches messages; two-line live rows; selection and keyboard navigation including ends and page/home/end; row activation (lifecycle next), interim edit (**E**), and compose handoff with task-crud; agents/shells controls; lifecycle routing entry points; tag filter + fuzzy search stack; sort chrome and interaction timestamp ordering; working-set restore (selected task, tag filter, sort — not search query); ticket import via compose title; list chrome keyboard reachability (**N**, **S**, **/**, **E**). Selected row shows both background highlight and focus ring. Rows virtualize or equivalent at ~500-task scale.

## Links / external references

| Link | Scope | Binding |
|--|--|--|
| [`artifacts/visual/task-list/`](artifacts/visual/task-list/) | Task list pane layout and row interactions | **required** |
| [`../situational-ui/design.md`](../situational-ui/design.md) | Main chrome split, shortcut-badge principle, actionable chip/button chrome | required |
| [`../task-crud/design.md`](../task-crud/design.md) | Inline compose, task edit slide-over, ticket import via compose title | required |
| [`../../shared/constraints/row-control-activation-constraints.md`](../../shared/constraints/row-control-activation-constraints.md) | Pointer chip activation; badges on selected row only | required |
| [`../../shared/constraints/list-selection-scroll-into-view-constraints.md`](../../shared/constraints/list-selection-scroll-into-view-constraints.md) | Scroll selected row into view | required |
| [`../../shared/constraints/invalid-submit-feedback-constraints.md`](../../shared/constraints/invalid-submit-feedback-constraints.md) | Compose invalid submit | required |
| [`../../shared/constraints/focus-return-after-overlay-constraints.md`](../../shared/constraints/focus-return-after-overlay-constraints.md) | Focus return after overlays | required |
| [`../../shared/constraints/keyboard-badge-constraints.md`](../../shared/constraints/keyboard-badge-constraints.md) | Visible shortcut badge on every actionable control | required |
| [`../../shared/constraints/actionable-affordance-constraints.md`](../../shared/constraints/actionable-affordance-constraints.md) | Actionables look clickable | required |
