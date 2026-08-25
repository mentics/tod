# Design interview — logging — 2026-08-25-0831

- **Entity:** `C:/data/git/tod/doc/process/projects/tod/tasks/logging`
- **Phase:** `design-interview`
- **Purpose:** Design interview for diagnostic logging — lock constructions (crate, on-disk layout, rotation, CLI/settings level control, module placement, done checks) conforming to task `user.md` and project logging practices constraint.
- **Session id:** `design-interview-2026-08-25-0831`
- **Started:** `2026-08-25T08:31:00-07:00`

**Prior session:** `task-requirements-interview-2026-08-25-0653.md` — requirements/constraints accepted; task moved to `design`.

**Parked for this phase:** logging crate choice (entity `to-process.md`, from requirements q-033).

## q-001

Which logging crate (or crate set) should tod use?

**1)** `tracing` + `tracing-subscriber` (plus a file appender such as `tracing-appender`)
**2)** `log` facade + `flexi_logger` (built-in rotation)
**3)** `log` facade + `env_logger` / `fern`
**4)** Other — name the crate(s)

**Recommend:** 1 — common for structured/canonical fields and runtime level reload.

Which do you want?

**Answer:** 1 — `tracing` + `tracing-subscriber` (plus file appender such as `tracing-appender`)

## q-002

What on-disk encoding should each diagnostic log line use?

**1)** One JSON object per line (NDJSON)
**2)** One structured `key=value` text line
**3)** Freeform prose lines only

**Recommend:** 1 — matches “structured, easy to parse/filter/aggregate” in the shared logging constraints.

Which do you want?

**Answer:** 1 — One JSON object per line (NDJSON)

## q-003

Where should the primary on-disk diagnostic log directory live?

**1)** Under `.local/.config/tod/logs/` (beside existing `tod.yml` / `tod.db`)
**2)** Under `.local/logs/tod/`
**3)** OS application-data location (not repo-local)

**Recommend:** 1 — matches current `TodPaths` layout; easy to show in settings.

Which do you want?

**Answer:** 2 — Under `.local/logs/tod/`

## q-004

How should the ~50 MB on-disk cap be enforced?

**1)** Rolling files (e.g. `tod.log`, `tod.log.1`, …) pruned when total size exceeds ~50 MB
**2)** Single file truncated/pruned when it exceeds ~50 MB
**3)** Other — describe

**Recommend:** 1 — keeps recent history readable while bounding total size.

Which do you want?

**Answer:** 1 — Rolling files pruned when total size exceeds ~50 MB

## q-005

Shared logging constraints require a CLI control for minimum emit level. What form should that CLI control take?

**1)** `--log-level <error|info|debug|trace>`
**2)** `-v` / `-vv` (and similar) verbosity flags mapped to levels
**3)** Other — describe

**Recommend:** 1 — explicit levels matching settings and the shared level table.

Which do you want?

**Answer:** 1 — `--log-level <error|info|debug|trace>`

## q-006

When the CLI level flag and the settings verbosity disagree for one process run, which wins?

**1)** CLI wins for that process; settings remain stored for later runs without the flag
**2)** Settings always win
**3)** Whichever is more verbose wins

**Recommend:** 1 — usual “override for this run” pattern for troubleshooting.

Which do you want?

**Answer:** 1 — CLI wins for that process; settings remain stored for later runs without the flag

## q-007

Where should logging init and file writers live in the source tree?

**1)** `crates/tod/src/logging/` module inside the tod binary crate
**2)** New workspace crate `crates/tod-logging`
**3)** Thin init in `main` / `app` only — no dedicated module yet

**Recommend:** 1 — enough structure without a new crate for this slice.

Which do you want?

**Answer:** Modify / guidance — Prefer using the **`tracing` crate API directly** in call sites. If utilities are needed, put them in **`crates/tod/src/logging/`** (option 1). Utility functions can live there; don’t force a heavy logging module abstraction — mostly direct `tracing` usage.

## q-008

What must pass for this logging work to count as done?

Proposed checks:

1. With default settings, starting tod writes at least one info-level line to the on-disk log location
2. Settings shows each diagnostic log path; changing verbosity in settings changes what is written on later actions
3. The CLI level control selects the minimum emit level for that process run
4. After total on-disk logs exceed ~50 MB, rotation/pruning brings total back around the cap
5. After quit and relaunch, prior log files remain readable at the path(s) shown in settings

**1)** Accept all five
**2)** Adjust — say which numbers change and how

**Recommend:** 1 — covers requirements + shared CLI/settings level control.

Which do you want?

**Answer:** 2 Adjust — keep 1, 3, 5 as proposed; change 2 and incorporate size-as-setting:

- **Check 2 (modified):** Settings only needs to show the **log directory** (singular). Not multiple log paths — there should be **one directory**.
- **New / related (size):** The ~50 MB cap should be a **setting**, with **50 MB as the default**. Rationale: for testing you can set a much smaller cap. So done-check 4 should reflect the configured/default cap (default 50 MB), not a hard-coded-only constant with no settings surface.
- **Checks 1, 3, 5:** Accept as proposed (“the rest look fine”).

## Interview complete

- Queue drained (q-008 processed).
- Design constructions + done checks recorded in `design.md`.
- Task `user.md` Requirements 1 and 5 updated to match Adjust (singular log directory; size cap as settings-configurable default 50 MB).
- Design → planning gate satisfied; `state.md` → `planning`.
