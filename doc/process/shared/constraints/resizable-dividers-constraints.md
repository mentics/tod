# Resizable dividers constraints

Reusable UI practice for desktop applications. Projects that adopt this document treat it as a binding constraint.

## Rule

Any **divider line** that separates two panels or two sections of flexibly sized content must be **draggable** so the user can resize the adjacent areas.

This applies to:

- **Vertical dividers** between side-by-side panels or columns (for example a list beside a detail pane, or a three-column workspace).
- **Horizontal dividers** between stacked sections when both sections have flexible size and resizing would meaningfully change how much content each area shows (for example a transcript above a command input, or a top pane above a bottom pane).

Dragging a divider reallocates space between the adjacent regions; the layout must update live while dragging.

## Exemptions

A divider does **not** need to be draggable when:

- It is purely decorative or separates fixed-size chrome from content (for example a title bar, toolbar, or status strip with a fixed height).
- One side is fixed-size by design and resizing would not give the user meaningful control (for example a narrow icon rail with no variable content).
- The separated regions are not panels or sections in the layout sense (for example a border around a card, or a table grid line).

When in doubt, if both sides show user content that could benefit from more or less space, the divider should be draggable.

## Expectations

- Provide a clear drag affordance on hover (for example a resize cursor) so dividers are discoverable without documentation.
- Enforce sensible minimum sizes so panels do not collapse to unusable slivers during or after a drag.
- Divider drag must not break keyboard navigation, text selection, or primary interactions in either adjacent panel.
- Where layout proportions are persisted (application settings, session state, or similar), restored sizes should respect the same minimum-size rules.
