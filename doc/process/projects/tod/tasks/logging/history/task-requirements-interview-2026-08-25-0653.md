# Task requirements interview — 2026-08-25-0653

- **Entity:** `C:/data/git/tod/doc/process/projects/tod/tasks/logging`
- **Phase:** `task-requirements-interview`
- **Purpose:** Task requirements interview for diagnostic logging — draft measurable task `user.md` (Goal exists; Requirements/Constraints empty) covering project requirement **27. Diagnostic logging — Users can view tod’s own diagnostic logs for troubleshooting.**
- **Session id:** `task-requirements-interview-2026-08-25-0653`
- **Started:** `2026-08-25T06:53:00-07:00`

## q-001

Project requirement 27 is “Users can view tod’s own diagnostic logs for troubleshooting.”

For this task, what must “tod’s own” cover?

**1)** Tod application process only (UI, core services, integrations)
**2)** Also include managed-agent / environment diagnostics surfaced by tod
**3)** Other — describe

**Recommend:** 1 for a first shippable slice; agent transcripts and shells already cover agent-side inspection unless you need them here too.

Which do you want?

**Answer:** 1 — Tod application process only (UI, core services, integrations)

## q-002

Where must the user be able to view these diagnostic logs?

**1)** In-app viewer inside tod
**2)** Open log file/folder via external tool only
**3)** Both in-app viewer and on-disk access

**Recommend:** 1 unless you already rely on opening files outside tod for support.

Which do you want?

**Answer:** 2 — Open log file/folder via external tool only

## q-003

What depth of diagnostic detail must be available for troubleshooting?

(Notable operational events = things like starts/stops, config changes, integration call outcomes — not full debug spam.)

**1)** Errors and failures only
**2)** Errors plus notable operational events
**3)** Full debug detail available when needed

**Recommend:** 2 unless you already know you need on-demand debug depth.

Which do you want?

**Answer:** 2 — Errors plus notable operational events

## q-004

Must diagnostic logs survive a tod quit and relaunch?

**1)** Yes — logs from before quit remain after relaunch
**2)** No — current application run only

**Recommend:** 1 — troubleshooting often happens after a restart.

Which do you want?

**Answer:** 1 — Yes — logs from before quit remain after relaunch

## q-005

For viewing diagnostic logs, must the user be able to search or filter (for example by text, time, or severity)?

**1)** Yes — search or filter within the log view
**2)** No — chronological scroll/read is enough

**Recommend:** 1 if logs can grow long; 2 only for a minimal first cut.

Which do you want?

**Answer:** Not applicable — because we're doing file only (no in-app viewer). Mark/close accordingly; do not invent an obligation.

## q-006

Must the user be able to copy or export a diagnostic log excerpt for sharing outside tod (for example paste into chat or save a file)?

**1)** Yes — copy or export an excerpt outside tod
**2)** No — viewing (in-app or on disk) is enough

**Recommend:** 1 — troubleshooting often needs sharing with someone else.

Which do you want?

**Answer:** Not applicable — because we're doing file only. Mark/close accordingly; do not invent an obligation.

## q-007

While the user is viewing diagnostic logs with tod still running, must the view update live as new log lines are written?

**1)** Yes — view updates as new lines are written
**2)** No — a snapshot or manual refresh is enough

**Recommend:** 2 for a first cut unless you regularly watch logs during an active failure.

Which do you want?

**Answer:** Not applicable — because we're doing file only. Mark/close accordingly; do not invent an obligation.

## q-008

Diagnostic logs can accidentally capture sensitive values (tokens, credentials). What must hold for this task?

**1)** Operational detail OK; no special redaction this phase
**2)** Must avoid writing secrets/credentials into logs
**3)** Other — describe

**Recommend:** 2 — project already stores credentials locally; leaking them into logs would hurt troubleshooting trust.

Which do you want?

**Answer:** 2 — Must avoid writing secrets/credentials into logs

## q-009

Diagnostic logs survive quit/relaunch. How long must retained logs remain available on disk (time-based retention)?

**1)** At least the last 7 days remain available
**2)** At least the last 30 days remain available
**3)** No time-based retention this phase (other limits only, or keep indefinitely)

**Recommend:** 1 — enough for typical post-incident troubleshooting without unbounded growth.

Which do you want?

**Answer:** 3 — No time-based retention this phase (other limits only, or keep indefinitely)

## q-010

Must diagnostic log storage have a size bound (with rotation or pruning when exceeded)?

**1)** Cap total diagnostic logs around 50 MB with rotation/pruning
**2)** Cap total diagnostic logs around 200 MB with rotation/pruning
**3)** No size cap this phase

**Recommend:** 1 — keeps local disk modest for a desktop app.

Which do you want?

