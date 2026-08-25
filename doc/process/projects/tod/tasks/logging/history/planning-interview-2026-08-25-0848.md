# Planning interview — logging — 2026-08-25-0848

- **Entity:** `C:/data/git/tod/doc/process/projects/tod/tasks/logging`
- **Phase:** `planning-interview`
- **Purpose:** Implementation / planning interview — lock build steps, verification mapping, and assumptions for diagnostic logging so `plan.md` is actionable and gate-ready.
- **Session id:** `planning-interview-2026-08-25-0848`
- **Started:** `2026-08-25T08:48:36-07:00`

**Prior:** Design interview `design-interview-2026-08-25-0831.md` complete; `design.md` + `plan.md` draft exist; task in `planning`.

## q-001

The draft plan lists these assumptions. Accept them for planning, or convert any into harder requirements / different wording?

1. Max-size setting is integer **megabytes** (UI + YAML), default `50`; prune compares total file bytes to `mb * 1024 * 1024`.
2. Settings verbosity changes apply **mid-run** via a reloadable filter when there is **no** CLI `--log-level` for this process; with CLI set, CLI level stays for the whole run.
3. Max-size setting changes apply on subsequent prune/rotate cycles in the same process (no immediate rewrite of existing files beyond prune).
4. Size-cap prune across rolled files may be a small **custom helper** beside `tracing-appender` (still rolling files + prune to configured max).
5. No automatic secret redaction/scrubbing layer in this task — compliance is **call-site only**.
6. This task does **not** add log upload, remote shipping, or an in-app log viewer — external tools open the directory shown in settings.
7. Verification OS: **Windows** (dev machine) is sufficient for running-context checks; path unit tests stay OS-agnostic where practical.
8. Level enum in settings/CLI matches the shared table: `error` | `info` | `debug` | `trace` (no separate `warn` control).

**1)** Accept all eight
**2)** Adjust — say which numbers change and how

**Answer:** 1 — Accept all eight assumptions.

**Note:** Later q-004 Modify supersedes assumption 1’s megabytes unit (setting becomes kilobytes); plan assumptions updated accordingly after q-004.

## q-003

For **this** task’s emission scope, what is mandatory beyond wiring?

**1)** Only the process-start **info** lifecycle canonical line is mandatory; further significant-action lines stay opportunistic (cheap one-liners only where a clear success/failure boundary already exists)
**2)** The plan must also list specific significant actions to instrument before this task counts as done

**Answer:** Modify — Must conform to `doc/process/shared/constraints/logging-constraints.md` (when/where/level). Plan need not exhaustively list every significant action place. At design level, actions defined in design imply log emission for those actions; implementation instruments significant actions accordingly. For this logging plumbing task: deliver infrastructure + mandatory start lifecycle line; shared-constraints conformance is ongoing; when significant actions are defined (in design/impl of features), they get canonical lines — do not invent a full action inventory now. Recorded rule: design-defined actions ⇒ log emissions.

## q-004

What validation bounds should the max on-disk size setting (`log_max_size_mb`) enforce?

**1)** Integer ≥ **1** MB (reject 0 / negative); no hard upper cap beyond that
**2)** Integer ≥ **1** MB and also a documented upper maximum (say what max)
**3)** Other — name the bounds

**Answer:** Modify — Change unit from megabytes to **kilobytes**. Setting is integer kilobytes. Allowed range **1 KB through 100 GiB expressed as KiB** (`1..=104857600`). Default was 50 MB → **51200 KB**. Update user.md / design.md / plan.md accordingly.

## q-005

How should Settings display the discoverable log directory path?

**1)** Absolute resolved filesystem path (from `TodPaths` / current data root)
**2)** Repo-relative path string only (e.g. `.local/logs/tod/`)

**Answer:** 1 — Absolute resolved filesystem path.

## q-006

How should verification prove the **no secrets in logs** constraint for this task?

**1)** Call-site review + spot-check that emitted fields in the start line (and any other lines added here) do not include tokens/passwords/credentials
**2)** Also require automated checks (e.g. fixture logs / greps for known secret patterns)

**Answer:** 1 — Call-site review + spot-check.

## q-007

Should the plan explicitly require holding the non-blocking appender’s worker guard for the process lifetime and flushing/dropping it on clean quit (so the start line and recent writes are durable)?

**1)** Yes — add that as an explicit init/shutdown step
**2)** No — rely on `tracing-appender` defaults; no separate plan step

**Answer:** 2 — No explicit guard/shutdown plan step; rely on `tracing-appender` defaults.

## q-008

Is the draft plan’s verification mix acceptable for the gate?

Proposed mix:

1. Automated unit tests — paths, YAML defaults/round-trip, CLI parse, CLI-vs-settings level resolution, prune-under-cap helper
2. Agent-driven running-context checks with temp `--data-root` — default start NDJSON line, Settings path + verbosity effect, CLI level, survive quit/relaunch, prune under a small configured max

**1)** Yes — unit tests + agent-driven running-context checks (journal evidence in `active` / `verifying`)
**2)** No — verification must be fully automated scripts only (no agent UI exercise)

**Answer:** 1 — Unit tests + agent-driven running-context checks.

## Mapping note

Human misnumbered one item: their “question two” addressed emission/logging-constraints scope (q-003), not startup failure (q-002). **q-002** was re-asked.

## q-002

If creating the log directory or initializing the file appender fails at startup, what should tod do?

**1)** Continue without file logging (best-effort stderr/console notice); the rest of the UI still runs
**2)** Refuse to start — exit non-zero with a clear error

**Answer:** 2 — Refuse to start — exit non-zero with a clear error if creating the log directory or initializing the file appender fails at startup.

## Interview complete

- Queue drained (q-002 processed).
- `plan.md` updated for init-failure policy; prior answers already reflected (KB size, absolute path, emission rule, verification mix, no explicit guard step).
- Entity `to-process.md` had only `done` items — deleted for planning→ready hard drain.
- Interactive look-over of `plan.md` required before `ready`.
