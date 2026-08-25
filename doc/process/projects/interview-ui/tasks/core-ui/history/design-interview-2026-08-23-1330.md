# Design interview — core-ui — 2026-08-23

## Session

**Agent:** Design agent bootstrapped design interview. Lifecycle state `design`, mode `interactive`. Human directed proceed to design.

**Prior context:**

- Task `user.md`: full Interview UI (project reqs 1–18), agent/protocol changes in scope, interview skills changes in scope, verification dogfood, constraints (interview views only, build on ui-scaffolding).
- Project `user.md`: 18 requirements across queue/sync, interaction, deep-dive, agent orchestration, session management.
- Project `to-process.md`: multiple open design-phase items (list layout preference, deep-dive chrome, question-action behaviors, on-disk protocol details, branch session scaffolding, completion UX, agent reply formats for MC keyboard).
- Codebase: GPUI shell + `TaskListView` + reusable `ListView<T>` exist; no interview Rust code; protocol in `refs/process/interview/`; agent launch undecided (zero code); no filesystem watcher yet.

**Design probe areas:**

- View/navigation architecture (how interview mounts in app without reworking shell)
- Agent launch mechanism (Cursor SDK, CLI, subprocess)
- UI session persistence location/format
- Queue watcher + question list delegate
- Per-action behaviors (reject, defer, more options, deep-dive)
- On-disk protocol refinements
- Deep-dive branch session model

## q-001

How should users reach the Interview UI inside tod without reworking the existing shell or task-list views?

**A)** Replace the main content area — add minimal navigation (e.g. tab or menu) to swap task list ↔ Interview UI in the same window
**B)** Separate window — Interview UI opens in its own GPUI window; task-list window stays as-is
**C)** Overlay/panel — Interview UI slides over or beside task list without changing task-list code

**Recommend:** B — zero shell/task-list changes; clearest fit for "interview views only."
Which do you want?

**Answer:** A — Replace main content area with minimal navigation (tab/menu) to swap task list ↔ Interview UI in same window.

## q-002

How should the UI start researcher and answer-processor runs?

**A)** Cursor SDK — programmatic agent API from Rust (`@cursor/sdk` or equivalent)
**B)** Cursor CLI — spawn `cursor` / agent CLI subprocess with a prompt and config path
**C)** Configurable external command — generic subprocess template (not Cursor-specific)

**Recommend:** B for v1 unless SDK is already proven in this repo — subprocess boundary matches fresh-session-per-run requirements.
Which do you want?

**Answer:** Not A/B/C pick yet — design decisions:
- Module boundary: interview primary code talks to an agent provider interface; swap backends (Cursor now; Claude later).
- Cursor is the only v1 backend.
- Must work with Cursor subscription (not pay-per-token); user uses Auto model only (special usage availability).
- Spike required to choose SDK vs CLI (or other) for Cursor interaction under those constraints.

## q-003

Where should the UI persist interview session list metadata (display name, active/archived/complete status, path to scratchpad)?

**A)** UI-owned index under `.local/` (e.g. JSON/SQLite beside existing agent scratchpad tree)
**B)** Derive entirely from scanning `.local/agent/process/…/scratchpad/interviews/` — no separate index file
**C)** Extend each session's `interview-config.md` with UI fields — list view scans config files only

**Recommend:** A — archive/complete status and labels are awkward to infer from filesystem alone.
Which do you want?

**Answer:** Shared SQLite DB under `.local/data/tod` — interview is first consumer; later used by tod app generally. Not a separate JSON index.

## q-004

For the open-question list (see all questions, answer any in any order), which layout?

**A)** Master-detail — scrollable question list + fixed detail/answer pane for the selected question
**B)** Single scrollable list — each question row expands inline for reading and answering
**C)** List + overlay — compact list; selecting a question opens a full-screen or modal answer surface

**Recommend:** A — best fit for efficient keyboard navigation and parallel visibility of the queue.
Which do you want?

**Answer:** A — Master-detail (list + fixed answer pane).

## q-005

When the user rejects a queued question, what should the UI do?

**A)** Submit rejection to answer-processor — agent records it in transcript and removes or obsoletes the queue file
**B)** Delete the queue file locally only — no agent run; researcher replenishes later if needed
**C)** Move question to a deferred/hidden list — stays off the open queue but recoverable in UI

**Recommend:** A — keeps transcript authoritative and matches agent-owned queue semantics.
Which do you want?