**Answer:** 1 — Cap total diagnostic logs around 50 MB with rotation/pruning

## q-011

Access is external-tool only (no in-app viewer). How must the user discover or open the diagnostic log location?

**1)** In-app control opens the log folder (or file) with the OS default tool
**2)** Path visible in settings; user opens it with an external tool themselves
**3)** Documented path only — no in-app path display or open action

**Recommend:** 1 — discoverable without hunting docs when troubleshooting.

Which do you want?

**Answer:** 2 — Path visible in settings; user opens it with an external tool themselves

## q-012

Detail depth is errors plus notable operational events. Must the user be able to change logging verbosity, or is that depth fixed for this phase?

**1)** Fixed at errors plus notable operational events — no user verbosity control this phase
**2)** User can raise or lower logging verbosity (for example in settings)

**Recommend:** 1 — matches the chosen depth without extra controls this phase.

Which do you want?

**Answer:** 2 — User can raise or lower logging verbosity (for example in settings)

## q-013

For external-tool access, where must diagnostic logs live?

**1)** One known local folder holds all tod application diagnostic logs
**2)** Multiple locations are OK if each is discoverable/documented

**Recommend:** 1 — simpler to find and open when troubleshooting.

Which do you want?

**Answer:** 2 — Multiple locations are OK if each is discoverable/documented

## q-014

Must diagnostic logs stay on the user’s machine only for this phase (no automatic upload or shipping off-machine)?

**1)** Stay on the machine only — no automatic upload
**2)** Automatic upload/share is in scope this phase

**Recommend:** 1 — matches local durable-state posture; sharing remains a manual external-tool action.

Which do you want?

**Answer:** 1 — Stay on the machine only — no automatic upload

## q-015

Must diagnostic logging at the required depth be active without the user turning it on first?

**1)** On by default at the required depth
**2)** Off until the user enables logging

**Recommend:** 1 — otherwise the first failure often has no logs.

Which do you want?

**Answer:** 1 — On by default at the required depth

## q-016

Approve this constraint wording for task user.md?

No secrets in logs — Diagnostic logs must not contain secrets or credentials (including API tokens and passwords).

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different constraint

Which do you want?

**Answer:** 1 — Accept

## q-017

Approve this requirement wording for task user.md?

Application process coverage — Diagnostic logging covers the tod application process: UI, core services, and integrations.

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** 3 Reject — Unnecessary requirement; we'll add constraints later about scope. Drop this requirement. Do not invent replacement scope obligations now.

## q-018

Approve this requirement wording for task user.md?

Discoverable log paths — Each diagnostic log location’s path is visible in application settings so the user can open it with an external tool.

- Success criteria:
  1. User can see each diagnostic log location path in settings
  2. Opening a shown path with an ordinary external/filesystem tool shows that location’s diagnostic logs

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** Modify — Keep the requirement statement; remove success criteria (unnecessary). Write the statement only:
Discoverable log paths — Each diagnostic log location’s path is visible in application settings so the user can open it with an external tool.

## q-019

Approve this requirement wording for task user.md?

Required depth — At the required depth, diagnostic logs include errors and failures plus notable operational events (for example starts/stops, config changes, and integration call outcomes).

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** Defer / come back — This should be a constraint (not a requirement) because it must apply across everything and everything in the future. Need to talk through all the different places to cover before locking wording. Do not write it as a requirement now. Park for later discussion. Do not invent places-to-cover list without asking.

## q-020

Approve this requirement wording for task user.md?

On by default — Diagnostic logging at the required depth is active without the user enabling it first.

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** 1 Accept

## q-021

Approve this requirement wording for task user.md?

Verbosity control — Users can raise or lower diagnostic logging verbosity in application settings.

- Success criteria:
  1. User can change diagnostic logging verbosity in settings
  2. The chosen verbosity remains after quit and relaunch

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** Modify — Keep the requirement statement; remove success criteria (unnecessary). Write the statement only:
Verbosity control — Users can raise or lower diagnostic logging verbosity in application settings.

## q-022

Approve this requirement wording for task user.md?

Survive quit and relaunch — Diagnostic logs written before a tod quit remain available after relaunch.

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** 1 Accept

## q-023

Approve this requirement wording for task user.md?

Size-bounded storage — Total on-disk diagnostic log storage is capped at around 50 MB, with rotation or pruning when the cap is exceeded.

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different requirement

Which do you want?

**Answer:** 1 Accept

## q-024

Approve this constraint wording for task user.md?

Local-only logs — Diagnostic logs remain on the user’s machine only; tod does not automatically upload or ship them off-machine.

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — drop or replace with different constraint

Which do you want?

**Answer:** 1 Accept

## Narrative scope answer (covers q-025–q-032 and parked q-019 depth/coverage)

