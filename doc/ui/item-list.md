# The item list

One list component every list-shaped view in the app uses.

## The rule

**The same item affords the same actions wherever it is shown.** An obligation
in the conversation side pane can be edited, reordered, and deleted exactly as
the one in the Tasks-view panel can, because it is the same obligation. Seeing
familiar data in a second place and finding it inert is the thing this is meant
to end.

So a list is read-only only where the *data* does not permit the action — not
because that particular panel never grew the feature. A capability the delegate
leaves out is a claim about the item, and should be justifiable as one.

## Why

Every list feature the app needs exists somewhere, and no list has all of them.

| | groups | keyboard | selection | editing | row menu | virtualized |
|---|---|---|---|---|---|---|
| Obligations panel (`views/obligations/`) | 3 levels | full | single | inline | — | — |
| Conversation side pane, obligations (`conversation/side_pane.rs:605`) | — | — | — | — | — | — |
| Review findings, conversation side pane (`views/rows/finding_row.rs`) | — | cursor | — | status only | — | — |
| Change set (`conversation/change_set.rs`) | 1 level | cursor | multi | inline | — | — |
| Plan steps, panel and conversation side pane (`views/plan_steps/`) | — | full | single | inline | — | — |
| Command history, agent transcripts, database | — | up/down | single | — | — | — |

The same obligation therefore looks and behaves differently depending on which
panel it is in: the Tasks-view panel groups it, lets the user edit and reorder
it, and moves a cursor through it; the conversation side pane shows a flat
hand-built row with no cursor at all, even though the shared `obligation_row`
it could use is already there and the change set already uses it in compact
mode.

Fixing this list by list means re-deciding, each time, which keys work, whether
there is a context menu, and which component owns focus. Doing it once means
every list gets the full set.

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
`flat_rows` already does:

```rust
enum ItemRow<K> {
    Group { depth: usize, label: SharedString, count: usize, key: K },
    Item  { key: K, render: ..., columns: ... },
}
```

- `depth` is the nesting of the *group*, and is open-ended: a list declares how
  many levels it groups by and the component indents headings accordingly. A
  list with no grouping emits no group rows. Item rows carry no depth — that
  absence is the whole distinction in the scope section above, and it is what
  keeps the type from drifting into a tree.
- `K` is the caller's stable key (obligation id, plan step id, node id). The
  component tracks the cursor, the selection, and which groups are collapsed by
  key, so a store change that reorders rows does not move the cursor.
- Item rows render through `views/rows/` (`obligation_row`, `plan_step_row`,
  `node_row`) or a caller-supplied renderer. The component owns the row
  container — padding, hover, highlight, the `row` / `row-wrapped` styles — and
  the caller owns what is inside it.

**Columns** are declared once per list (body, status badge, kind, trailing
actions) so they align across every row, including across groups.

## What the component owns

- The cursor and every navigation key: `ListArrowUp/Down`, `ListPageUp/Down`,
  `ListHome`, `ListEnd` from `ui/list/keyboard.rs`; Left/Right to collapse and
  expand a group, or to jump to the group heading.
- Selection: single, or multiple with Space and Ctrl+click, as a capability.
- Drag reordering, including the drop indicator and the autoscroll, lifted from
  the obligations panel's `ObligationDragPayload`. Whether a given list accepts
  a drop is the delegate's `reorder`; the drag gesture itself is never
  reimplemented per list.
- Scrolling, keeping the cursor in view, and virtualization. Today only the task
  tree and the transcript are virtualized; every other list renders every row.
- Filter chips (`ui/status_filter.rs`) and Ctrl+F search.
- The right-click row menu — currently only the task tree has one
  (`views/task_list/row_menu.rs`), and `ContextMenuExt` is otherwise used only
  by `selectable_text`. Standard entries (copy, the row's own actions) come
  free; the delegate adds more.
- Navigation mode vs edit mode, per CLAUDE.md: the disabled input, the
  `set_input_tab_stop` removal, Escape to leave, Ctrl+Enter to commit a
  multi-line field and Enter a single-line one.
- Empty state, via `style::empty_message`.

## What the delegate supplies

Capabilities are opt-in at the type level, but per **The rule** above, two lists
over the same kind of item implement the same set. Opting out is for items that
genuinely cannot take the action — a command-history entry is not editable, a
reversed change is not reorderable.

```rust
trait ItemListDelegate {
    type Key;
    fn rows(&self, cx: &App) -> Vec<ItemRow<Self::Key>>;
    // opt-in, each with a default of "not supported":
    fn edit(..);            // inline edit, commit, abandon
    fn create(..);          // below / above / as a child
    fn reorder(..);         // Cmd+Up/Down, drag
    fn delete(..);
    fn context_entries(..); // extra row-menu entries
    fn activate(..);        // Enter
}
```

Where lists genuinely differ, the difference goes here. The component does not
grow a per-list flag for it.

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
5. **The conversation view converts.** It routes Up/Down through its own
   `ConversationUp/Down` (`conversation/keyboard.rs:63`) and hand-rolls a
   `side_cursor`. Once the side pane holds an item list, those keys reach the
   list through its own context while it is focused, and the ad-hoc cursor goes
   away.

## Styling

`doc/ui-style-guide.yaml` defines `row`, `row-wrapped`, `hover-row`,
`highlighted` and the `chunk` / `chunk-header` family for the item row itself.
The list around it is `list-group` and its per-level variants
(`list-group-outer`, `list-group-inner`), plus `list-group-count` and
`list-group-chevron`; each is implemented once in `ui::style` and used only by
the component, so no view sets a heading's colour, weight or indent itself.

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
   is the state and the rows, not the key set. Next the change set, which
   brings multi-select and row buttons in.
5. Move over command history, agent transcripts, and the database view.

Each step is complete on its own; the list of views above is the checklist.

Where a view already exists for the items — obligations, plan steps — a second
list is never built for them. The pane hosts that view embedded. A hand-built
copy is how the two obligations lists drifted apart in the first place.

## Editing inside a conversation

Editing in the side pane does not compete with the change set — it feeds it.
A user edit made there goes through `ConversationEdit`, the path that already
exists for exactly this, so the edit is recorded as the user's own action in
`conversation_actions`, shows up in the change set beside the agent's, and is
reversible the same way. The list does not know any of this: it calls the
delegate, and the conversation's delegate routes the edit.

The same holds for reordering and deletion in a conversation's side pane.

## Open questions

None outstanding.
