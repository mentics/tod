# Core UI — plan

> **Cycle:** redesign → implement (2026-08-25). Prior foundation/workspace already shipped; this plan is the **delta** for H1–H8 (project reqs 6, 18–24) + task req 5 (F1–F6). Do not re-land Steps 1–30 from the original build unless a fix step touches them.

## Goal (from user.md)

Deliver full Interview UI (project reqs 1–24) inside tod GPUI, including selection/nav/MC/Complete UX, unbootstrapped-session recovery, and independent-review fixes F1–F6; re-verify (mock + ACP smoke + E2E as needed).

## Baseline (already implemented)

Treat as done unless a redesign step below revises it:

- Crate layout, `.local` paths, SQLite sessions, `tod.yml` settings
- Agent provider + Cursor ACP + mock provider
- Interview SKILL / researcher / answer-processor bootstrap & queue YAML formats (letter MC examples may still need digit-only scrub — see R2)
- Queue watcher, shell tabs, session list/archive/kickoff
- Three-column workspace, pending state, actions, deep-dive, replenishment, status/toasts
- **`confirm_toast`** helper (`crates/tod/src/ui/toast.rs`) for Yes/No confirmation notifications
- Prior verifying PASS (sandbox mock) — **not** a waiver of H1–H8 / F1–F6

## Redesign steps (ordered)

### Protocol & agents (digits + payload shape)

1. **Digit-only MC in agent docs** — Scrub researcher (and related) skills/prompts/protocol examples so `options[].key` / labels are `"1"`, `"2"`, `"3"`, … only. No A/B/C examples. Task req 2; project req 6. Files under `skills/process/interview/`.
2. **Answer/action YAML body placement (F3)** — `format_answer_payload` / `format_action_payload` (`transcript.rs`): front matter = `id` + optional `option` (answers) or `action` + `id` (actions) only; free text **after** closing `---` — do not serialize `body` into FM via `serde_yaml::to_string(record)`. Align with `design.md` protocol examples.

### Agent / run correctness (F1–F6)

3. **Mock researcher actions (F1)** — Stop routing researcher-action submits through replenish-only paths. `submit_action` must start an action-shaped run; `MockAgentProvider` must handle action prompts with defer/reconsider/more-options semantics (mutate/delete queue per action), not `replenish_from_prompt`. Touch `workspace.rs` submit path + `agent/mock.rs`.
4. **Pending clear on failure (F2)** — On answer-processor / researcher-action failure, clear pending for the **finished run’s `question_id`**, not `last_submitted_id`. Concurrent action + later answer must not clear the wrong id.
5. **ACP cancel kills child (F4)** — `CursorAcpProvider::cancel_run` must retain a worker/process handle and terminate the ACP child; hung-replenish reconcile must cancel for real, not only drop UI bookkeeping.
6. **Workspace queue bind (F5)** — `WorkspaceView::new`: bind only the session’s `config_path` / queue. **No** fallback to repo-root `interview-config.md` + `queue/`. If paths missing, show pending/scaffolding state until sync — never watch the wrong queue.
7. **Hung replenish = failure (F6)** — `reconcile_hung_replenishment` must finish runs as **error/cancelled**, not `Ok(())` success (must not mark replenish succeeded / exhausted falsely).

### Workspace UX (H1–H8 / project 20–24 + 6, 18)

8. **Selection visibility (H1/H2 / req 20)** — Session-list and question-list selected rows + focus chrome on list/response controls: strong contrast (not weak accent tint alone). Cite `refs/process/other/ux-design.md` contrast / states / actionable lists.
9. **Single-line question rows (H3 / req 21)** — Question list: one line `id` + short label; ellipsis OK; no stacked id-above-label.
10. **Workspace focus model (H4 / req 22)** — Focus regions: (A) question list, (B) response column in visual order MC → Notes → actions → Submit. **Middle column never focuses.** Right from list → uppermost response control; Left from response → list; Up/Down among response controls.
11. **Notes edit mode (H4 / req 22)** — Focus ≠ edit. Enter/Space enter edit; Escape exits edit (stay on Notes); while editing: text arrows; digit MC off; Ctrl+Enter submits. Click Notes → edit immediately.
12. **MC submit paths (H4/H6 / reqs 6, 22)** — Digits only; digit key → **immediate submit** when not in Notes edit. Space/Enter on focused MC submits; click MC submits. Remove letter-key select-only behavior.
13. **Escape stays in workspace (H5 / req 23)** — Escape never navigates to session list. Exit Notes edit when editing; otherwise no-op (unless nested chrome). Leave via **Back to interviews**.
14. **Response column layout (H7 / req 24)** — Response column horizontal fill / flex with window; MC labels wrap, **no ellipsis truncate**; Notes width tracks column. Drop fixed max-width that clips.
15. **Complete vs bound queue (H8 / req 18 + F5)** — Show in-place Complete **only** when bound queue has zero open questions and replenishment not waiting. SQLite `complete` must not stick Complete over a non-empty bound queue; if questions reappear, show body UI and allow answers. Pair with step 6 (correct queue bind).

