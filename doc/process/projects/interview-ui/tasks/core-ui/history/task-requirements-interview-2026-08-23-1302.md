# Task requirements interview — core-ui — 2026-08-23

## Session

**Agent:** Proposed agent bootstrapped task `core-ui` requirements interview. Lifecycle state `proposed`, mode `interactive`. Human invoked `/process` to create a new task under project `interview-ui`.

**Prior context:**

- Project `interview-ui` is **active** — project-defining interview complete 2026-08-23; see `doc/process/projects/interview-ui/user.md` for cross-cutting requirements.
- Product ideas in `.local/doc/interview-ui/ideas.md` and project entity `to-process.md`.
- `refs/process/todo.md` — next work: dedicated UI listing `scratchpad/interviews/{session-id}/queue/` (one file per question), parallel answer submission to answer-processor, researcher replenishes queue; no interview orchestrator (two sessions only). Implement the UI next.
- Prior tod task **ui-scaffolding** (done): runnable GPUI desktop shell.
- Prior tod task **task-list** (in progress/done): task list UI — explicitly **not** app-shell, **not** interview-ui-slice.

**Scoping to resolve in this interview:**

- Which project requirements belong in this first task slice vs deferred tasks
- MVP scope for v1 implementation
- Verification bar and out-of-scope boundaries

---

## Scope directive (applies to q-001–q-006, q-008)

**Human:** For this task we want to implement everything currently defined in the interview-ui project.

**Interpretation recorded:** Full project scope — not a minimal slice. All project requirements 1–18 in scope. Session management, deep-dive, question actions, agent feedback/recovery, and multiple simultaneous interviews included. Items from q-008 list 2–4 included as project requirements. Item 1 (task-list → launch interview) remains excluded per project constraint 4, not task deferral.

<!-- answer-anchor: scope-directive -->

---

## q-001

For the core-ui slice, is this the right minimum feature bundle (from project requirements)?

1. List open questions from the queue folder with filesystem watch (~1s sync)
2. Parallel independent answer submission (question id + answer text to answer processor)
3. Researcher replenishment when open count drops below threshold
4. Pending/deactivated state after submit until the queue file is removed or modified on disk

**A)** Yes — that's the right core bundle for this task
**B)** Missing something — name what
**C)** Too much — drop one or more items (which numbers?)

**Answer:** Full project scope (see scope directive) — bundle is insufficient; implement all project requirements, not this minimum set only.

<!-- answer-anchor: q-001 -->

---

## q-002

Session list, launch new interview, and archive (project requirements 14–17) — belong in the core-ui task slice?

**A)** Yes — include all session-management requirements in core-ui
**B)** No — defer session management; core-ui targets one pre-configured interview session
**C)** Partial — include only some (name which)

**Answer:** A — include all session-management requirements (full project scope).

<!-- answer-anchor: q-002 -->

---

## q-003

Deep-dive branch from a queued question (project requirements 9–10) — in core-ui or deferred to a later task?

**A)** Include in core-ui
**B)** Defer to a later task

**Answer:** A — include in core-ui (full project scope).

<!-- answer-anchor: q-003 -->

---

## q-004

Question actions beyond answer submit — reject, request more options, defer/revisit (project requirement 8) — in core-ui?

**A)** Submit only — other actions deferred to a later task
**B)** Include all four actions (affordances exist; per-action behavior stays in design)
**C)** Include a subset — name which

**Answer:** B — include all four actions (full project scope).

<!-- answer-anchor: q-004 -->

---

## q-005

Agent run feedback and error recovery (project requirements 12–13) — how much belongs in core-ui?

**A)** Full — visible in-flight/success/failure status, re-enable submit on answer-processor failure, researcher auto-retry with exponential backoff (max 3)
**B)** Minimal — show errors visibly; skip auto-retry and advanced recovery for now
**C)** Defer entirely to a later task

**Answer:** A — full feedback and error recovery (full project scope).

<!-- answer-anchor: q-005 -->

---

## q-006

Multiple simultaneous active interview sessions (project requirement 14) — in core-ui?

**A)** Yes — user can have several interviews open/active at once
**B)** No — one active interview at a time is enough for this slice

**Answer:** A — multiple simultaneous interviews (full project scope).

<!-- answer-anchor: q-006 -->

---

## q-008

Assumed deferred unless pulled in:

