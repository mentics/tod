# Task list — plan

Task: `doc/process/projects/tod/tasks/task-list/`

## Goal

Rebuild the primary left-pane task list for situational main chrome: two-line rows, list chrome (action bar, fuzzy search, tag filter, sort), keyboard-first selection, and per-row agents/shells/lifecycle controls — per `user.md`, `design.md`, and Accepted visual package.

## Current baseline

| Area | Status |
|--|--|
| `crates/tod/src/ui/list/` | Generic `ListView<T>`, keyboard actions, `TWO_LINE_ROW_HEIGHT`, page/home/end helpers |
| `crates/tod/src/views/task_list/` | Partial `TaskListView` + `TaskListDelegate`; loads process tasks via `scan_process_tasks`; Enter opens interview (legacy) |
| Row renderer | Title + lifecycle + tags + `N agents` only — missing ticket id, shells, chips, badges, menus |
| Chrome | No action bar, search, sort, or tag-filter UI |
| Shell | Tab-based `Shell` in `app/window.rs`, not situational split |

**Rebuild strategy:** extend/replace in place under `views/task_list/`; keep `ListView` foundation; migrate shell mounting when situational-ui lands (Step 12).

## Steps

1. **Task row model** — Extend `TaskItem` (or replace with domain type): `id`, optional `ticket_id`, `title`, `lifecycle`, sorted `tags`, `agents`/`shells` counts (or linked ids), `interaction_timestamp`. Expand `fixtures.rs` for all lifecycle states + varied counts/tags/tickets; keep `scan_process_tasks` adapter until persistence is ready.

2. **Pane entity skeleton** — Introduce `TaskListPane` entity: vertical layout = action bar + list chrome (search, tag-filter row, sort row) + list body + empty/no-matches states. Replace bare `TaskListView` render tree; preserve list focus context and keyboard bindings.

3. **Action bar** — **New task** button only (no global Agent/Terminal). Wire **New task** to inline compose (task-crud). Ticket import: when compose title matches ticket-id pattern, fetch from configured issue tracker (Linear default).

4. **Fuzzy search + filter stack** — Search `TextInput` above list; filter visible rows by query over title, ticket id, tags, lifecycle. Maintain `tag_filter: Option<String>`; combined predicate = tag match AND search match. No-matches body: “No tasks match.” + **Clear filters** (clears both).

5. **Sort pipeline** — `ListWorkingSet { sort_key, sort_direction, tag_filter, selected_id }`. Sort keys: interaction timestamp (default newest-first), title, lifecycle (project order), ticket id (blanks last). Sort chrome: control + active label + reset-to-default; preserve selection when sorted task remains visible.

6. **Interaction timestamp** — Field on each task; bump on row activation (lifecycle next), open edit, agents/shells activate, lifecycle chip activate, and successful create (title or ticket import via compose). Default sort uses this field.

7. **Two-line row renderer** — Rebuild `TaskListDelegate::render_item` to match visual package: line 1 ticket (link style) + title; line 2 lifecycle chip · `agents {N}` · `shells {N}` · tag chips (alpha order). Selected row: 3px left accent, fill, outline. Use gpui-component chip/button primitives.

8. **Row control components** — Shared `RowChip` with optional shortcut badge (selected row only). Agents/shells: count 0 → create + open/focus; count > 0 → dropdown (`{label} · {status}` / **New agent…** / **New shell…**); toggle-close on re-activate. Shells count-0 with multiple agents → environment picker prompt.

9. **Tag filter UX** — Tag chip click toggles/switches filter; digit badges 1–9/0 on selected row (up to 10 tags). Tag-filter chrome: “Filtered by tag” + chip + Clear. Active tag highlight on matching chips.

10. **Selection semantics** — Initial select+focus visible row; clamp arrows at ends (keep existing wrap-revert); page/home/end + scroll-into-view; when filter/search hides selection → nearest visible row; preserve on sort change; post-delete nearest row or none.

11. **Row activation — lifecycle next** — Enter / pointer on non-actionable row chrome (excludes title edit affordance) → emit `OpenLifecycle { task_id, lifecycle }` per design routing table (same as lifecycle chip). Pointer on chips selects row per row-control activation constraint.

12. **Open edit handoff** — **E** / title activate on selected row → emit `OpenTaskEdit { task_id }` (stub panel until task-crud). Title shows **E** badge lower-right when selected. Edit follows list selection; compose opens closes edit (event to task-crud).

13. **Lifecycle chip routing** — Same `OpenLifecycle` emission as Step 11; stub panels/toasts until process UI exists. Status-area feedback for row actions.

14. **Ticket import via compose** — On compose submit, if title matches ticket-id pattern, call issue tracker (Linear default stub); duplicate selects existing; fetch error keeps compose open. Normal title otherwise creates task.

15. **Working-set restore** — Persist/restore selected task id, tag filter, sort key+direction (not search query) via local settings or persistence stub; scroll restored selection into view.

16. **Live row updates** — Subscribe to task/agent/shell change signals (fixture mutations first; persistence events later) and refresh visible row fields without relaunch.

17. **Main chrome integration** — Mount `TaskListPane` as left pane of situational split (coordinate with situational-ui / replace tab shell). Until split lands, mount pane in Tasks tab as interim with note in journal.