### Session bootstrap recovery (req 19 — 2026-08-25)

16. **Unbootstrapped session gate** — `SessionsView::open_selected` / `open_workspace`: if session lacks valid `config_path` on disk and bootstrap is not in flight for that session, show **`confirm_toast`** (*“{label} has not been set up yet. Do you want me to set it up?”*). **No** → dismiss, stay on list. **Yes** → reactivate if needed, `start_researcher_bootstrap`, open workspace. Do **not** auto-bootstrap on open without consent. New compose kickoff (Shift+Enter) remains auto-bootstrap without toast.
17. **Workspace self-healing** — `WorkspaceView`: when `scaffolding_pending` and bootstrap gate clear, emit `NeedsBootstrap`; sessions view closes workspace and re-shows confirm toast. Prevents infinite “Waiting for researcher scaffolding”.

### Answer-processor session pool (req 7a, 11 — 2026-08-25)

21. **Remove global answer lock (7a)** — `can_mutate` must not gate on `answer_in_flight()`. Only per-question `pending` disables that question’s submit path. User can submit B while A’s processor run is in flight.
22. **Settings: pool size + answers per session** — Add `AnswerProcessorSettings` to `TodSettings` / `tod.yml` (`session_pool_size`, default 4; `answers_per_session`, default 4). Settings UI: two threshold rows (same layout as researcher). Update accepted settings visual notes.
23. **ACP session pool (provider)** — Refactor `CursorAcpProvider` (and mock) so answer-processor runs use a **pool** of long-lived ACP sessions: `session/new` once per pool slot; subsequent prompts via `session/prompt` on the same session. Track prompt count per session; **on response completion**, if count reached answers-per-session, **close immediately** (do not defer recycle to the next submit). **Assign:** idle session → use; if in flight on next submit → another idle or new slot (until pool max); all busy at max → local queue drained on idle.
24. **Workspace dispatch** — `submit_answer` calls provider pool dispatch (not one-shot `spawn_run` per answer). Track multiple concurrent `RunKind::AnswerProcessor` runs; pending/failure still keyed by `question_id` (F2).
25. **Pool status footer (11a)** — Status footer right side: `{active} active / {in_pool} in pool / {max} max`; wire to provider pool counts; left side unchanged.

### Verification & close

18. **Targeted regression** — cargo test/check; mock batch covering F1–F3, F5–F6 paths; hung-cancel smoke for F4 if feasible; UI checks for reqs 6, 18–24.
19. **Re-verify prior checklist** — Re-run sandbox mock batch + one ACP smoke (or full prior verifying harness) so redesign did not regress reqs 1–17 / deep-dive.
20. **E2E** — If prior E2E is stale relative to this delta, re-run one real process interview via UI to interview-complete (task req 4).

## Constructions (must match design / user)

| Concern | Construction |
|--|--|
| MC keys | Digits only; immediate submit; agents emit `"1"`/`"2"`/… |
| Answer / action YAML | FM keys only (`id`/`option` or `action`/`id`); body after `---` |
| Mock actions | Action semantics, not replenish |
| Pending on failure | Clear by run `question_id` |
| ACP cancel | Kill child process |
| Queue bind | Session config only; no repo-root fallback |
| Hung replenish | Failure/cancel, not success |
| Selection chrome | High-contrast selected + focused |
| Question list rows | Single-line id + label |
| Focus model | List ↔ response; middle non-focusable |
| Notes edit | Explicit edit mode; Ctrl+Enter; click-to-edit |
| Escape | No leave workspace |
| Response layout | Flex horizontal fill; wrap; no MC truncate |
| Complete | Only empty bound queue; don’t stick over open Qs |
| Unbootstrapped open | Confirm toast; no silent auto-bootstrap |
| Bootstrap retry | Workspace `NeedsBootstrap` → list + toast |
| Rapid submit (7a) | Submit A then B immediately; only A pending |
| Session pool (11) | Steps 22–25 | Pool settings; reuse + parallel; recycle on Nth response; footer counts |
| Scope | Interview views only; no state-agent edits |

