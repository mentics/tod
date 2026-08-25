# Visual design — interview workspace Accepted

**Date:** 2026-08-23 1646
**Package:** `artifacts/visual/interview-workspace/`
**Source:** Human-provided screenshot (chat image) → `preview.png`

## Decision

**Accept** the three-column interview workspace as the look-and-work reference for question list + question body + answer controls together.

## Layout locked

- Left: question list (`id` + short label); selected = blue left bar + highlight; pending dimmed
- Middle: full question prose
- Right: MC options → Notes → action dropdown + Submit

Bindings from `design.md` still apply: Ctrl+Enter in Notes; auto-select next after submit-like actions; other actions via dropdown.