1. Task-list → launch interview integration (project constraint 4)
2. Efficient question interaction extras — in-place edit of wording-approval text, keyboard multiple-choice selection (project requirements 4 and 6)
3. Two-threshold researcher concurrency — second researcher when count drops below lower threshold while first is in flight (project requirement 3)
4. Interview completion finished-state UX beyond a clear "complete" marker (project requirement 18)

**A)** Confirm all deferred for core-ui
**B)** Pull one or more in — which numbers (or close variants)?

**Answer:** B — pull in 2, 3, and 4 (project requirements). Item 1 remains out of scope per project constraint 4 (self-enclosed phase — not a task deferral).

<!-- answer-anchor: q-008 -->

---

## q-007

Scope is the full Interview UI (all project requirements 1–18). What verification bar defines this task done?

**A)** Manual dogfood — complete one real process interview end-to-end using only this UI
**B)** Automated tests covering key behaviors (queue sync, submit payload, replenishment, session lifecycle)
**C)** Both manual dogfood and automated tests
**D)** Different bar — describe

**Answer:** A — Manual dogfood — complete one real process interview end-to-end using only this UI

<!-- answer-anchor: q-007 -->

---

## q-009

For core-ui out-of-scope: does this task include changing researcher or answer-processor agent implementations or on-disk interview file formats, or is it UI-only (invoke existing agents and consume existing queue/config/transcript layouts)?

**A)** UI-only — no agent code or on-disk protocol changes
**B)** Includes agent or protocol changes — describe what

**Answer:** B — Includes agent or protocol changes — user did not yet describe what changes.

<!-- answer-anchor: q-009 -->

### q-009 follow-up (what changes)

You chose B — core-ui includes agent or protocol changes. What changes are in scope for this task?

Describe specifically (examples of areas to clarify):
1. Researcher agent implementation changes
2. Answer-processor agent implementation changes
3. On-disk interview file format changes (queue / config / transcript layouts)
4. Other — name what

**Answer:** All of the above — meaning items 1, 2, and 3 (researcher agent implementation changes; answer-processor agent implementation changes; on-disk interview file format changes). Item 4 "Other" not specified.

<!-- answer-anchor: q-009-followup -->

---

## q-010

For core-ui out-of-scope: do ui-scaffolding (GPUI app shell) and task-list UI stay in their prior tasks — core-ui adds interview functionality only, not shell or task-list work?

**A)** Yes — those prior tasks stay separate; core-ui is interview views/functionality only
**B)** No — core-ui may rework or extend shell/task-list too

**Answer:** A — Yes — prior tasks stay separate; core-ui is interview views/functionality only

<!-- answer-anchor: q-010 -->

---

## q-011

For core-ui out-of-scope: are edits to interview process skills, state agent definitions, or bootstrap conventions in scope, or does core-ui consume the process as already defined?

**A)** Out of scope — UI consumes existing process; no skill/state-agent edits in this task
**B)** In scope — this task may change process definitions too

**Answer:** B — In scope — this task may change process definitions too — user did not yet describe which.

<!-- answer-anchor: q-011 -->

### q-011 follow-up (which process definitions)

You chose B — this task may change process definitions. Which process definition changes are in scope?

Describe specifically (examples of areas to clarify):
1. Interview process skills
2. State agent definitions
3. Bootstrap conventions
4. Other — name what

**Answer:** Probably just number 1 — Interview process skills only (not state agents, not bootstrap). Treated as item 1 only.

<!-- answer-anchor: q-011-followup -->

---

## q-012

Must core-ui integrate interview functionality into the existing tod GPUI desktop app from ui-scaffolding (not create a new standalone shell or web app)?

**A)** Yes — build on the completed ui-scaffolding shell inside tod
**B)** No — may replace or bypass ui-scaffolding infrastructure

**Answer:** A — Yes — build on the completed ui-scaffolding shell inside tod

<!-- answer-anchor: q-012 -->

---

## q-013

Approve this Goal wording for task user.md?

Implement the full Interview UI product — every requirement in project user.md is in scope for this task.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different goal

**Answer:** A — Accept Goal wording as currently proposed in the question / task user.md

<!-- answer-anchor: q-013 -->

---

## q-014

Approve this requirement wording for task user.md?

Full project scope — Deliver all requirements numbered 1–18 in doc/process/projects/interview-ui/user.md. The task is complete only when each passes its statement and success-criteria checks (where present).

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different requirement

**Answer:** A — Accept full-scope requirement wording as currently proposed

