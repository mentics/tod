# The item list

One list component every list-shaped view in the app uses.

## The rule

**The same item affords the same actions wherever it is shown.** An obligation
in the conversation side pane can be edited, reordered, and deleted exactly as
the one in the Tasks-view panel can, because it is the same obligation. Seeing
familiar data in a second place and finding it inert is the thing this is meant
to end.

So a list is read-only only where the *data* does not permit the action — not
because that particular panel never grew the feature. A capability a list does
not configure is a claim about the item, and should be justifiable as one.

## Why

Every list feature the app needs existed somewhere, and no list had all of
them. This is how it stood when the migration started:

| | groups | keyboard | selection | editing | row menu | virtualized |
|---|---|---|---|---|---|---|
| Obligations panel (`views/obligations/`) | 3 levels | full | single | inline | — | — |
| Conversation side pane, obligations | — | — | — | — | — | — |
| Review findings, conversation side pane | — | cursor | — | status only | — | — |
| Change set (`conversation/change_set.rs`) | 2 levels | full | multi | inline | — | — |
| Plan steps, panel and conversation side pane | — | full | single | inline | — | — |
| Command history, database results, agent transcripts | — | up/down | single | — | — | — |

The same obligation therefore looked and behaved differently depending on
which panel it was in: the Tasks-view panel grouped it, let the user edit and
reorder it, and moved a cursor through it; the conversation side pane showed a
flat hand-built row with no cursor at all, even though the shared
`obligation_row` it could use was already there and the change set already
used it in compact mode.

Fixing this list by list means re-deciding, each time, which keys work, whether
there is a context menu, and which component owns focus. Doing it once means
every list gets the full set. Every list in the table is now the one component,
so the row above is history, not a to-do list.

## Scope

**A list, not a tree.** The line is not how deep the grouping goes — it is
whether the *content rows* nest.

In the item list they do not. Inside any group, however deeply nested that group
is, there is one flat run of item rows, and an item never owns another item. A
group is a heading over a run; it is not itself an item. Grouping can go as many
levels as a list needs (obligations need two: kind, then section) and the thing
is still a list, because adding a level changes how items are sorted into piles,
not what an item is.

In the node tree the content rows themselves nest: a node owns nodes, to any
depth, and that is what indent/outdent, drag reparenting, and subtree collapse
all act on. That is a different thing, not a more-grouped list.

**The node tree is out of scope.** It is deliberately a tree of any depth, with
indent/outdent, drag reparenting, and a row menu of its own. It keeps
`ui/list/ListView` and its own delegate. Anything the item list grows that the
tree also wants (the context menu, the focus discipline below) is lifted into a
shared helper rather than forced through one component.

**The transcript is out of scope.** `ui/transcript_list.rs` is a chat log, not a
list of items.

## The model

The caller flattens its data into rows, as `views/obligations/mod.rs`'s
`flat_rows` does:

```rust
enum ItemListRow<T, G = ()> {
    Group { spec: GroupSpec, group: G },
    Item  { key: String, item: T },
}
```

Both carry the caller's own payload — `T` for an item, `G` for a group — so a
view reads a row's meaning off the row instead of parsing its key back. A list
that groups by one thing, or not at all, leaves `G` as `()`.

- `depth` is the nesting of the *group*, and is open-ended: a list declares how
  many levels it groups by and the component indents headings accordingly. A
  list with no grouping emits no group rows. Item rows carry no depth — that
  absence is the whole distinction in the scope section above, and it is what
  keeps the type from drifting into a tree.
- The key is the caller's stable one (obligation id, plan step id, session id).
  The component tracks the cursor, the marks, and which groups are collapsed by
  key, so a store change that reorders rows does not move the cursor and a
  collapsed group does not lose a mark.
- Item rows render through `views/rows/` (`obligation_row`, `plan_step_row`,
  `node_row`) or a caller-supplied renderer. The component owns the row
  container — padding, hover, highlight, the `row` / `row-wrapped` styles — and
  the caller owns what is inside it.

## Columns

A list that is a table declares its columns once, as `ColumnSpec`s, and the
component does the rest. Nothing else sets a width, which is what keeps the
columns aligned: a row asks for a column by key (`state.column("status", el)`)
and gets a cell of the declared width.