18. **Keyboard reachability** — Bindings: New task **N**, sort **S**, search focus **/**, tag clear, edit (**E**), row badges (A/T/digits).

19. **Scale check** — Verify list virtualizes smoothly with ~500 fixture rows (generate fixture helper).

20. **Manual verification** — `cargo run` walkthrough per [Verification](#verification) on Windows.

## Constructions (from design.md)

Use named constructions in `design.md` Constructions table and visual package — especially: pane ~58%, chip labels, badge placement, dropdown menu shape, tag-filter chrome, no-matches copy, module layout (`ui/list/` + `views/task_list/`), `ListView` + disabled list search/confirm.

## Requirement traceability

| Req | Plan step(s) | Verification check |
|--|--|--|
| 1 Task list pane | 2, 17 | Left pane visible beside agent area in main split (or interim tab) |
| 2 Action bar | 3 | New task only; no global Agent/Terminal; no From Linear button |
| 3 Empty / no-matches | 2, 4 | Zero tasks → “No tasks”; filtered out → “No tasks match.” + Clear filters |
| 4 Two-line rows | 7 | Ticket+title line 1; lifecycle·agents·shells·tags line 2 |
| 5 Live row fields | 16 | Edit/count changes reflect without relaunch |
| 6 Agents count | 1, 7 | Chip shows associated agent count |
| 7 Shells count | 1, 7 | Chip shows open shell session count |
| 8 Selection visuals | 7, 10 | One row; accent + focus treatment |
| 9 Keyboard navigation | 10 | Arrows, page, home/end; ends hold |
| 10 Filtered-out selection | 4, 10 | Selection moves to nearest visible row |
| 11 Selection after delete | 10 | Nearest visible or none |
| 12 Sort preserves selection | 5, 10 | Same task stays selected when visible |
| 13 Initial selection | 10 | Cold open selects+f focuses a row |
| 14 Row activation | 11, 13 | Enter / row chrome runs lifecycle next |
| 15 Lifecycle next routing | 11, 13 | Same routing for row activation and lifecycle chip |
| 16 Edit control | 7, 12 | Selected row title shows **E** badge lower-right |
| 17 Open edit from selected row | 12 | **E** / title opens edit |
| 18 Edit follows selection | 12 | Switch selection switches edit |
| 19 Edit closes for compose | 3, 12 | New task closes edit first |
| 20 Agents control | 8, 11 | 0/create/menu/open detail; badge on selected row |
| 21 Shells control | 8 | Same pattern; multi-agent prompt; **shells** label |
| 22 Lifecycle chip | 8, 13 | Runs lifecycle next (req 15) |
| 23 Tag filter | 4, 9 | Chip/digit toggle/switch/clear/highlight/chrome |
| 24 Filter + search stack | 4 | Both apply; clear one keeps other |
| 25 Create selection vs filter | 5, 14 | New task create (title or ticket) respects filter/search |
| 26 Fuzzy search | 4, 18 | Narrows rows; keyboard focus/clear |
| 27 Sort keys/direction | 5 | All keys + initial directions |
| 28 Sort chrome | 5, 18 | Control, label, reset |
| 29 Interaction timestamp | 6 | Updates on listed interactions; create sets timestamp |
| 30 Working-set restore | 15 | Selection, tag filter, sort restored; not search |
| 31 Ticket id in compose | 14 | Ticket pattern → from-ticket import; duplicate/error paths |
| 32 New task | 3 | Opens compose via task-crud hook |
| 33 Keyboard reachability | 18 | All chrome actions reachable by keyboard |

## Assumptions

1. **task-crud** (proposed) owns compose + edit slide-over; this task emits events and hosts action bar until task-crud entities exist (stubs OK for planning→active).
2. **situational-ui** (proposed) owns main split mount; interim Tasks-tab mount is acceptable until split merges.
3. **agent-list-detail** owns agent create/detail and shell sessions; row controls call into shared agent service stubs initially.
4. **Persistence** may follow later; fixtures + in-memory store satisfy plan verification until wired.
5. Provisional shortcut keys: ⌘N New task, `/` search focus, **E** edit (selected row), A/T row badges on selected row — replaced by settings task later.
6. Lifecycle panels/interview routing can stub to toast or placeholder panel until process UI tasks exist.
7. Linear fetch uses existing credential prompt path on missing credentials.
8. Windows manual verification sufficient per project desktop scope.

## Deferred (decision trees)

| Defer | If → then |
|--|--|
| task-crud not ready | Stub compose/edit entities → wire events when task-crud reaches active |
| situational split not ready | Keep Tasks tab mount → move entity to left pane when situational-ui lands |
| persistence not ready | Fixtures + JSON/session store → swap repository without changing pane constructions |
| Shortcut settings unset | Provisional bindings in Step 17 → read from settings when available |

## Verification

Manual on **Windows**: `cargo run` from repo root.

| Group | Checks |
|--|--|
| Pane chrome | Action bar scope; empty vs no-matches |
| Rows | Two-line layout; ticket id; agents/shells labels; tag order; selected accent+outline |
| Navigation | Arrows (ends hold), page/home/end, scroll-into-view, initial selection |
| Filter/search | Stack behavior; clear filters; tag toggle/switch/highlight/chrome |
| Sort | Keys, directions, reset, selection preserved, timestamp default order |
| Row controls | Agents/shells 0 vs menu; lifecycle emits route; digit tag badges |
| Cross-task | New task → compose; ticket id in compose → import; Enter/click → lifecycle next; **E**/title → edit hook |
| Restore | Relaunch restores selection, tag filter, sort (not search) |
| Scale | ~500 rows remain responsive |
| Visual | Matches `artifacts/visual/task-list/preview.png` for in-scope chrome |

Reference: `design.md` Verification section for full walkthrough list.

## Out of scope (this task)

- Browseable Linear issue list
- Project grouping in list UI
- Full lifecycle panel implementations (routing entry only)
- Agent fleet pane (agent-list-detail)
- Automated E2E tests (manual only this phase)
