# Collection panel

Working design for a keyboard-first desktop panel. Each collection panel occupies a user-arranged layout slot and contains the config widget, the collection pane, and panel controls. The **collection config** determines which items the pane shows and how they are arranged. A saved config has a name and can be opened in several panels or used by an agent or rule. See the [glossary](../glossary.md) and [UI style guide](../ui-style-guide.yaml).

## REQ-COLLECTION-01 · Edit a collection config

Keep one collapsible config widget above the collection pane. The config name or `<unsaved>` stays visible in the panel header and acts as the disclosure control. Click it or activate it from the keyboard to open the widget and focus its current criterion. A short summary can show the number of criteria or the current arrangement. The collection pane stays visible while the widget is open.

| Control | Interaction |
|---|---|
| **Source** | Choose **Query** or **Manual list**. Start a blank query to browse available items, or start an empty manual list and add items. **Create from selection** starts a manual list using the marked items in another panel. That command makes the new list's starting membership explicit when coming from a query. |
| **Search and filters** | In Query mode, each criterion is a chip with a field, an operator, and a value. Type a clause such as `Tag is Design`, `Text contains keyboard`, or `Agent cost > $review_budget` in a single input. Show field, operator, and value suggestions as the user types. Value suggestions use current data, including configured lifecycle phases and compatible variables. |
| **Add items** | In Manual list mode, search across item types by name, type, or tag. Add by identity, with an indication for items already present. Use **Remove from collection** to change membership and **Move up** or **Move down** to change manual order. |
| **Sort by** | Add an inline chip such as `Sort by Agent cost descending`. Recognize aliases such as `Order by`. Sort peers within each group or relationship branch. A manual list defaults to the user's order. |
| **Group by** | Add an inline chip such as `Group by Item type`. Preview headings immediately, with counts and a **No value** heading for items missing the chosen field. |
| **Nest by** | Add an inline chip such as `Nest by Part of`. Suggest relationships or item-valued fields. Show a real example when available, such as **Query bar appears under Workbench via Part of**. |

Results update as each valid edit is applied. While a filter is unfinished or invalid, show field-level feedback and keep the last valid results visible. Closing the widget keeps the applied changes and returns focus to the config name.

### Edit query criteria in place

Wrap chips as the slot narrows or more criteria are added. Start with a four-row cap, then scroll vertically within the strip. Keep the selected chip in view. Float a plus at the upper right of the strip and replace it with a circle-and-slash while editing. Give both icons accessible names. Clicking a chip selects it and gives it keyboard focus. Clicking the strip's empty area focuses the current chip, or the plus if the strip is empty. Left and Right browse in criterion order. Up and Down move to the nearest chip in the adjacent visual row. Home and End jump to the first and last chips.

Enter replaces the selected chip with an input in the same position and selects its value for editing. As the caret moves, the popup shows choices using the text before the caret in the active field, operator, or value. With the caret at the start of an existing field, the field popup stays closed until there is a prefix. Choosing a field or operator changes that part of the draft while preserving the remaining text for review. Space while browsing starts a new criterion beside the selection. While typing, Space can complete a recognized field or operator, and the input keeps focus for the next word. Up and Down move through suggestions in a popup anchored to that input. Enter applies a complete criterion and focuses its chip. The **Cancel config edit** action discards the draft and returns to the original chip, including when a suggestion is highlighted. Its default is Escape. The circle-and-slash invokes the same action. Delete while browsing removes the selected chip and moves focus to its neighbor. Undo restores the removed chip and its focus.

Bind keys to actions such as **Move left**, **Move down**, and **Cancel config edit**. Components consult the same bindings. The text input preserves ordinary typing, caret movement, and text deletion. F1 opens the keyboard reference with the current bindings. Question mark also opens it outside text inputs.

Arrangement chips use the same editor, autocomplete, and apply, delete, and undo actions as criteria. Match aliases to the same suggestion and display the canonical label after applying. Keep arrangement chips at the end of the strip in **Sort → Group → Nest** order, with one chip per kind. Adding a second chip of the same kind updates the existing one. Preserve focus on the applied chip when it moves.

**Acceptance** — Starting from an empty slot, open a collection panel and activate `<unsaved>` from the keyboard. Build a query with a tag and a typed condition. Save it by name, reopen it in a second slot, and edit a criterion in the first slot. Applying it immediately updates the named definition and both panels.

