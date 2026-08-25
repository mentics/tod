# Core UI — plan

## Goal (from user.md)

Implement the full Interview UI product inside the tod GPUI desktop app: all project requirements 1–18, agent/protocol changes, interview process skill updates (not state agents), and end-to-end verification via one real process interview.

## Steps

### Foundation

1. **Crate layout** — Add interview modules under `crates/tod/src/interview/` (or a workspace member crate if separation helps testing). Export from `crates/tod/src/lib.rs` or `main` wiring as needed. Confirm `cargo check` passes.
2. **Local home paths** — Implement path helpers: repo `.local` as XDG-style home; durable config/data under `.local/.config/tod/` (SQLite DB, `tod.yml`). Create dirs on first use.
3. **SQLite persistence** — Add `rusqlite` (or equivalent). Schema v1: `interview_sessions` table per design (display name, status `active`|`archived`|`complete`, paths to scratchpad/transcript/config, created/updated timestamps). Migrations on open. No transcript-body tables.
4. **Settings file** — Load/save `tod.yml` under `.local/.config/tod/` with researcher thresholds (replenish default 8, second-researcher default 2). Wire Settings UI to this file.

### Agent provider & ACP

5. **Agent provider trait** — Define `AgentProvider` (or equivalent) with operations: start researcher replenishment, start answer-processor run, start deep-dive chat session; each returns a run handle with in-flight / success / failure lifecycle. Cursor is the only v1 backend behind this trait.
6. **Cursor ACP adapter** — Implement v1 adapter using ACP over Cursor Agent CLI (`agent acp`), fresh session per run, model `auto`, subscription auth. Follow spike reference `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/`. Handle initialize → authenticate → session/new → session/prompt → session/update; auto-allow permissions for unattended runs. Map lifecycle to provider run handle.
7. **ACP decision tree (deferred fallback)** — If ACP install/auth fails on target machine during implementation: log failure; consider SDK Node sidecar or CLI print-mode behind same trait — do **not** silently switch to pay-per-token or non-Auto model. Primary path remains ACP.

### Process & protocol changes

8. **Interview SKILL bootstrap update** — Update `skills/process/interview/SKILL.md` bootstrap: UI inserts SQLite session row on kickoff; **researcher** creates transcript (under entity `history/`), session scratchpad, empty `queue/`, and `interview-config.md` on first launch (UI does not create scaffolding separately). Document UI-owned transcripts.
9. **Researcher agent update** — Update `interview-researcher-agent.md`: bootstrap ownership shift; queue file format → YAML front matter + free-text body; MC `options:` list with `key`/`label` in front matter; handle researcher action protocol input when UI forwards action payloads; `researcher-status.md` for UI observation.
10. **Answer-processor agent update** — Update `interview-answer-processor-agent.md`: stop writing transcripts (UI appends to entity `history/` before invoking processor); accept YAML multi-record answer payloads; queue delete/modify semantics unchanged; status file for UI.
11. **UI transcript writer** — Before each answer-processor or researcher-action invoke, UI appends exact Q&A (or action) to entity `history/{description}-{YYYY-MM-DD}-{HHMM}.md` per interview SKILL transcript rules.

### Queue & filesystem sync

12. **Queue file parser** — Parse queue files: YAML front matter (`id`, `created`, optional `options:` with `key`/`label`) + markdown body. One question per file.
13. **Queue watcher** — Watch session `queue/` with `notify` (debounced ~300ms). Reflect add/remove/modify within ~1s. Per-session watcher lifecycle tied to open interview.

### App navigation (minimal)

14. **Shell tabs** — Extend `Shell` with minimal top navigation: **Tasks** | **Interview** | **Settings** (per accepted visual). Swap main content area only; do not rework task-list implementation. Tasks tab keeps existing `TaskListView`.

### Session management views