## Requirement traceability

### Project `user.md` (1–24)

| Req | Plan | Check |
|--|--|--|
| 1–6, 8–10, 12–17 | Baseline + step 19 regression | Prior verifying checks still pass |
| 6 MC digits + immediate submit | Steps 1, 12 | Digit keys submit; no letters; agents emit digits |
| 18 Complete vs queue | Steps 6, 15 | Complete only empty bound queue; reopen body if Qs return |
| 7 Pending per question | Baseline + step 21 | Only submitted Q pending; others submittable |
| 7a Rapid submit | Step 21 | No global answer-in-flight lock |
| 11 Session pool | Steps 22–25 | Pool settings; reuse + parallel; recycle on Nth response |
| 11a Pool visibility | Step 25 | Footer shows active / in pool / max |
| 19 Unbootstrapped recovery | Steps 16–17 | Toast on open; No stays on list; Yes bootstraps; self-heal from workspace |
| 20–21 Selection + single-line | Steps 8–9 | Selected/focus clearly distinct; one line id+label |
| 22 Workspace nav | Steps 10–12 | Left/Right/Up/Down, Notes edit, MC Space/Enter/click, Ctrl+Enter |
| 23 Escape | Step 13 | Escape does not → session list |
| 24 Response layout | Step 14 | Fill width; wrap; no truncate |

### Task `user.md`

| Req | Plan | Check |
|--|--|--|
| 1 Full project 1–24 | All redesign + baseline | Each project req check |
| 2 Agent/protocol + digits | Steps 1–2 | Skills/prompts digit-only; YAML shape |
| 3 Interview skills only | Step 1 | No state-agent edits |
| 4 E2E | Step 20 | Interview-complete via UI |
| 5 F1–F6 | Steps 2–7 | Each finding fixed + pointer in review journal |

## Assumptions

1. Prior verifying PASS remains valid evidence for untouched paths; redesign requires re-check of touched paths + smoke regression.
2. Implementation interview waived for this redesign planning pass — locked design constructions + Accept H1–H8 + F1–F6 journal are sufficient; waiver in `history/`.
3. Human authorized advance past interactive look-over to **active** once plan ready (2026-08-25).
4. Flex recipe for response column may use GPUI `flex`/`flex_grow` sharing remainder after list+body; exact weights implementer choice if visual matches req 24.
5. Windows-only verification continues.

## Verification

| # | Check | How |
|--|--|--|
| F1 | Mock defer/reconsider/more-options | Mock provider; queue mutates per action, not blind replenish |
| F2 | Failure clears correct pending | Concurrent action + answer; fail first; only its id re-enabled |
| F3 | YAML FM has no `body` key | Inspect payload / unit test on formatters |
| F4 | cancel_run kills ACP child | Start run; cancel; process gone (Task Manager / wait) |
| F5 | No repo-root queue bind | Open session without config; must not watch repo `queue/` |
| F6 | Hung replenish ≠ success | Force hung reconcile; status/error not exhausted-success |
| 19 | Unbootstrapped confirm toast | Open session without config; toast Yes/No; No stays on list; Yes bootstraps |
| 19 | Workspace self-heal | Stuck scaffolding_pending without bootstrap → back to list + toast |
| 6 | Digit immediate submit | Press `1`; answer submits |
| 18 | Complete gating | Non-empty queue never shows stuck Complete |
| 20–21 | Selection + single-line | Visual/manual |
| 22–23 | Nav + Escape | Keyboard/mouse matrix per req 22–23 |
| 24 | Response flex/wrap | Widen window; long MC wraps |
| 7a | Rapid multi-submit | Submit q-001 then q-002 without waiting |
| 11 | Pool reuse + parallel | Same session for sequential idle submits; close after Nth response |
| 11a | Pool footer counts | `0 active / 1 in pool / 4 max` in status footer |
| R | Regression | Mock batch + ACP smoke |
| E2E | Task req 4 | Full interview via UI if needed |

**Out of scope:** letter-key MC; Reject action; state-agent edits; macOS/Linux; task-list → interview integration.