**Answer:** Do not have "reject". Instead reconsider / "consider" style action (user said: "we'll probably do something like reconsider. We'll just be consider, but not reject.").

## q-006

When the user defers a question to revisit later, what should the UI do?

**A)** Submit defer intent to answer-processor — transcript note; queue file removed or marked by agent
**B)** UI-only hide — question leaves the active list but file stays on disk unchanged
**C)** Move to a separate "deferred" list in UI — queue file unchanged; user can restore manually

**Recommend:** A — agent transcript should record deferrals for researcher replenishment logic.
Which do you want?

**Answer:** Yes defer — submit defer intent to the researcher (not answer-processor). User said "submit it to the researcher."

## q-007

What minimum on-disk scaffolding must a deep-dive branch session create at start?

**A)** Transcript only — append-only markdown under entity `history/` or branch-specific path
**B)** Transcript + branch config — small config file linking parent session id and parent question id
**C)** Full scratchpad — branch gets its own directory mirroring interview scratchpad minus queue

**Recommend:** B — enough to wire closure back to the parent question without inventing a full second interview.
Which do you want?

**Answer:** User reframed: deep-dive context should include rich context, not only scaffolding choice:
- Current project and task
- Current lifecycle state
- Purpose of this interview
- Whether initial / design / planning interview (phase)
Scaffolding min artifacts (A/B/C) left open; question modified to ask scaffolding only.

## q-008

When the user requests more options on a queued question, what should the UI do?

**A)** Submit request to answer-processor — agent modifies the question file body with additional options
**B)** Submit request to researcher — new researcher run targets that question id specifically
**C)** UI-only prompt edit — user edits question text locally; no agent until they submit an answer

**Recommend:** A — answer-processor already may modify queue files; keeps one agent path per user action.
Which do you want?

**Answer:** B — Submit request-more-options to the researcher.

## q-007 (scaffolding follow-up)

What minimum on-disk scaffolding must a deep-dive branch session create at start?

**A)** Transcript only
**B)** Transcript + branch config
**C)** Full scratchpad

**Answer:** None of the above / no special on-disk scaffolding. Deep-dive is just an agent chat session: UI sends context (already settled), then ordinary chat — no dedicated transcript/config/scratchpad artifacts required specifically for the branch.

## q-009

Cursor CLI leftovers binding (pre-ship Auto billing check vs product gate vs ACP in v1) — see queue file.

**Answer:** User does not understand the question / doesn't know. Do not delete; rewrite q-009 in plain language (same A/B/C decision). Left open in queue.

## q-010

When the user chooses Consider / Reconsider on a queued question, what should the UI do?

**A)** Submit to researcher
**B)** Submit to answer-processor
**C)** UI-only flag

**Answer:** Reconsider → submit to the **researcher**.

## q-011

Where should the UI place the control that starts a deep-dive from a question?

**A)** Detail-pane action only
**B)** Master-list row affordance only
**C)** Both

**Answer:** Deep-dive is one of the **other actions** set (with defer, reconsider), not a unique chrome placement. Prefer a dropdown / “Run other action” (or similar) for those actions, separate from multiple-choice option selection.

Volunteered answer-submit UX:
- User may submit free-text alone
- User may select an MC option alone
- User may combine MC selection + extra text in one submit
- Must be able to submit text without choosing an MC option

## q-012

Which construction should watch the interview queue directory?

**A)** `notify` only
**B)** Poll-only
**C)** Hybrid

**Answer:** A — `notify` only (no polling). User prefers A unless Windows notify buffer overflow becomes a real problem (usual argument for hybrid).

## q-013

How should queue / config / transcript formats evolve for v1?

**A)** Keep markdown layouts; additive only
**B)** Versioned machine header + markdown body
**C)** Migrate queue entries to JSON files

**Answer:** C — queue entries as **JSON files**. Agents/protocol must change accordingly (in scope).

## q-014

What reply format should agents use so the UI can bind keys to options?

**A)** Inline labels in markdown body (`**A)**` / `**B)**` / …)
**B)** Structured options block required
**C)** Sidecar options JSON

**Answer:** B — structured options block required.

## q-015

How should the launch UI collect and pass interview context?

**A)** Structured form
**B)** Free-text only
**C)** Picker + purpose

**Answer:** UI side — make launch easy: structured form and/or picker; if existing projects/tasks can be detected, show a list. Prefer construction: structured launch form that includes entity picker for existing project/task (or new) + purpose. Not free-text-only.