- **Fixed or content.** A fixed column holds the same width on every row.
  Exactly one column — the content column — has no width and takes what the
  fixed ones leave.
- **A column is for a value every row has.** A plan step always has a status,
  a finding always has a severity. Something only some rows carry (an
  obligation's standing, which only exists during verification) stays inside
  the content column as trailing context, where the empty case costs nothing.
- **A grouping is never a column.** A group heading is a band over a run of
  items, as it always was; its label starts where the content column does, so
  it lines up with the text beneath it and the fixed columns stay empty
  across it.
- **A header names the columns** — above the rows, outside the scrolling area,
  so it does not scroll away. A list that declares no columns has no header
  and is a plain list, which is what the change set wants.
- **A list whose columns are its data declares them again when the data
  changes** (`ItemList::set_columns`). The database view's columns are
  whatever the query returned, so they are not known when the view is
  written; a column's key and label are therefore owned strings, not
  `&'static str`.

Today: findings are `severity | answer | finding`, a plan is `# | status |
step`, command history is `time | change`, the agent-transcripts session list
is `time | agent`, and a query result is its own columns. Obligations and the
change set declare none.

## What the component owns

Today, in `ui/item_list/`:

- **The cursor**, tracked by key, and the navigation keys — up/down, page
  up/down, home/end, and Left/Right to collapse a group or jump to its
  heading. `keyboard::bind_item_list_keys(cx, surface, keys)` registers them
  under the surface's own key context; the flags on `ItemListKeys` say which
  of editing, creation, reordering, marking, grouping and search that surface
  also binds.
- **Group headings**: the band, the indent per level, the chevron, the count,
  the collapsed set (by key, so a rename or a reorder does not expand
  everything), and the rename field.
- **Marking**, with `with_marking()`: the checkbox gutter, Space on the row
  under the cursor, and `selection()`, which falls back to the cursor so an
  action can mean "this one" without a marking step. Marks are held by key and
  survive a row being hidden by a collapsed group.
- **Columns**, with `with_columns()` / `set_columns()`: the declared widths,
  the header row, and the heading offset that keeps a grouping from reading as
  a column. See above.
- **Scrolling** and the scrollbar, and keeping the cursor in view.
- **The right-click row menu** (`ui/item_list/row_menu.rs`): the gesture, the
  anchoring at the pointer, the chrome, and moving the cursor onto the row
  that was clicked. It is the same `ContextMenuExt` `selectable_text` uses,
  not a second implementation. A list declares what an item affords
  (`with_row_actions`) and what its text is (`with_row_text`); it never builds
  a menu, and one that declares neither has none.

  **The row's own actions come free.** One `Vec<RowAction>` feeds the hover
  buttons and the menu both, so a list cannot let the two drift apart; an
  action there is no room to show as a button is `RowAction::menu_only()`.
  Copy is standard, and copies the drag selection if there is one, else the
  whole row — the component cannot read a row's text off the row, since `T`
  is the caller's payload and the rendered row an opaque element, so a list
  that wants Copy says what its rows say.

  Where the row carries a menu, the row's text gives up its own Copy menu
  (`RowOptions.menu_hosted`). One right-click hovers both hitboxes, so
  otherwise two menus open stacked on each other; the row's is the one to
  keep, because it offers what the row affords as well as Copy.

What it does *not* own yet, and where that work lives instead:

- **Drag reordering.** `with_drag()` hands the caller the row the component
  built and takes back a draggable one, so the payload stays the caller's —
  but the drop indicator and the autoscroll are still the obligations panel's,
  and only that panel drags. Lifting them is the next thing worth doing, since
  it is the one capability a list can have that another list over the same
  items does not.
- **Search.** `item_list::search::matches_query` is a shared matcher, called by
  the obligations panel. There is no component-owned Ctrl+F.
- **Filter chips.** `ui/status_filter.rs` is the view's; the component never
  sees a filter, it only sees whatever rows survived one.
- **The empty state.** Each view still renders its own `style::empty_message`.
- **Virtualization.** Every list renders every row. Only the task tree and the
  transcript — both out of scope — virtualize.

## What the caller supplies

There is no delegate trait. The component is a plain struct the view owns, and
a view configures it with builder hooks (`with_marking`, `with_drag`,
`with_columns`, `with_group_editor`) and drives it with method calls. A hook
left off is a capability the list does not have.

The caller supplies three things:

1. **The rows** — it flattens its data into `ItemListRow`s each render, and
   the component reconciles the cursor and the marks by key.
2. **The item renderer** — a closure the component calls per item row, given
   an `ItemRowState` (its index, its key, whether it is highlighted, marked or
   being edited, and the list's columns). Item rows render through
   `views/rows/` so the same item looks the same everywhere.
3. **What an item affords** — `with_row_actions` returns the item's
   `RowAction`s, which become both its hover buttons and its menu entries.
   There is no separate "extra menu entries" hook: an entry that is only in
   the menu is a `menu_only()` action.
4. **What the keys mean** — the component reports what the user did as
   `ItemListEvent`s through a `RowHost`, and the view converts them into its
   own action type and carries them out. This is why a conversation's edit can
   route through `ConversationEdit` while the same list outside a conversation
   writes a plain outline mutation: the list does not know the difference.

Per **The rule** above, two lists over the same kind of item configure the same
set. Opting out is for items that genuinely cannot take the action — a
command-history entry is not editable, an agent session is a record of
something that already happened.

## Keyboard focus

This is the part to get right, since it is what is inconsistent today.

1. **One list owns the keyboard at a time.** A list reads keys only when its own
   focus handle is focused. No list binds a key that fires while another panel
   is focused.
2. **Key context per instance.** Each list is constructed with a context name and
   registers its bindings under `key_context::excluding_input(name)`, so a
   focused text field never swallows a navigation key and a navigation key never
   reaches a field. Handlers that must fire inside an input (Escape,
   Ctrl+Enter, a single-line Enter) use `key_context::including_input`.
3. **Plain arrows belong to the list; Ctrl+arrows cross panels.** A list that
   uses Left/Right for groups calls `bind_modified_pane_nav`; one that does not
   calls `bind_pane_nav`. Ctrl+Left/Right work on every multi-column surface
   either way.
4. **The host routes, the list does not.** A list emits a focus event (as the
   Tasks drawers emit `FocusTaskList`) and the shell moves focus. The list never
   reaches for another panel's handle.
5. **The conversation view converts.** It still routes Up/Down through its own
   `ConversationUp/Down` (`conversation/keyboard.rs`), because the side pane is
   one of several panes it moves between and the key has to mean different
   things in each. What went away is the *state*: there is no `side_cursor` any
   more. The view drives the pane's `ItemList`, and the list is the cursor, the
   marks and the rows. A view may own the key set; no view owns a cursor.

## Styling

`doc/ui-style-guide.yaml` defines `row`, `row-wrapped`, `hover-row`,
`highlighted` and the `chunk` / `chunk-header` family for the item row itself.
The list around it is `list-group` and its per-level variants
(`list-group-outer`, `list-group-inner`), plus `list-group-count` and
`list-group-chevron`; each is implemented once in `ui::style` and used only by
the component, so no view sets a heading's colour, weight or indent itself.

The table styles are `list-cell` (one column's cell: a declared width, or the
fill for the content column), `list-header` (the column names) and
`list-mark-gutter` (where the selection checkbox sits). The last two are
widths a heading has to be offset by, which is why they are named rather than
inlined: `list_group` takes the lead offset and every list computes it the
same way.

## Migration

1. **Done.** Extract the component from the obligations panel, the most
   complete list, so nothing is lost. The panel keeps every key it has today.
2. **Done.** Point the conversation side pane's obligations list at it. It
   hosts the real `ObligationsView` embedded, the way the context panel
   already did, rather than growing a second implementation of the same rows —
   this is the inconsistency that started this, and step 2 is where **The
   rule** is first actually true of something. The standing each side-pane row
   carried moved onto the shared obligation row, so the node tree shows it too.
3. **Done.** Move plan steps onto the component, and have the side pane's
   plan list host that view. The status dropdown became a row action — the
   chip on the shared plan-step row opens it, in both lists, and `t` opens it
   from the keyboard where the panel used to cycle status blindly. What only a
   conversation knows about a step — a handoff's answers, what verification
   found — reaches the row through a host hook rather than moving into the
   shared row.
4. **Done.** Move the review findings onto the component. A finding is now a
   shared row (`views/rows/finding_row.rs`), so it looks and answers the same
   wherever it is shown, and the pane's hand-rolled `side_cursor` is gone. The
   conversation view keeps its own Up/Down/Enter dispatch and drives the list
   with it, since the pane is one of several the view routes between; the list
   is the state and the rows, not the key set.
5. **Done.** Move the change set onto the component. It is the first list to
   use `with_marking()`: Space and the row checkbox mark, and "Reverse
   selected" works on `ItemList::selection`, which falls back to the cursor so
   R reverses "this one" without a marking step. Its node headings became real
   group headings, and the "Plan" label a group nested inside one, so both
   collapse — which also makes them cursor stops, as they are in every other
   list. The change set declares **no columns**: its rows are nodes,
   obligations, plan steps and capability changes, and the one thing they all
   have is the operation, which is a leading icon rather than a value to line
   up. A row's own disclosure — showing a change in full with its
   field-by-field detail — stays the view's, since it is about the row's
   content, not its place in the list.
6. **Done.** Move over command history and the database view, the two
   read-only lists. Both get the cursor by key, click to select, scrolling
   and the navigation keys; neither takes editing, creation, reordering,
   marking or search, because a change that already happened and a row of a
   query result are not things the user can change here.

   **Command history is `time | change`** — a table, since every entry has
   both, and the times now line up in a fixed column under a header instead
   of floating at the right edge of each row. Clicking a row now *selects*
   it, where it used to undo through it on the first click; Enter and Ctrl+Z
   undo, as they already did, and the footer says so.

   **A query result is a table of its own columns**, one per column the query
   returned, keyed by position so `select a.id, b.id` still lines up. The
   last is the content column, since no column of a result means more than
   another. The results are the last of the view's focus stops: Down past Run
   moves into the rows, Up off the first row hands the keyboard back.

7. **Done.** Move over the agent-transcripts sidebar — the last hand-rolled
   list. The transcript beside it stays out of scope
   (`ui/transcript_list.rs`): it is a chat log, not a list of items.

   **The session list is `time | agent`**, and it **groups by the day a
   session was last active** — "Today", "Yesterday", then the date. The
   sessions already arrive newest first, so a day is one contiguous run and
   the grouping reorders nothing; it also earns the time column, which now
   says only the time of day because the heading above it says which day.
   What a session *ran on* — its platform, and how much traffic was logged
   for it — is trailing context inside the content column, not a column,
   because traffic logged under a key no session was recorded for has no
   platform to show.

   The list takes navigation and nothing else: an agent session is a record
   of what already happened, so there is nothing to edit, create, reorder or
   mark. The cursor is the selection — moving it shows that session's
   transcript, and resting on a day heading leaves the transcript as it was.
   The 1–9 number badges now number the rows on screen, so a collapsed day
   does not leave a badge pointing at something hidden.

Each step is complete on its own; the list of views above is the checklist,
and every step on it is done.

Where a view already exists for the items — obligations, plan steps — a second
list is never built for them. The pane hosts that view embedded. A hand-built
copy is how the two obligations lists drifted apart in the first place.

## Editing inside a conversation

Editing in the side pane does not compete with the change set — it feeds it.
A user edit made there goes through `ConversationEdit`, the path that already
exists for exactly this, so the edit is recorded as the user's own action in
`conversation_actions`, shows up in the change set beside the agent's, and is
reversible the same way. The list does not know any of this: it reports what
the user did, and the view routes it.

The plan list is where this is concrete. `PlanStepsView` holds one
`MutationRouter` — an optional closure wrapping an `OutlineMutation` into an
`InterviewCommand` — and every mutation it makes goes through the one `apply`
that consults it. Hosted in a conversation, the router wraps each edit as a
`ConversationEdit`; hosted in the Tasks view, there is no router and the edit
is a plain queued outline mutation. One choke point, set once, rather than a
conversation-aware branch at each call site.

The same holds for reordering and deletion in a conversation's side pane.

## Open questions

None outstanding.