15. **Session list view** — Interview tab default: list from SQLite; Active / Archive tabs; row selection + Open; **New interview** in-menu entity/purpose controls (no separate launch screen). Visual: `artifacts/visual/sessions-menu/`.
16. **New interview kickoff** — On launch: UI inserts SQLite row (status active, pending paths); builds typed prompt/payload for researcher; starts researcher via provider. Researcher creates scaffolding; UI updates SQLite paths when config/transcript/scratchpad appear (watch parent dirs or poll once after researcher success).
17. **Archive** — Archive action sets SQLite status `archived`; files stay on disk; archive tab filters; block answer submit and replenishment while archived.

### Interview workspace

18. **Three-column workspace** — Selected session: scrollable question list (id + short label) | full question body | response pane (MC → Notes → action dropdown + Submit). Visual: `artifacts/visual/interview-workspace/`.
19. **MC + keyboard** — Bind MC option keys from YAML front matter; selected option highlight; user may submit MC alone, text alone, or both.
20. **Notes + submit** — Notes text area; Submit builds YAML answer record(s) per design; **Ctrl+Enter** in Notes submits; after submit-like action, auto-select next question in list.
21. **Pending state** — After submit, mark question pending/deactivated until queue file removed or modified; re-enable and refresh content on modify.
22. **Question actions dropdown** — Consider/reconsider, Defer, More options, Deep dive — separate from MC. Build researcher YAML action records (`action` + `id` + optional body); mark pending like answer submit.
23. **In-place Complete** — When queue empty and researcher returned no further questions (not waiting on in-flight replenishment), show Complete message in workspace middle column; Back to interviews + optional doc links. Visual: `artifacts/visual/complete-inplace/`. Update SQLite status `complete`.

### Deep-dive

24. **Deep-dive view** — Separate chat UI (not three-column workspace). Start from question action with context: project, task, lifecycle state, interview purpose, phase. Ordinary agent chat via provider (non-interview session). Visual: `artifacts/visual/deep-dive/`.
25. **Use this** — User selects transcript text → **Use this** pastes into parent question Notes; user edits and submits via normal answer-processor path. No auto-detect / auto-submit.

### Agent orchestration

26. **Replenishment logic** — When open count < replenish threshold (from `tod.yml`), start researcher run. If count < second threshold while one researcher in flight, start second (max 2 concurrent). Skip for archived/complete sessions.
27. **Fresh sessions** — New ACP session per replenishment and per answer submission (v1).
28. **Status area** — Use persistent status location for in-flight and success (quiet success OK). Integrate minimal status strip if not present (tod req 10 pattern).
29. **Error toasts** — Failures → toast/banner (more visible than status). Answer-processor failure: re-enable submit on question + show error. Researcher failure: auto-retry up to 3 with exponential backoff; after 3, stop auto-retry and expose manual kickoff control.

### Settings

30. **Settings view** — Number inputs for both thresholds; labels/help per visual. Visual: `artifacts/visual/settings/`.

### Verification

