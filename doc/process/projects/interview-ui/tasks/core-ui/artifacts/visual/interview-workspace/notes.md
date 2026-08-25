# Interview workspace — Accepted visual

**Status:** Accepted (human screenshot, 2026-08-23)
**preview:** `preview.png`

## Layout (three columns, one workspace)

1. **List (left)** — scrollable open questions; each row = question `id` + short label. Selected row: blue left accent + row highlight. Pending/deactivated rows appear dimmed.
2. **Body (middle)** — full question prose for the selected item (no chrome competing with the answer pane).
3. **Response (right)** — MC options (letter + label; selected option highlighted, letter accented) → free-form **Notes** field → bottom row: action dropdown + primary **Submit**.

## Behavior (from design.md + this Accept)

- MC selection + Notes + Submit on one path; Notes alone or MC alone allowed.
- **Ctrl+Enter** in Notes submits (same as Submit).
- After submit-like actions, **auto-select next** question in the list.
- Action dropdown holds other actions (Consider / Defer / More options / Deep dive) — not MC options.

## Look

Dark charcoal workspace; white body text; blue accent for selection, selected MC letter, and Submit. Compact response column; generous body reading width.
