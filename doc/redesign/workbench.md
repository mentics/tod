# Workbench

Start with a collection panel, an item inspector, and a conversation panel. Add, duplicate, pin, resize, or move panels through commands. A collection panel can have its own config and selection. A detail or chat panel can follow that selection or stay pinned to a task, product item, session, or named collection. A saved layout restores panel bindings as well as their positions. Luau in the planned ZGX direction could use the same commands for layout and rule customization.

| Component | Responsibility |
|---|---|
| Command palette | Find items or collections and invoke commands for the active panel or selected items |
| Config widget | Edit criteria and arrangement in one inline chip strip with autocomplete |
| Collection pane | Browse and mark items. Show the scope of a bulk action before it runs |
| Inspector | Show the selected item's relevant fields, relationships, effective obligations, history, and actions |
| Conversation panel | Talk to an agent with a visible target and render structured results alongside a short reply |
| Attention queue | Move through human decisions, failed or blocked work, and metric alerts |
| Activity and changes | Trace agent and user actions, inspect evidence, and reverse eligible changes |

Every component follows [CON-KEYBOARD-01](product-definitions.md#con-keyboard-01--operate-every-component-by-keyboard). A command behaves consistently whether invoked from the palette, a menu, or a shortcut. Marked rows and the cursor are distinct. A chat draft stays on its original target while the user browses elsewhere.

## REQ-QUERY-01 · Compose and reuse a query

A user can combine text, item type, tags, typed dates and numbers, and relationship criteria. The same query controls support tasks and product items. Criteria have explicit meanings, such as **Completed last month**, **Every Wednesday**, and **Agent cost > $10**. A named collection config preserves query and arrangement, and results update as data changes. [Collection panel details](collection-panel.md)

**Acceptance** — From the keyboard, find design tasks completed last month that cost more than $10 and mention export. Edit the threshold, group by lifecycle, save the config, and reopen it from the command palette. Select one row, a group, or all matches and see the exact scope before applying a tag.

## REQ-CHAT-01 · Converse and act in the same interface

The composer takes ordinary conversation. A turn can return a short text reply alongside structured decisions, proposed tags, findings, changes, or evidence. A decision can be answered from its card in chat, the task inspector, or the attention queue. Each representation opens the same evidence and shows the same resolved state. The user can inspect the full transcript and can tell a suggested change from a recorded change.

**Acceptance** — Start a chat from a task, switch the results selection without moving an open draft, and answer a structured decision with a keyboard shortcut. The updated decision appears in the inspector and the conversation.

See [Define and assign lifecycles](lifecycles.md#req-lifecycle-01--define-and-assign-lifecycles) for workflow authoring and execution.