**Inline editing acceptance** — Browse a ten-criterion query across rows and into the scrollable area. Edit `Item type is Task` and move the caret from the value to the operator to the field. The popup follows the relevant choices. Select an alternative operator and verify the value stays in the draft. Highlight a suggestion, press Escape, and press an arrow to keep browsing. Add `Tag is Design` with one input, then remove a chip and undo the removal without leaving the strip.

## REQ-COLLECTION-02 · Save and manage configs

Put **New collection** and **Open collection** in the panel menu and command palette. Show the active config name in the panel header. Autosave each applied change to a named config, including additions, edits, deletions, and undo. An unfinished criterion remains a draft in its input until applied.

| Panel label | Header activation | Save behavior |
|---|---|---|
| `<unsaved>` | Open or close the config widget | **Save collection** asks for a name and binds the panel to the saved definition. |
| **Design work** | Open or close the config widget | Applied changes save immediately. Other panels using the name pick up the new definition. |

Offer **Save a copy** on a named config to save the current definition under a new name and bind this panel to the copy. **Rename collection** changes the name across references to the config. **Delete collection** identifies the saved definition and where it is known to be in use. Deletion removes that definition from the picker. Open panels retain their working definitions as unsaved collections, and the member items remain available. A saved layout that later tries to open a deleted config offers **Open collection** or **New collection** in that slot.

When closing or replacing a panel with an unsaved config, offer **Save**, **Discard**, and **Cancel**. If the named config changes elsewhere while a criterion is being edited, keep the text in the input and show that the saved definition changed. Let the user review the latest definition before applying a conflicting edit. Show a save failure in the panel so the user can retry. Workspace layout remembers slot placement and saved config bindings. Column widths and expanded groups belong to each panel.

**Acceptance** — Open the same named config in two slots. Resize columns in one, then rename the config and see the new name in both. Save a modified copy and reopen it from the palette. Delete the original and verify that its open panel still shows its items and can save them under a new name.

## REQ-COLLECTION-03 · Browse and act in the collection pane

