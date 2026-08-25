# Interview workspace — Accepted visual

**Status:** Accepted (human screenshot, 2026-08-23)
**preview:** `preview.png`

## Layout (three columns, one workspace)

1. **List (left)** — scrollable open questions; each row = **single line** `id` + short label (ellipsis OK; do not stack id above label). Selected row: blue left accent + row highlight **clearly distinguishable** from background. Pending/deactivated rows appear dimmed.
2. **Body (middle)** — full question prose for the selected item (no chrome competing with the answer pane).
3. **Response (right)** — MC options (**digit** + label; wrap labels, **no ellipsis truncation**) → free-form **Notes** field → bottom row: action dropdown + primary **Submit**. Response column fills available horizontal space / grows with window (supersedes “compact fixed” where it clipped).

## Behavior (from design.md + this Accept)

- MC digit-key / click / Space-Enter-on-focus **submit** immediately; Notes alone also allowed; **Ctrl+Enter** while editing Notes submits.
- After submit-like actions, **auto-select next** question in the list.
- Action dropdown holds other actions (Consider / Defer / More options / Deep dive) — not MC options.
- Focus model / Escape / edit mode: see project `user.md` §21–22 and `design.md` (post–2026-08-25 Accept).

## Look

Dark charcoal workspace; white body text; blue accent for selection, selected MC digit, and Submit. Flexing response column (no MC truncation); generous body reading width.
