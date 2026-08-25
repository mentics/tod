# Logging — plan

## Goal (from user.md)

Users can view tod’s own diagnostic logs for troubleshooting.

## Steps

1. **Dependencies** — Add `tracing`, `tracing-subscriber` (json + env-filter), and `tracing-rolling-file` (with `non-blocking`) to `crates/tod/Cargo.toml`. Confirm `cargo check`.
2. **Paths** — Extend `TodPaths` with `log_dir()` → `{repo_root}/.local/logs/tod/` and `ensure_log_dir()`. Unit-test path shape (mirror existing `TodPaths` tests).
3. **Settings model** — Extend `TodSettings` with diagnostic logging fields: `log_level` (default `info`), `log_max_size_kb` (default `51200`; valid range `1..=104857600`). Persist in existing `tod.yml` via current YAML load/save. Defaults apply when keys are absent; reject out-of-range values on save/load paths used by settings UI.
4. **Logging module** — Add `crates/tod/src/logging/` with init + path helpers only (no call-site facade). Wire [`tracing-rolling-file`](https://crates.io/crates/tracing-rolling-file) under `log_dir()` (`tod.log` + Debian-style `tod.log.N`), NDJSON, non-blocking via its `non-blocking` feature. Size bound: rotate when the active file reaches ~`max_bytes / file_slots` (live atomic so settings max-size changes apply); keep a fixed rolled-file count so total stays around the configured max (`log_max_size_kb * 1024`). Prefer direct `tracing::info!` / `error!` / etc. at call sites. Rely on appender/worker-guard defaults for flush (no separate explicit shutdown plan step).
5. **Level resolution** — Effective minimum level for a process run: CLI `--log-level` if present, else settings `log_level`, else `info`. Install a reloadable `EnvFilter` (or equivalent) so settings verbosity changes apply to later actions in the same process without relaunch. CLI override wins for the whole run even if settings change mid-run.
6. **CLI** — Extend `LaunchOptions::from_args` with `--log-level <error|info|debug|trace>`; reject unknown values. Update usage string in `main.rs`. Pass resolved options into init before UI work.
7. **Process init** — In `main` (after `--data-root` / `set_data_root`, before `App::run`): discover paths, load settings, resolve level, `ensure_log_dir`, init subscriber + appender. If creating the log directory or initializing the file appender fails, **refuse to start**: print a clear error to stderr and exit non-zero (do not open the UI). On success, emit one **info** lifecycle canonical line (mandatory plumbing deliverable for on-disk write).
8. **Settings UI** — Extend existing Settings tab (`interview/views/settings.rs`): show read-only **absolute** log directory path; control for verbosity (`error` / `info` / `debug` / `trace`); control for max size in **KB** (default 51200; clamp/validate to `1..=104857600`). Save to `tod.yml` on change; when saving verbosity (and no CLI override this run), reload the filter; when saving max size, use the new cap on subsequent prune.
9. **Canonical emissions** — This task delivers logging infrastructure plus the mandatory start lifecycle line. Ongoing conformance to [`logging-constraints.md`](../../../shared/constraints/logging-constraints.md) (when/where/level). Design-defined significant actions imply canonical log emissions when those actions exist; do **not** invent a full action inventory in this task. No secrets/credentials in fields (call-site discipline; no redaction layer; verify via review + spot-check).
10. **Verification** — Automated unit tests for paths, settings defaults/round-trip/range, CLI parse, level resolution (CLI vs settings), and prune-under-cap helper with a tiny max size (KB). Agent-driven running-context checks for on-disk NDJSON, absolute settings path display, CLI level, survive relaunch, and prune under a small configured max (see [Verification](#verification)).

## Constructions (must match design / user constraints)

| Concern | Construction |
|--|--|
| Logging crates | `tracing` + `tracing-subscriber` + `tracing-rolling-file` (+ `tracing-appender` via non-blocking) |
| Call-site style | Prefer `tracing` macros/API directly; no heavy facade |
| Logging utilities | Optional helpers only in `crates/tod/src/logging/` |
| On-disk line encoding | NDJSON (one JSON object per line) |
| Log directory | `.local/logs/tod/` under data/repo root (`TodPaths`) |
| Settings — path | Absolute resolved filesystem path for that one directory |
| Settings — verbosity | Persisted level; default `info` |
| Settings — max size | Persisted KB; default `51200`; range `1..=104857600` |
| Size / rotation | `tracing-rolling-file` size rolling (`tod.log` / `tod.log.N`); file size × slots ≈ configured max (bytes = KB × 1024) |
| Appender lifetime | Rely on `tracing-appender` defaults (no explicit guard/shutdown plan step) |
| Init failure | Log dir create or file-appender init failure → stderr error + non-zero exit; no UI |
| CLI level control | `--log-level <error\|info\|debug\|trace>` |
| CLI vs settings | CLI wins for that process run; settings stored for later runs |
| Default level | `info` |
| Settings UI surface | Extend existing Settings tab — no separate visual package |
| Shared constraints | `logging-constraints.md` binding; design-defined significant actions ⇒ canonical lines when actions exist |
| Local-only / no upload | Logs stay on disk under the local path; no shipping |

## Requirement traceability

| user.md requirement / constraint | Design / plan element | Implementation (to fill) | Check |
|--|--|--|--|
| 1. Discoverable log directory | Steps 2, 8; absolute path display | `TodPaths::log_dir`; Settings UI shows `logging::absolute_log_dir` | Settings shows the absolute log directory path |
| 2. On by default (info) | Steps 3, 5, 7; default level | `TodSettings` default `info` + `logging::init` filter + start line | With default settings, start writes ≥1 info line on disk |
| 3. Verbosity control | Steps 3, 5, 8 | Settings `log_level` + `logging::reload_level` | Changing verbosity changes what is written on later actions |
| 4. Survive quit and relaunch | Steps 4, 7 | `tracing-rolling-file` under durable `log_dir` | After quit/relaunch, prior files remain readable under shown directory |
| 5. Size-bounded storage (KB) | Steps 3, 4, 8 | `log_max_size_kb` drives rotate size + rolled file count | After exceeding configured max (default 51200 KB; test with small cap), rolling/pruning brings total around cap |
| Constraint: No secrets in logs | Step 9 | Call-site discipline (start line fields only lifecycle) | Review + spot-check of emitted fields |
| Constraint: Local-only logs | Constructions: local-only | File sink only in `logging::init`; no upload | Logs only under local `log_dir` |
| Shared: CLI + settings level control | Steps 5–6, 8 | `--log-level` on `LaunchOptions` + settings UI | CLI selects min level for that run; settings control exists |
| Shared: canonical / desktop adaptation | Steps 7, 9; design emission rule | `process_start` info line in `logging::init` | Start line present; significant actions follow constraints when defined/instrumented |

## Assumptions

1. Max-size setting is integer **kilobytes** (UI + YAML), default `51200`, range `1..=104857600`; prune compares total file bytes to `kb * 1024`.
2. Settings verbosity changes apply **mid-run** via a reloadable filter when there is **no** CLI `--log-level` for this process; with CLI set, CLI level stays for the whole run.
3. Max-size setting changes apply on subsequent prune/rotate cycles in the same process (no immediate rewrite of existing files beyond prune).
4. Size rolling and rolled-file retention come from `tracing-rolling-file` (size condition + max file count); optional prune helper only if needed for mid-run max shrink.
5. No automatic secret redaction/scrubbing layer in this task — compliance is call-site only.
6. This task does not add log upload, remote shipping, or a log viewer inside tod — external tools open the directory shown in settings.
7. Verification OS: Windows (dev machine) sufficient for running-context checks; path unit tests are OS-agnostic string/shape checks where practical.
8. Level enum in settings/CLI matches shared table: `error` \| `info` \| `debug` \| `trace` (no separate `warn` control).

## Verification

| # | Check (from design done checks + requirements) | How |
|--|--|--|
| 1 | Default start writes ≥1 info-level NDJSON line under log dir | Agent run with default settings / temp `--data-root`; assert file contains info line |
| 2 | Settings shows **absolute** log directory; verbosity change affects later actions | UI/settings exercise: absolute path visible; change level then trigger action; assert new lines respect filter |
| 3 | `--log-level` selects minimum emit level for that run | Run with `--log-level error` (and/or `debug`); assert filter behavior vs settings |
| 4 | Configured max size prune | Unit test prune helper with tiny max KB; optional running-context with small `log_max_size_kb` |
| 5 | Survive quit and relaunch | Write logs, exit, relaunch same `--data-root`; prior files still present/readable |
| 6 | Settings persistence | Change verbosity / max size KB; relaunch; values restored from `tod.yml` |
| 7 | Unit: paths, YAML defaults/round-trip/range, CLI parse, CLI-vs-settings resolution | `cargo test` in `crates/tod` |
| 8 | No secrets in logs | Call-site review + spot-check of start line (and any other lines added here) |
| 9 | Init failure refuses start | Force a log-dir or appender init failure (e.g. unwritable path); assert clear stderr + non-zero exit and no UI |

Document agent-run evidence in the task journal during `active` / `verifying`.