## q-016

When an interview hits the complete condition (empty queue and the latest researcher run added no further questions), how should the finished state appear?

**A)** In-session finished panel in the interview view (status + actions to open related docs manually) — session-list also shows complete
**B)** Blocking modal/dialog until dismissed, with the same doc-open actions
**C)** Session-list status only — no dedicated finished chrome inside the interview view

**Recommend:** A — clear finished state where the user already is; list badge is secondary.
Which do you want?

**Answer:** A — In-session finished panel + session-list complete.

## q-017

Where should the two researcher replenishment thresholds (defaults 8 and 2) be stored and edited?

**A)** Global app Settings UI, persisted in the shared SQLite DB under `.local/data/tod`
**B)** Global app Settings UI, persisted in a separate config file under `.local/` (not SQLite)
**C)** Per-interview only — fields on each session’s `interview-config.md` (no global app settings)

**Recommend:** A — requirements call for application settings; SQLite is already the shared store.
Which do you want?

**Answer:** B — Global Settings UI, persisted in a separate config file under `.local/` (not SQLite).

## q-018

When the user archives an interview session, which persistence construction should apply?

**A)** SQLite status → `archived` only; session files stay put; archive view filters on status; mutation (answer submit / replenish) blocked while archived
**B)** Move session scratchpad (and/or transcript) into an on-disk archive tree **and** set SQLite `archived`
**C)** Hide from the active list in SQLite with no archive view and no mutation lock beyond “hidden”

**Recommend:** A — files preserved in place; status drives archive view and inactive behavior.
Which do you want?

**Answer:** User direction: metadata and most things in SQLite, including transcripts. Archive = status/metadata in SQLite; prefer storing session metadata + transcripts in SQLite; on-disk queue/protocol files still needed for agent compatibility. Expand SQLite construction beyond session-list-only. (Do not invent full schema beyond settled sketch.)

## q-019

Answer-processor invocation stays question id + one answer text string. When the user combines an MC selection with extra free text in one submit, how should that single string be encoded?

**A)** Human-readable: selected option line first (e.g. `B) …`), then a blank line, then the free text
**B)** Machine delimiter: `option:<key>` on the first line, remaining lines are free text
**C)** JSON object string as the entire answer text (e.g. `{"option":"B","text":"…"}`)

**Recommend:** A — readable in the transcript; matches how options already appear to agents.
Which do you want?

**Answer:** Question was insufficient. User wants a full answer-processor submission protocol: include question id/number; compact, concise, unambiguous; not required to be JSON; support multiple answers in one payload; keep simple so answer-processor agent can be specified against it. User invited agent to come up with the construction. Proposed YAML front-matter + free-text body multi-record protocol written into `design.md`; q-019 rewritten as Accept/Modify/Reject of that protocol (left open).

## q-020

Queue entries are JSON files. Which required object shape should agents and the UI treat as the v1 schema?

**A)** Minimal: `id`, `created` (ISO-8601), `text`; optional `options` as `[{ "key": "A", "label": "…" }, …]`; optional `modified`
**B)** Richer on-disk: A’s fields plus UI/agent workflow fields in the file (`pending`, `deferred`, display rank, etc.)
**C)** Split: question body markdown file + sibling `*.options.json` (id/text only in the main JSON)

**Recommend:** A — pending/defer live in UI or agent runs, not as required queue-file fields.
Which do you want?

**Answer:** User said “GAML” → treat as YAML front matter. Revisit JSON (q-013): queue files use YAML front matter + free-text body instead of JSON. Metadata in front matter; prose unescaped in body. Format may support multiple YAML documents in one file (`---` separators), but keep one file per open question for concurrent answer processing. MC options live in YAML front matter (`options:` list), not a JSON array.

## q-021

For the shared SQLite DB, which session-list schema sketch should v1 use?

**A)** One `interview_sessions` table: display name, status enum `active` | `archived` | `complete`, paths to scratchpad/transcript/config, created/updated timestamps
**B)** Separate tables for active vs archived sessions (complete as a flag or third table)
**C)** SQLite holds display labels only; active/archived/complete and paths are derived by scanning the scratchpad tree each launch

**Recommend:** A — one row per session; status + paths are first-class (matches “SQLite is the index”).
Which do you want?

**Answer:** Agent chooses best approach — A: one `interview_sessions` table (display name, status active|archived|complete, paths, timestamps). Align with expanded SQLite usage from q-018 where sensible (transcripts may be additional tables — note briefly; details can be planning).

