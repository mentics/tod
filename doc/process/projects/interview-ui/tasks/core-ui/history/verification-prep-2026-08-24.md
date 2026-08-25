# Verification prep — 2026-08-24

Implementation session completed plan steps 17, 24–30 (code). Steps 31–32 are manual verification in `verifying` state.

## Automated (ship-with-code)

| Check | Result |
|-------|--------|
| `cargo check -p tod` | Pass |
| `cargo test -p tod` | 14 tests pass (db, config, queue, transcript, settings, replenishment, agent prompt) |

## Per plan step — implementation status

| Step | Status | Notes |
|------|--------|-------|
| 17 Archive | Done | SQLite `archived`, Active/Archive tabs, workspace banner + `mutations_blocked`, replenishment skipped |
| 18–23 Workspace | Done (prior session) | Three-column workspace live |
| 24 Deep-dive view | Done | Separate chat UI from workspace; parent context bar |
| 25 Use this | Done | Pastes agent turn into parent Notes; manual submit on workspace |
| 26 Replenishment | Done | Threshold logic in `replenishment.rs`; scheduler in workspace poll |
| 27 Fresh sessions | Done | ACP `session/new` per run (existing adapter) |
| 28 Status area | Done | Workspace/deep-dive footer status line |
| 29 Error + retry | Done | Error banner; researcher 3× backoff; manual Kickoff researcher button |
| 30 Settings | Done | Number inputs left, labels/help right per accepted visual |
| 31 Manual integration | Pending | Requires interactive Windows pass (see plan Verification table) |
| 32 E2E process interview | Pending | Human-only; conduct in verifying state |

## Manual checklist (for verifying state)

Run from repo root with `cargo run -p tod` on Windows.

1. **Archive** — Archive a session; reopen from Archive tab; confirm submit/replenish blocked; files remain.
2. **Deep dive** — Open workspace → Deep dive action → chat → Use this → edit Notes → Submit.
3. **Replenishment** — Lower open count below threshold (or use small queue); observe researcher run in status.
4. **Second researcher** — With one researcher in flight and open count below second threshold, observe second run (max 2).
5. **Researcher failure recovery** — Force failure (e.g. invalid agent path); confirm retry/backoff then manual kickoff.
6. **Settings** — Change thresholds in Settings; confirm `tod.yml` updates.
7. **Regression** — Tasks tab list still works.
8. **E2E** — Full process interview via UI only until Complete (step 32).

## Known limitations

- ~~Scaffolding sync after kickoff uses newest `interview-config.md` heuristic (may mismatch session if multiple concurrent kickoffs).~~ **Fixed 2026-08-24** — matches entity + phase; earliest config after kickoff (`find_bootstrap_config_for_session`).
- MC keyboard hard-bound to A/B/C/D keys (YAML `options.key` validated on select but not dynamically bound).
- Live ACP not exercised in automated tests; requires Cursor Agent CLI on dev machine.
- Deep-dive multi-turn sends full conversation transcript in each ACP prompt (v1).
