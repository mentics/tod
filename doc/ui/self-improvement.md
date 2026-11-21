# The self-improvement view

Design: `doc/requests/design.md` 11.4. Loop: `doc/lifecycle/self-improvement.md`.

A dialog opened from the status bar's **Lessons** button, like the incidents
dialog. Code: `crates/tod-ui/src/views/self_improvement.rs`; what it shows is
`tod_core::self_improvement::load`, one store read off the UI thread.

## Lessons tab

An item list (`doc/ui/item-list.md`; columns source, category, status, lesson)
in sections, each a collapsible heading with a count:

| Section | Holds |
|---|---|
| New | lessons no process review has taken in yet |
| Grouped by the process review: *node* | one per run, newest first; the run node is where the drafted change goes through the lifecycle |
| Applied by commit *id* / node *slug* | what was changed because of them (`applied_ref`) |
| Declined | lessons the user will not act on |

A row shows the summary, the user's quote, the target, and the links (node,
request, session, send-back, incident, review node). Dynamic text is selectable.

| Action | Where | Does |
|---|---|---|
| Open node | button, Enter | closes the dialog and shows the node's task panel (`UnifiedView::show_node`) |
| Open review, Open the change | menu / button | the same, for the run node or the node that applied it |
| Decline, Reopen | button, D | `lessons::Op::Decline` / `Op::Reopen` through `fleet.interview("user", ..)`; a reopened lesson is `grouped` again if a run had taken it in, else `new`. An applied lesson cannot be declined |
| Show changes | button, C | for an applied commit: `git show --stat --format=fuller` in the run node's (else the lesson's node's) checkout, in a panel under the list; an unmade checkout or a missing commit is shown as an error |
| Open in editor | menu | `code_editor::open_code_editor_for_node` for that checkout (Zed; a sandbox or dev container opens there) |

Git, the store read and every write run on the background executor; a failure
is shown above the list, never dropped. Each action records a journey
`UserAction` (surface `self_improvement`) on the lesson's node with the row's
actions as the `Presented` snapshot.

## Measures tab

`tod_store::lesson_stats::compute(conn, None).render()`: the text
`tod-cli lessons stats` prints (requests, who settled them, triage rates, time
to answer, cost, incidents, send-backs, lessons, and what is not measured).

Keys: Up/Down/PageUp/PageDown/Home/End move, Left/Right fold a section, M
switches tab, R reloads, Escape closes the dialog.