31. **Manual integration pass** — Run full verification checklist (see [Verification](#verification)) on Windows.
32. **End-to-end process interview** — Using only Interview UI, conduct one real process interview on a scratch entity until interview-complete state (task user.md req 4).

## Constructions (must match design / user constraints)

| Concern | Construction |
|--|--|
| App navigation / mounting | Replace main content area; tab/menu Tasks ↔ Interview ↔ Settings; same window |
| Agent provider boundary | Interview code → trait → Cursor ACP adapter only in v1 |
| Cursor usage | Subscription + Auto model via ACP |
| Agent launch (v1) | ACP over Cursor agent; fresh session per run |
| Local home | `.local` repo home; durable files under `.local/.config/tod/` |
| Session persistence | SQLite metadata; on-disk queue/config/protocol for agents |
| Transcripts | UI writes `history/*.md` only; not in SQLite |
| SQLite schema | `interview_sessions`: name, status, paths, timestamps |
| New session row | Insert on kickoff; update paths when scaffolding appears |
| Interview finished | In-place Complete in workspace; SQLite `complete` |
| Researcher thresholds | Global Settings → `tod.yml` (not SQLite) |
| Archive | SQLite `archived`; files in place; mutation blocked |
| Answer payload | YAML multi-record: `id`, optional `option`, body |
| Researcher actions | YAML multi-record: `action`, `id`, optional body |
| Workspace layout | Three columns per accepted visual |
| Reject action | Not implemented; Consider/reconsider instead |
| Queue watcher | `notify` + debounce; no polling primary path |
| Queue format | YAML front matter + body; one file per question |
| MC binding | `options:` with `key`/`label` in front matter |
| Launch UI | From session list/menu; researcher creates scaffolding |
| Agent errors | Toast/banner + in-context recovery |
| Agent success/in-flight | Status area; no success toast |
| Visual design | Match accepted packages under `artifacts/visual/` |
| Scope constraint | Interview views only; do not rework task-list UI |
| Process skill scope | May change interview SKILL + researcher/answer-processor agents; not state agents |

## Requirement traceability

### Project `user.md` (reqs 1–18)

| Req | Plan element | Implementation (to fill) | Check |
|--|--|--|--|
| 1. Replenishment threshold | Steps 4, 26, 30; Constructions: thresholds in `tod.yml` | `TodSettings` + `SettingsView` (wired); replenishment scheduler pending step 26 | Replenish runs below configured threshold; user can change in Settings |
| 2. Queue folder sync | Steps 12–13 | `notify` watcher per session | Add/remove/change reflected within ~1s |
| 3. Researcher concurrency | Steps 26–27 | Max 2 in-flight researcher runs | Second run when count < lower threshold while first in progress |
| 4. Efficient question interaction | Steps 18–20 | Editable Notes; MC keyboard; in-place wording edit | From approval-style question, edit text in pane and submit without copy elsewhere |
| 5. Answer submission payload | Steps 10–11, 20 | UI builds YAML record; invokes answer-processor with payload | Question id + answer content reach processor (YAML shape per design) |
| 6. Keyboard MC selection | Step 19 | Key bindings from `options:` keys | User selects MC option via keyboard |
| 7. Pending state | Step 21 | Pending UI until file gone/modified | Submit → pending; re-enable on modify |
| 8. Question actions | Step 22 | Action dropdown → researcher protocol | Deep-dive, consider, more-options, defer available |
| 9. Deep-dive start | Step 24 | Separate non-interview session + context | Branch starts from queued question |
| 10. Deep-dive closure | Step 25 | Use this → parent Notes; manual submit | No auto-detect/submit; user submits parent normally |
| 11. Fresh agent sessions | Steps 6–7, 27 | ACP session/new per run | New session per replenishment and per answer |
| 12. Agent run feedback | Steps 28–29 | Status + toast patterns | Visible in-flight, success (status), failure (toast) |
| 13. Error recovery | Step 29 | Re-enable submit; researcher retry + manual kickoff | AP failure re-enables question; researcher 3× backoff then manual |
| 14. Multiple simultaneous interviews | Steps 15–16, 13 | Per-session SQLite + watchers + provider runs | Two+ sessions active concurrently |
| 15. Session list and launch | Steps 15–16 | Session list view + in-menu new interview | List visible; launch from list |
| 16. Archive | Step 17 | SQLite archived + UI filter + block mutations | Archived reopenable; submit/replenish blocked |
| 17. New session creation | Steps 8–9, 16 | UI row + researcher bootstrap | User provides context; researcher creates scaffolding |
| 18. Interview completion | Step 23 | In-place Complete + SQLite complete | Empty queue, no pending replenishment, clear finished state |

### Task `user.md`

| Req | Plan element | Implementation (to fill) | Check |
|--|--|--|--|
| 1. Full project scope | All steps + traceability above | Entire interview feature set | Each project req 1–18 passes |
| 2. Agent/protocol changes | Steps 8–11 | SKILL + agent defs + on-disk formats | Researcher/AP agents and queue/config/transcript layouts updated |
| 3. Process skills (not state agents) | Steps 8–10 | `interview/SKILL.md`, researcher, answer-processor only | No edits to `planning-agent.md` etc. |
| 4. E2E verification | Step 32 | Real interview via UI only | Reaches interview-complete |

### Inherited / constraints

| Source | Plan element | Check |
|--|--|--|
| Project constraint 1: embedded GPUI | Steps 14–30 in existing `crates/tod` | Not standalone/web |
| Project constraint 4: self-enclosed | No task-list → interview integration | Session list is entry point |
| Project constraint 5: session durability | Steps 3, 16; SQLite + on-disk files | Survives app restart |
| Project constraint 6: agent compatibility | Steps 8–12 | Queue/config work with updated agents |
| Task constraint 1: interview views only | Step 14 minimal tabs | Task list unchanged beyond tab shell |
| Task constraint 2: ui-scaffolding | Step 14 extends `Shell` | Same GPUI app |
| tod req 10 status area | Step 28 | In-flight/success in status location |
| tod req 11 failure feedback | Step 29 | Errors in toast/banner |

## Assumptions

1. Developer has Cursor Agent CLI installed and authenticated (`agent login` or `CURSOR_API_KEY`) on the Windows dev machine; ACP path verified by user spike.
2. ACP remains the v1 primary launch path; fallback adapters are contingency only (see Step 7 decision tree).
3. GPUI + gpui-component versions from ui-scaffolding/task-list remain acceptable; interview views follow same stack.
4. `rusqlite` bundled SQLite is acceptable for v1 session metadata.
5. Minimal status area and toast/banner components may be net-new in tod shell (not present in task-list task) — scoped to interview + shared hook points only.
6. End-to-end verification uses a throwaway process entity (task or project) created for the test interview.
7. Cross-platform verification on Windows only for this task (consistent with prior tod tasks).
8. Implementation interview waived — design interview, task requirements interview, accepted visuals, and ACP spike provide sufficient step-level and verification detail; waiver recorded in `history/` pending human confirmation at look-over.

## Verification

Manual verification on **Windows**. Run from repo root after implementation steps complete.

### Per-requirement checks (project 1–18)

| # | Check | How |
|--|--|--|
| 1 | Replenishment at configured threshold; Settings changes threshold | Lower open count artificially; change Settings; observe runs |
| 2 | Queue sync ~1s | Add/remove/modify queue file on disk; UI updates |
| 3 | Max 2 researcher runs | Drop count below both thresholds during in-flight run |
| 4 | Edit approval text in place | Open wording-approval question; edit Notes; submit |
| 5 | Answer payload to processor | Submit answer; confirm processor receives id + content (YAML) |
| 6 | Keyboard MC | Press option key; selection moves |
| 7 | Pending until resolved | Submit; pending until file deleted/modified |
| 8 | All four action types | Exercise consider, defer, more-options, deep-dive |
| 9 | Deep-dive session starts | Deep-dive opens separate chat with context |
| 10 | Use this → manual submit | Paste from deep-dive; edit; submit parent |
| 11 | Fresh sessions | Observe new ACP session per run (logs/handles) |
| 12 | Status + error visibility | In-flight in status; failure in toast |
| 13 | Recovery paths | Force AP failure (re-enable); force researcher failure (retry then manual) |
| 14 | Two sessions concurrent | Open two interviews; both functional |
| 15 | List + launch | Session list; new interview from menu |
| 16 | Archive | Archive session; reopen; mutations blocked |
| 17 | Researcher scaffolds | New interview; researcher creates config/queue/transcript |
| 18 | Complete state | Drain queue; in-place Complete shown |

### End-to-end (task req 4)

| # | Check | How |
|--|--|--|
| E2E | Full process interview via UI only | New interview on scratch entity → answer questions through design/planning-like phase → interview-complete without chat fallback |

### Regression

| # | Check | How |
|--|--|--|
| R1 | Task list still works | Tasks tab: list renders and keyboard nav unchanged |
| R2 | App restart durability | Restart tod mid-interview; session and queue restored |

**Out of scope:** automated UI tests; macOS/Linux verification; task-list → interview integration.