## q-022

Where should agent-run failure UI and recovery controls live?

**A)** Contextual: answer-processor errors on that question (re-enable submit + show error); researcher failures in a session-level status strip with retry / manual kickoff after three auto-retries
**B)** Toasts/notifications only — no persistent in-view error chrome on the question or session strip
**C)** Separate Errors panel/tab listing all run failures, with recovery only there

**Recommend:** A — matches “errors visible” and per-question re-enable vs researcher kickoff.
Which do you want?

**Answer:** Align with tod project requirement: operation failures use toast or banner (tod `user.md` req 11 — more visible than status area). Interview agent errors use the same toast/banner pattern as tod; recovery controls remain available (re-enable submit / researcher kickoff) in context. Not a separate Errors panel; not toasts-only without recovery.

## q-009

Cursor CLI (`agent -p --model auto`) is already the v1 launch path. Before we treat billing as settled, and before we decide whether a heavier protocol belongs in v1, pick one:

1. **A)** Before shipping, just run one test agent with Auto and check the Cursor usage dashboard once (recommended) — ACP stays a later upgrade, not required for the simple CLI print-mode adapter
2. **B)** The app itself should block agent runs until the user confirms billing / subscription usage
3. **C)** Implement the heavier ACP protocol in v1 instead of simple CLI print-mode (exit/stdout observation)

**Recommend:** A — one live check; do not block the UI on billing chrome or ship ACP first.
Which do you want?

**Answer:** C / ACP — User completed spike: ACP worked successfully with a test script. Change Cursor v1 launch construction from CLI print-mode to ACP (Agent Client Protocol over Cursor agent). Update spike decision tree: ACP is settled for v1, not post-v1. Keep subscription + Auto. Spike path: `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/`.

## q-019

Accept, modify, or reject the answer-processor submission protocol (YAML front matter + optional free-text body multi-record payload; `id` required; `option` optional).

**A)** Accept — keep this protocol as written
**B)** Modify — keep the approach; state exact edits
**C)** Reject — do not use this protocol; state what to use instead

**Recommend:** A

**Answer:** A Accept. Confirm `option` is optional (omit when no MC selection). Protocol accepted (remove “pending Accept”).

## q-023

Accept, modify, or reject researcher submission protocol for consider / defer / more-options (same multi-record YAML as answer-processor, but `action` not `option`).

**A)** Accept
**B)** Modify
**C)** Reject

**Recommend:** A

**Answer:** Write construction — same multi-record YAML protocol as answer-processor; researcher actions use `action` (not `option`) — e.g. `action: defer` | `reconsider` | `more-options`, plus `id`, optional free-text body.

## q-024

How should entity `history/*.md` transcripts coexist with SQLite in v1?

**A)** Dual-write
**B)** Markdown `history/` only for agents; SQLite metadata + path
**C)** SQLite primary; materialize markdown when agents need it

**Recommend:** A

**Answer:** SQLite is primary for transcripts this app writes. Markdown/history may still exist elsewhere — do not delete those; not a dual-write requirement. Primary concern: write to DB.

## q-025

Researcher replenishment thresholds persist in a separate config file under `.local/`. Which path/construction should v1 use?

**A)** `.local/config/tod/settings.toml` (or `.json`)
**B)** `.local/data/tod/settings.toml` (or `.json`)
**C)** `.local/config/interview-ui/settings.toml` (or `.json`)

**Recommend:** A

**Answer:** Treat `.local` as home (XDG-style). Persistent tod files under `.local/.config/tod/` (not `.local/config/...`). Settings = JSON file there. Move SQLite to live under `.local/.config/tod/` as well (user override of earlier `.local/data/tod`).

## q-026

Deep-dive closure when an answer is found — how should it work?

**A)** Explicit UI control — confirm “submit to parent”
**B)** Structured agent signal — auto-submit
**C)** Hybrid — agent signal + one-click confirm

**Recommend:** A

**Answer:** Redesign deep-dive closure UX: No auto-detect of agent “answer ready”. Choosing Deep dive does not change the main interview question UI (inputs stay); opens deep-dive separately elsewhere. In the deep-dive view: user can select text from transcript (or similar) and use a “Use this” / copy-into-answer control that pastes into the parent question’s answer text area. User may edit that text, then submit the parent question normally. Supersede any auto-submit construction.

## q-027

