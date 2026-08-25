# Core UI

Project: `doc/process/projects/interview-ui/`

## Goal

Implement the full Interview UI product — every requirement in project `user.md` is in scope for this task.

## Requirements

1. Full project scope — Deliver all requirements numbered 1–23 in `doc/process/projects/interview-ui/user.md` (including selection visibility, single-line list rows, workspace keyboard/pointer navigation, Escape, MC digit submit, response column layout, and Complete-only-when-queue-empty). The task is complete only when each passes its statement and success-criteria checks (where present).
2. Agent and protocol changes — This task includes (1) researcher agent implementation changes, (2) answer-processor agent implementation changes, and (3) on-disk interview file format changes (queue, config, and transcript layouts). **MC digit keys:** update researcher (and related) skills/prompts/protocol examples so agent-authored options use `"1"`, `"2"`, `"3"`, … only — no letter keys.
3. Interview process skills — This task may change interview process skills. Do not change state agent definitions or bootstrap conventions in this task.
4. Verification — Manually complete one real process interview end-to-end using only this Interview UI; the interview must reach the interview-complete state.
5. Independent review F1–F6 — Fix findings F1–F6 from `.local/agent/process/projects/interview-ui/tasks/core-ui/journal/2026-08-25-review-independent.md` in this same redesign→implement cycle if not already fixed (in scope; planning/active work items).

## Constraints

1. Interview views only — Do not rework or extend the GPUI app shell or task-list UI owned by prior tasks; this task adds interview views and functionality only.
2. Build on ui-scaffolding — Integrate Interview UI into the existing tod GPUI desktop app delivered by the completed ui-scaffolding task; do not create a new standalone shell or web app.
