# Command palette

The palette is tod's keyboard-first way to find commands, items, and named collections. It combines workspace-wide search with the ability to run actions on the current target. [Try the interactive prototype](prototypes/command-palette.html).

## Search and ranking

- Search the whole workspace from every entry point. Label each result as a command, item, or collection and show enough of its location to distinguish duplicate names.
- Group matches by immediate context, broader context such as a project, and Everywhere. Use real context names and omit empty sections. An empty query starts nearby. A clearly stronger query match can bring another section to the top. Close matches preserve section order.
- Match full names, partial words, ordered letters, initials, common aliases, and small typos. Reward word starts and consecutive letters. Match meaningful combinations of the action and its target, such as `pause sync`. For example, `p rn` should find **Pause runner**, and `nmd` should find **Needs my decision**.
- Personalize by how frequently the user executes each command via the palette, menus, or shortcuts. Frequency is a bounded boost for plausible matches and a signal for what to show with an empty query. A strong name match can still outrank a frequently used command. Count executions by command identity, independently of the target.

## Context and targets

**Use context to suggest actions. Use the target shown to the user to decide what an action affects.**

The detail the user is viewing, the current selection, and the object the user invoked an action on can differ. The invocation establishes the target for contextual actions. A palette opened from a selection can act on that selection. A context menu opened on a different item can act on that item while the current detail stays visible. The focused row supplies a single target when navigating a list without an explicit selection.

Show the target on each contextual command before it runs, such as **Pause runner · Fix sync retries** or **Pin tasks · 3 selected tasks**. Make group members easy to inspect. If a command needs a target that is ambiguous, ask the user to choose. Check that the displayed target is still valid before executing, and explain when a change prevents execution.

Commands can act on a target in one part of the workspace and take the user somewhere else. **Pin tasks** can ask for a destination elsewhere in tod. Menus, shortcuts, and the palette use the same command name, target rules, arguments, confirmations, and feedback. A context menu may surface a short list and provide **All actions…** to reach the searchable list.

## Keyboard behavior

| Gesture | Behavior |
| --- | --- |
| Up or Down | Move by one result across section boundaries. |
| Option/Alt + Up or Down | Jump to the first result of the previous or next section. Use the same group movement convention in other grouped lists while rows are being navigated. |
| Enter | Run the highlighted command or open the highlighted item or collection through normal workspace navigation. |
| Shift+Enter | Open **Actions…** for the highlighted item or collection. In a workspace list, open actions for the current target. |
| Escape | Return from Actions to the prior search with its query and highlight restored. Escape from the palette restores the focus that opened it. |

Keep the search field ready for typing during result navigation. When the query changes, highlight the leading result. When results update with the query unchanged, reconcile the highlight by result identity. Provide a visible **Actions…** affordance and show its shortcut.

## Collections and opening behavior

Named collections open in the ordinary collection panel with their own membership, sort, group, and nest rules. **Needs my decision** and **Errors** are examples of collections defined using ordinary configs. **Edit collection** targets the config. An item command targets the named item or selection.

The workspace navigation design will decide where opening an item displays its detail. The prototype uses a detail pane to demonstrate opening, focus, and the distinction between the displayed item and the action target.

## ZGX integration

The [standalone ZGX frontend](../../frontends/tod-zgx/README.md) implements the palette in the redesigned shell. The Rust model owns search, section ordering, command targets, and returning to the prior result. ZGX presents that model and handles native focus and keyboard events.

The default ZGX demo covers named dynamic collections, multiple selected tasks, and mock runner and conversation effects. The optional live build searches saved TOD nodes and outlines and calls the existing runner API for pause and resume. Its detail pane is provisional.

## Prototype walkthrough

- Open the palette with **Cmd/Ctrl+K**. Try `p rn`, `nmd`, and `keyboard shortcuts` from the default task. Clear the query and move between sections with **Option/Alt + Up/Down**.
- Focus a row in **All tasks** and use **Option/Alt + Up/Down** to jump between its project groups.
- Search for **Review retry policy** and press **Shift+Enter**. Run **Pin task**, or press **Escape** to return to the result. Opening the result with **Enter** displays its detail.
- Select multiple tasks with their checkboxes, then try **Pin tasks** or **Open agent conversation**. Right-click an unselected task to try an action on that task while the current detail and selection stay in place.
- Use **Reset demo** to restore mock data and command history.