How should in-flight and success status for researcher / answer-processor runs appear?

**A)** Contextual only — quiet success
**B)** Persistent session status strip
**C)** Toast/banner for success and failure

**Recommend:** A

**Answer:** In-flight/success: use tod’s persistent status location for success (and status-ish history later — not implementing full notification history yet). Errors → toast/banner. Quiet success via status is fine for now.

## q-028

When should the new session row appear in SQLite on interview launch?

**A)** UI inserts the row when it starts the launch run; updates paths when config/transcript appear
**B)** UI watches for scaffolding then inserts
**C)** Insert only after researcher exit succeeds

**Recommend:** B

**Answer:** A — Insert SQLite session row on kickoff; update paths later when scaffolding appears.

## q-029

Who writes entity `history/*.md` interview transcripts in v1, given SQLite is the primary store for transcripts this app writes?

**A)** Agents (answer-processor / researcher) keep appending markdown under `history/`; UI writes SQLite only — no UI dual-write
**B)** UI writes SQLite only; agents stop writing markdown transcripts
**C)** UI dual-writes SQLite and markdown `history/` itself

**Recommend:** A — keeps agent protocol / transcript path compatible; “not dual-write” means the UI does not write both.
Which do you want?

**Answer:** History transcripts (`history/*.md`) are written **only by the UI**. **Not** stored in SQLite. Agents must **stop writing** transcripts (process/agent changes in scope for this task). Revise constructions that said SQLite is primary for transcripts — SQLite still holds session metadata and other durable UI data; transcripts = UI-owned markdown history only.

## q-030

Exact filename for the JSON settings file under `.local/.config/tod/` (researcher replenishment thresholds)?

**A)** `settings.json`
**B)** `tod.json`
**C)** Other — name the filename

**Recommend:** A — conventional; same directory as SQLite.
Which do you want?

**Answer:** Settings file = **`tod.yml`** (YAML, not JSON) under `.local/.config/tod/`. YAML vs JSON settled as YAML/`tod.yml`.

## q-031

I’ve reviewed the current design constructions for this phase. These top-level areas look complete and coherent for a reasonable Interview UI of this kind — I don’t see a compelling gap to propose next:

1. App navigation / mounting in tod
2. Agent provider boundary + Cursor ACP launch (subscription + Auto)
3. Local home layout (`.local/.config/tod/`) — SQLite persistence + JSON settings
4. Session list / archive / launch form / finished state / SQLite row timing
5. Answer-processor and researcher multi-record YAML submission protocols
6. Question list UX (master-detail, consider/defer/more-options, answer submit, MC key binding)
7. Deep-dive (context, no special scaffolding, copy-into-parent closure)
8. Queue watcher (`notify`) + YAML front-matter queue files
9. Agent-run in-flight/success status + failure toast/banner with recovery

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** Completeness = **not done**. Visual design is needed in this process; asked how. Not A. Record visual design as an open design-phase need; rewrite q-031 to propose how visual design fits (options), recommend A (HTML mockups as design artifacts) + optionally park B as process improvement.

---

## q-031 (resolved)

How should visual design fit this task / process?

**A)** HTML mockups under doc/mockups, linked from design.md
**B)** Extend process skill (design-agent / gates) to require visual-design step
**C)** Separate frontend-design spike/session
**D)** Other

**Answer:** Update the process for visual design. Separate **visual design agent** that walks through mockups. Artifacts referenced from `design.md` (not embedded). Artifact home: **task `artifacts/`** (for this task: `doc/process/projects/interview-ui/tasks/core-ui/artifacts/`) — under the task scope, not a parallel project `doc/visual/` tree. Process skill updated accordingly (visual-design-agent, design-agent handoff, design→planning checklist). Path corrected 2026-08-23 after human feedback.

<!-- answer-anchor: q-031 -->

---

## Human design decisions (2026-08-23 ~1637) — no queue id

Human volunteered two constructions (settled intent; recorded into `design.md`):

1. **Ctrl+Enter submit** — When focus is in the question free-form answer text area, Ctrl+Enter submits that text as the answer for that question (same submit path as the Submit control).

2. **Auto-select next question** — Immediately after the user does something on a question that submits it (answer submit or other submit-like actions that put the question pending / remove it from active work), the UI selects the next question in the list.

Note: visual package still draft / not Accepted; these may need mockup affordance updates later (Ctrl+Enter hint; next-select behavior). Design interview completeness re-opened — researcher replenish requested.

