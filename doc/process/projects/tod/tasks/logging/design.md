# Logging — design

Task: `doc/process/projects/tod/tasks/logging/`

## Intention

Ship on-disk diagnostic logging for the tod application process so users can open the log directory with an absolute path in settings, control verbosity via settings and CLI, configure a max storage size in kilobytes (default 51200 KB), keep logs across relaunch, and prune rolling files against that cap — conforming to shared logging practices (canonical lines, levels, desktop adaptation). Design-defined significant actions imply canonical log emissions per those practices when those actions exist.

## Constructions

| Concern | Construction |
|--|--|
| Logging crates | `tracing` + `tracing-subscriber` + `tracing-rolling-file` (size rolling; `non-blocking` via `tracing-appender`) |
| Call-site style | Prefer the `tracing` API directly at call sites; no heavy logging facade/abstraction |
| Logging utilities | Optional helpers (init, appender wiring, path helpers) in `crates/tod/src/logging/` only when needed |
| On-disk line encoding | One JSON object per line (NDJSON) |
| Log directory | Single directory `.local/logs/tod/` (repo-local under the data root) |
| Settings — path | Settings shows that one log directory as an **absolute** resolved filesystem path |
| Settings — max size | Configurable max on-disk log storage size in **kilobytes**; integer **1..=104857600** (100 GiB); **default 51200 KB** |
| Size / rotation | `tracing-rolling-file`: Debian-style `tod.log`, `tod.log.1`, …; rotate when active file reaches ~`max_bytes / file_slots`; keep fixed rolled count so total ≈ configured max; mid-run max-size updates the live size threshold (and prunes if over) |
| CLI level control | `--log-level <error\|info\|debug\|trace>` |
| CLI vs settings | CLI wins for that process run; settings stay stored for later runs without the flag |
| Default level | `info` (task requirement + shared constraints) |
| Settings UI surface | Extend existing Settings tab (directory path display, verbosity, max size) — no separate visual package |
| Init failure | If log directory create or file-appender init fails at startup → clear stderr error and non-zero exit (no UI) |
| Canonical emissions | Conform to [`logging-constraints.md`](../../../shared/constraints/logging-constraints.md); design-defined significant actions get canonical lines when those actions exist — this task does not invent a full action inventory |

## Done checks

1. With default settings, starting tod writes at least one info-level line to the on-disk log location
2. Settings shows the absolute log directory path; changing verbosity in settings changes what is written on later actions
3. The CLI level control selects the minimum emit level for that process run
4. After total on-disk logs exceed the configured max size (default 51200 KB), rotation/pruning brings total back around that cap
5. After quit and relaunch, prior log files remain readable under the log directory shown in settings

## Links / external references

| Link | Scope | Binding |
|--|--|--|
| [`doc/process/shared/constraints/logging-constraints.md`](../../../shared/constraints/logging-constraints.md) | Whole doc (levels, canonical log lines, desktop adaptation, runtime level control) | required |