**Answer:** All those questions were about the same scope topic.

Intent:
- One or more constraints for **when** to emit logs and **at what level**.
- Errors → error-level with rich troubleshooting detail.
- Want a **general** (not app-specific) logging best-practices document reusable across apps; the **project** references it as a constraint.
- Significant actions ≈ web “request”; on resolve emit **canonical log lines** (structured, easy to parse) at **INFO** on success / **ERROR** on failure. Default logging level: **info**.
- Desktop adaptation: do **not** log every click/navigation; do emit canonical lines for significant actions and after external calls; immediately **before** external calls emit canonical-style line at **debug**; major lifecycle events get info canonical lines.
- Temporary debug/trace OK while diagnosing; clean up afterward.
- Need **CLI arg** for level threshold AND settings control (settings already covered as a task requirement).
- Settings part = requirement (already discussed). Rest = constraints via the shared doc.

**Parent already executed (authorized by human; do not redo):**
1. Created `doc/process/shared/constraints/logging-constraints.md` from that narrative.
2. Added project constraint to `doc/process/projects/tod/user.md`:
   `5. Logging practices — Follow doc/process/shared/constraints/logging-constraints.md.`

### Mapping to open questions

## q-025

What does “places” mean for you here…

**Answer:** Same scope topic as above — when to emit logs and at what level; covered by the shared logging-constraints document (significant actions, external calls, lifecycle events; not every click/navigation). See narrative scope answer.

## q-026

Are places and scope constraints the same discussion, or two different things?

**Answer:** 1 — Same discussion — places and scope constraints are one topic (see narrative).

## q-027

Is errors plus notable operational events still the intended minimum detail level?

**Answer:** Superseded by narrative — depth/when/level defined in shared logging-constraints.md (error with rich detail; info default; canonical lines; debug pre-external-call). Not a separate task constraint.

## q-028

Hard floor vs soft default for verbosity vs required depth?

**Answer:** Covered by shared doc: default configured level is **info**; CLI + settings select the minimum emit level (user can raise/lower). Settings already a task requirement.

## q-029

Pursue non-fatal logging / ordinary-tool readability constraint categories?

**Answer:** Not addressed in narrative — skip these for now (option 1). Do not invent.

## q-030

Illustrative examples vs explicit membership list for notable operational events?

**Answer:** Superseded — shared logging-constraints.md defines when/level with canonical-line rules (not a brittle event membership checklist).

## q-031

Leave Requirement 2 as-is until depth constraint lands, or adjust now?

**Answer:** Depth/when/level now lands via project constraint → shared logging-constraints.md (default **info**). Leave Requirement 2 wording for possible later Accept tighten to say “info” explicitly; do not silent-rewrite this turn.

## q-032

Application process only still the intended scope bound for a scope constraint?

**Answer:** Superseded by narrative + shared doc (general practices + desktop adaptation). No separate task-level “application process only” constraint invented; project binds shared doc. Prior gather that agent diagnostics are out of this task’s view-logs product scope remains in transcript history only.

## q-033

Approve this tightened requirement wording for task user.md?

On by default — Diagnostic logging at the default depth (info) is active without the user enabling it first.

(Current Requirement 2 says “at the required depth”; shared logging-constraints.md default is **info**.)

**1)** Accept
**2)** Modify — provide revised text
**3)** Reject — keep current wording

**Recommend:** 1 — locks coherence with the shared default without inventing new obligation.

Which do you want?

**Answer:** Modify — Do not say “diagnostic logging.” Call it just **logging**. State that the **default logging level is info**. Preferred concise wording:

On by default — The default logging level is info.

(Human: “you don't need to call it diagnostic logging. Just call it logging… Just say that the default logging level is info.”)

**Also (park for design, not a requirement Accept now):** Human noted we should specify which crate to use for logging — they think it’s a design/“divine” question. Park for design phase; do not invent a crate choice or write it into user.md now.

## q-034

I’ve reviewed the current requirements for this phase. These top-level areas look complete…

Did we miss anything important?

**1)** No — this is enough for now
**2)** Yes — name what we missed

**Answer:** 1 — No — this is enough for now (after q-033 change).

## q-035

I’ve reviewed the current constraints for this phase. These top-level areas look complete…

Did we miss anything important?

**1)** No — this is enough for now
**2)** Yes — name what we missed

**Answer:** 1 — No — this is enough for now.

## Interview complete

- Queue drained (q-033–q-035 processed).
- Researcher status was already `complete`.
- Organize pass: no material regrouping of task `user.md` (flat Requirements 1–5 and Constraints 1–2 left as-is aside from Requirement 2 wording update).
- Proposed → design transition recorded in `state.md` and journal `2026-08-25-proposed-complete.md`.