Use the existing [item-list interaction](../ui/item-list.md#what-the-component-owns) for flat and grouped collections. Relationship nesting adds expandable item rows with the same focus, marking, and action behavior. Put the item count and the marked count above the rows. Keep the item title and the selected type and status indicators readable in a narrow slot. Move optional fields such as tags and lifecycle phase to row details when space is tight. Keep the config widget and item actions reachable through labeled controls or the panel menu.

| Element | Display and interaction |
|---|---|
| **Item type** | The default row layout puts a stable, neutral glyph unique to the item type in a leading position. Expose the type name in accessible row text and on focus or hover when the glyph is shown alone. |
| **Status** | The default row layout gives status its own position with a distinguishable glyph and semantic color. Expose the status label in accessible row text and on focus or hover. Leave the position empty when status is unavailable. Lifecycle phase can appear as a labeled field. |
| **Title and details** | The title opens the item's normal inspector or detail panel. Grouping, nesting, and status leave title color available for the user's own purposes. Show tags as space permits. **Edit tags** shows which tags were added manually or automatically. |
| **Group and branch** | A group heading carries a field value and count. An expandable item row carries an item name and remains actionable. Start with groups on the outside and relationship nesting inside each group. When a parent is outside the group or collection, show the child at that group's top level. |
| **Selection and actions** | Focus identifies the row under the keyboard cursor. Mark an item with Space or Shift-click on its title. Shift+Up or Shift+Down moves to the adjacent visible item, skipping group headings, and marks the inclusive range from the starting cursor row. Reversing direction shrinks that range while preserving prior marks. A subtle leading edge and row tint show each mark, and the row's accessible name includes its marked state. Marks survive collapse. The action menu offers **Focused item**, **Marked items**, **This group**, or **All matches** where applicable, with a count for the chosen scope. A command awaiting confirmation keeps the item IDs it showed when it opened. |

Start with one row per placement when an item has multiple matching parents. Count and mark by item identity so repeated rows represent one target. Stop a cyclic branch before a row would repeat in its own ancestry. If a cycle has no top-level item, start at its first item in sort order so every member can still be reached. Offer a relationship details link from the affected row. A live refresh keeps focus on the same item when it can. If that item leaves, move focus to the next visible row or the results header. Remove marks for items that leave the collection and update the count.

| Results state | Display |
|---|---|
| New manual list | **No items yet** with **Add items**. |
| Query with zero matches | **No matches** with **Edit filters**. Keep the current criteria visible. |
| Saved manual reference is unavailable | Show the unavailable entry in the config editor so it can be removed from membership. |
| Evaluation failed | Show the error and **Retry**. Label any retained results as stale and pause bulk actions until evaluation succeeds. |

**Keyboard.** Tab and Shift+Tab enter and leave the collection pane as a single stop. On entry, show the current row cursor and let the next arrow key move it. Expose the active row to assistive technology. Use Up and Down to browse, Space to mark an item, and Enter to open it. Left collapses the focused group or branch if it is open. Otherwise, Left moves to the row’s visible parent if one exists. Right expands a collapsed group or branch and keeps the cursor on it. Page Up and Page Down move the row cursor by approximately one visible pane height and keep the destination in view. From a row, `/` opens the config widget and focuses its current chip. In the widget, Tab moves among controls and Enter commits the current choice. Escape first cancels an active chip edit, even after Tab moves focus to another control in the panel. Escape while browsing chips returns focus to the config name. With a text field focused, typing keys retain their text-editing behavior. Use the same commands in menus and the command palette.

**Acceptance** — In a mixed collection, distinguish a task and a requirement by type. A task can show **Blocked** alongside a **Review** phase. Collapse a group containing a mark and verify that the selected count is stable. Mark an item appearing under two parents and confirm that the bulk tag action targets it once. Complete the flow by keyboard.

## REQ-COLLECTION-04 · Configure row display

Let the user choose which fields appear in collection rows and control their display order. Start with a default layout that includes the item title, type, and status. Retain the title as the entry point to the item. Offer fields such as lifecycle phase, tags, and agent cost where available. In a mixed collection, a field can be empty for some item types while its column stays aligned across rows.

Preview row display changes in the pane. The collection retains the same membership, arrangement, focused item, and marks when presentation changes. Make the display controls reachable by keyboard. Keep room in the design for user-chosen row controls as well as field values.

**Proposed interaction.** Add one `Show` chip in the existing config strip, before the arrangement chips. Typing `show` or `columns` in the inline editor suggests it. For example, `Show Item type, Agent cost, Lifecycle phase` displays those fields after the title, in that order. Autocomplete offers fields by name, uses the prefix of the field at the caret, and skips fields already listed elsewhere in the chip. Comma fills the highlighted or uniquely matched field and advances to the next field. Keep unresolved text in place so the user can continue listing fields. Enter applies a complete list or returns to the first unresolved field with an explanation. Escape cancels the edit. Removing the chip restores the default fields.

**Proposed persistence.** Save the chosen fields and their order with the collection config. Keep column widths specific to each panel. Test whether users need different field lists in two panels that share a saved collection before adding an override.

**Acceptance** — In **Design work**, edit `Show Item type, Status, Lifecycle phase` to `Show Item type, Agent cost, Lifecycle phase` using the keyboard. A task shows its cost and phase in that order. A requirement with no cost keeps its place and the same column alignment. The focused item and marked count stay stable.

**Open** — Check whether editing the field order inline is efficient with a long list. Revisit which row controls can come from item capabilities after the capability and tag model is settled.

## Design walkthrough

In the prototype, open **Design work** beside **Component breakdown**. **Design work** groups the design-tagged items by type and sorts peers by name. **Component breakdown** uses the same items, nests them by **Part of**, and sorts siblings by agent cost. The collection panel component appears above the tasks and requirements linked to it. Change **Group by**, **Nest by**, or **Sort by** in either panel to see how the settings combine.

Open an unsaved query, filter for **Design**, and group by **Type**. The results contain a task named **Verify keyboard navigation**, a product component named **Collection panel**, and a requirement named **Keep focus visible**. The task's status is **Blocked** and its lifecycle phase is **Review**. Edit the query to exclude tasks, see the task disappear, and save **Design definitions**. Open it in another slot to see the two definitions again. From that panel, mark both and choose **Create from selection** to start a manual review packet. A new matching design requirement can enter **Design definitions** while the packet still contains the two chosen items.

## Open design questions

- Which statuses are useful across item types? **Blocked** is an example for the walkthrough. Decide whether status is set by a user, derived from signals such as dependencies, or supplied by each item type. Decide whether a row summarizes more than one condition.
- If the app restores a workspace after restart, should it also restore unsaved config drafts? The initial interaction keeps drafts while their panel is open and prompts when the user closes or replaces it.
