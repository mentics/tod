# Tod shell redesign

Tod opens into a window of user-configured slots. The shell owns layout, keyboard focus, window management, saved views, global settings, and overlays. Each panel owns its content, optional title, and actions. The window uses a compact toolbar for global actions. A compact Layout menu holds slot-specific commands. A status bar can be added if there is information users need to see continuously.

## Slots, panels, and views

- A **slot** is a region of the window. It holds one panel or offers a panel picker when empty.
- A **panel** shows content such as a named collection configuration, agent activity, or notes. A configuration can appear in several slots or windows, with independent selection, scroll position, and other local interaction state.
- A **view** is a named layout for one window. It stores the split arrangement, sizes, panel assignments, and detail targets. Each window can load any saved view. Saving writes an explicit snapshot. Subsequent edits apply to the open window until saved again.

The starter view in the mockup is an example configuration. Every slot follows the same rules.

## Keyboard and layout

Use Command-Option and the arrow keys to focus a neighboring slot within the active window. Settings also offers Control-arrow when macOS leaves it available. Option-Left and Option-Right are reserved for Tod's Back and Forward navigation. At an uneven split, choose the slot that best lines up with the current one. A high-contrast border identifies the slot receiving input. The border becomes inactive when the window loses focus. Panels retain their own control-level focus cues.

Open the command palette with Command-K. **Set Panel** searches collections and other panel types and applies the selection to the focused slot. It is available during everyday use and layout editing. Replacing one assignment leaves other assignments intact.

Open the **Layout** menu from the window header, or run its commands directly from the palette. The focused slot can be split on either axis, resized, or removed. A split preserves the source and focuses its new empty neighbor. Removing a slot expands its sibling. Drag any divider to resize adjacent regions, including nested splits. The arrangement updates as you drag. **Undo Layout Change** restores the arrangement before the latest change.

Use Command-Option-Shift and an arrow to push the focused slot's edge in that direction. The whole shared divider moves and highlights, while focus stays in the slot. Hold the arrow to repeat until the adjacent slots reach their minimum size. Window edges stay fixed. One hold counts as one undoable change. To push the divider back, focus a slot on the other side or use **Resize Slot…** to shrink from the current slot.

## Where details open

A panel requests a presentation through the shell. The request identifies the panel to show and carries its instance data, such as the selected collection item. The shell supplies the originating slot and resolves the destination within its window.

Use **Set Detail Target…** from the Layout menu or command palette on a source slot to pick another slot or choose **Floating Panel**. The picker outlines the candidate slot as you browse. The route belongs to the source slot and applies to the panels assigned there. Removing either slot clears the route. Undo restores the previous routes.

A detail opens in the target slot when one is configured. It temporarily covers the assigned panel, which returns when the detail closes. A subsequent request to the same target replaces the current detail. Without a target, the shell opens a dismissible floating panel in the same window. Opening a detail moves keyboard focus to it. Closing it from there returns focus to the source. Open details are transient and saved views capture only the routing.

In the mockup, use arrow keys to select a collection row and Return to view details, or click **View Details** after selecting a row. Escape closes the detail.

## Windows and overlays

**New Window** starts with one empty slot. **Pop Out Panel** opens a window with the source panel's configuration, leaving the source assignment intact. The new window can be split, edited, and saved like any other window. Window switching follows platform conventions. Whole-session restoration is a separate decision.

The palette, pickers, and confirmations return focus predictably when dismissed. Toasts briefly acknowledge completed actions without taking keyboard focus. Errors that require a response stay visible until acted on or dismissed.

## Interactive mockup

The standalone macOS prototype uses mock data and its own local preferences. It remembers saved views and the main window's latest applied arrangement. Other windows can save their own views. The production shell can be developed alongside Tod's existing UI and connected to shared services when the interaction model is ready.

From the repository root, run:

```sh
./prototypes/shell/run.sh
```
