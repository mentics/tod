# Interactive prototype

The native ZGX frontend exercises a first slice of the collection panel spec alongside the command palette, including browsing the panel's own requirements as items. The walkthroughs below cover the broader workbench. [Native frontend](../../frontends/tod-zgx/README.md#collection-panels) · [Collection panel spec](collection-panel.md)

## WORK-QUERY-PROTOTYPE · Exercise the query bar

Create a runnable desktop-style prototype with mock data. This work item exercises [REQ-QUERY-01](workbench.md#req-query-01--compose-and-reuse-a-query) and [CON-KEYBOARD-01](product-definitions.md#con-keyboard-01--operate-every-component-by-keyboard). It also covers a product item linked to its implementation work and the source of an applied constraint.

The prototype uses local fixture data and a scripted agent turn. Searches, changes to criteria, selection, navigation, saving a view, defining a relationship, and answering a decision should update the demo state. Include a reset command so the same walkthrough can be repeated.

## Fixture data

- **Product items** — Query bar, Command palette, Results panel, and Conversation panel, with enough detail to inspect their behavior and linked work.
- **Obligations** — Query bar's typed-query requirement and a **Keyboard-first** tag that applies the keyboard constraint. Include a component without the tag so the user can preview adding it.
- **Work items** — A set of design and implementation tasks spanning different lifecycles, dates, and agent costs. Include design tasks above and below the cost threshold, tasks outside the date range, unfinished tasks, and otherwise matching tasks without the text term so each criterion can be tested in isolation.
- **Conversation** — A scripted reply asks which cost the query should display, with two options and a link to evidence. Answering updates the request in both the chat and inspector.

## Provisional keyboard controls

| Control | Action |
|---|---|
| `Cmd/Ctrl+K` | Open the command palette |
| `/` | Focus the query bar from the results panel |
| `↑` / `↓` | Move within results or suggestions |
| `Space` | Mark the focused row when results own focus |
| `Enter` | Open the focused result or commit an editor choice |
| `Tab` / `Shift+Tab` | Navigate controls |
| `Esc` | Close the active menu or cancel the edit and restore focus |

Expose panel navigation, opening chat, and bulk actions through the palette. Label shortcuts wherever an action is discoverable. When editing text, `/`, arrow keys, and `Space` retain their text-editing behavior.

## Walkthroughs

1. **Find and act on work.** Open the palette, choose the tasks view, and filter for design work completed last month with agent cost above $10 and text matching `export`. Group the results by lifecycle, mark two rows, and preview a bulk tag. Save the view and reopen it. Open one task and answer the scripted agent decision.
2. **Follow a product requirement into work.** Open **Query bar** in the inspector. Inspect the source of its keyboard constraint. Define `Shares interaction with` and link it to **Command palette**. Follow the existing `Exercised by` link to the prototype task and back. Browse another row while keeping a chat pinned to **Query bar**.

The prototype is ready for feedback when both paths work entirely from the keyboard and the visible result, focus, selection, and agent context update as expected after each action.