<!-- answer-anchor: q-014 -->

---

## q-015

Project requirement 8 defers per-action behavior (reject, defer, deep-dive, request options) to design — core-ui requirements should establish affordances only, not specify how each action behaves. Correct?

**A)** Yes — affordances in requirements; behavior details belong in design
**B)** No — nail specific per-action behavior in this task's requirements too

**Answer:** A — Yes — affordances in requirements; behavior details belong in design. Matches project requirement 8.

<!-- answer-anchor: q-015 -->

---

## q-016

Approve this requirement wording for task user.md?

Verification — Manually complete one real process interview end-to-end using only this Interview UI; the interview must reach the interview-complete state.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different requirement

**Answer:** A — Accept verification requirement wording as in the question.

<!-- answer-anchor: q-016 -->

---

## q-017

Approve this constraint wording for task user.md?

Interview views only — Do not rework or extend the GPUI app shell or task-list UI owned by prior tasks; this task adds interview views and functionality only.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different constraint

**Answer:** A — Accept "Interview views only" constraint wording as in the question.

<!-- answer-anchor: q-017 -->

---

## q-018

Approve this constraint wording for task user.md?

Build on ui-scaffolding — Integrate Interview UI into the existing tod GPUI desktop app delivered by the completed ui-scaffolding task; do not create a new standalone shell or web app.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different constraint

**Answer:** Not processed as answered. User said "question 18" but rationale ("redundant… tasks must follow constraints of projects") matches **q-019** (Project conformance), not q-018. Left open/unmodified.

<!-- answer-anchor: q-018-deferred-misattribution -->

---

## q-019

Approve this constraint wording for task user.md?

Project conformance — Satisfy every constraint in project `user.md`; task work must not contradict them.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different constraint

**Answer:** C — Reject — drop because redundant with process rule that tasks must follow project constraints. Do not write/keep as accepted obligation. Removed from task user.md if present from scaffolding.

<!-- answer-anchor: q-019 -->

---

## q-020

I’ve reviewed the current requirements for this phase. These top-level areas look complete and coherent for a reasonable task of this kind — I don’t see a compelling gap to propose next:

1. Full project scope — deliver all project requirements 1–18
2. Verification — manual dogfood of one real end-to-end interview

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Answer:** Deferred — user said they don't know and asked to check one more time for anything missed. Not A or B. Left open unchanged pending researcher re-check / human confirmation after gap review.

<!-- answer-anchor: q-020-deferred -->

---

## q-018 (correction)

Approve this constraint wording for task user.md?

Build on ui-scaffolding — Integrate Interview UI into the existing tod GPUI desktop app delivered by the completed ui-scaffolding task; do not create a new standalone shell or web app.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different constraint

**Answer:** A — Accept (dictation correction: prior turn mis-attributed reject to q-018; q-019 reject stands).

<!-- answer-anchor: q-018 -->

---

## q-021

Approve this requirement wording for task user.md?

Agent and protocol changes — This task includes (1) researcher agent implementation changes, (2) answer-processor agent implementation changes, and (3) on-disk interview file format changes (queue, config, and transcript layouts).

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different requirement

**Answer:** A — Accept

<!-- answer-anchor: q-021 -->

---

## q-022

Approve this requirement wording for task user.md?

Interview process skills — This task may change interview process skills. Do not change state agent definitions or bootstrap conventions in this task.

**A)** Accept
**B)** Modify — provide revised text
**C)** Reject — drop or replace with different requirement

**Answer:** A — Accept

<!-- answer-anchor: q-022 -->

---

## q-023

For task-specific constraints, one category still looks worth considering:

1. Existing-session continuity — when on-disk interview formats change, in-progress sessions on disk must remain usable (backward-compatible or migrated), not left unreadable

**A)** No — skip for now
**B)** Yes — pursue 1 (or rename / close variant)

**Answer:** A — No — skip for now (everything looks fine as-is; project constraint 6 covers protocol compatibility).

<!-- answer-anchor: q-023 -->

---

## q-024

I've re-checked project user.md, task user.md, and this transcript. Did we miss anything important?

**A)** No — this set is enough for now
**B)** Yes — name what we missed

**Answer:** A — No — this set is enough for now ("everything looks fine").

<!-- answer-anchor: q-024 -->

---

## Interview complete

Requirements interview drained 2026-08-23. Queue empty. Task `user.md` organized and ready for design gate.

