# Core UI — design

## Intention

Design how Interview UI mounts in the existing tod GPUI app, talks to agents through a swappable provider, persists session metadata, presents the question queue, and handles per-question actions (consider, defer, more-options, deep-dive) without inventing a separate reject path.

## Constructions

| Concern | Construction |
|--|--|
| App navigation / mounting | Replace the main content area; add minimal navigation (tab or menu) to swap task list ↔ Interview UI in the same window. Do not use a separate window or overlay/panel for v1. |
| Agent provider boundary | Interview primary code talks only to an **agent provider interface**. Backends are swappable (Cursor now; Claude later). Cursor is the only v1 backend. |
| Cursor usage constraints | Must work with a **Cursor subscription** (not pay-per-token). User uses the **Auto** model only (special usage availability). |
| Agent launch mechanism (Cursor v1) | **ACP (Agent Client Protocol)** over the Cursor agent — not CLI print-mode (`agent -p`). **Researcher:** fresh ACP session per replenishment run (max two concurrent). **Answer processor:** **session pool** of long-lived ACP sessions — reuse until answers-per-session limit, spawn another when busy or at capacity. Observe in-flight / success / failure via ACP session lifecycle. Keep subscription + Auto. User spike verified ACP with a test script: `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/`. Not the TS/Python SDK sidecar for v1. Prior print-mode spike: `doc/process/projects/interview-ui/doc/spike-cursor-agent-launch.md` (superseded for v1 launch path). |
| Answer-processor session pool | Pool of reusable ACP sessions behind the agent provider. **Settings** (in `tod.yml` + Settings UI): **maximum session pool size** (default 4) — cap on concurrent open answer-processor sessions; **answers per session** (default 4) — after the **Nth response is received and processed** on one session, **immediately close** that session (recycle on response completion, not on submit). **Assign on submit:** prefer idle session; if target session in flight on next submit, use another idle session or open new (until pool max); if pool full and all busy, queue locally and drain as sessions idle. **Workspace UX:** remove global answer-in-flight lock — only per-question pending disables that question (project req 7a). Not one-shot spawn/kill per answer. |
| Answer-processor pool status | Interview workspace **status footer**, **right side**: `{active} active / {in_pool} in pool / {max} max` — active = in-flight prompts; in pool = open sessions; max = configured pool size. Left side keeps existing status message (project req 11a). |
| Local home / XDG-style layout | Treat repo **`.local`** as the XDG-style home for tod. Persistent tod files live under **`.local/.config/tod/`** (not `.local/config/...`, not `.local/data/tod`). |
| Session / metadata persistence | Shared **SQLite** under **`.local/.config/tod/`** holds session metadata and other durable interview data the UI owns (**not** transcript bodies). On-disk queue/config/protocol files remain for agent compatibility. Not session-list-only; not derive-only from filesystem scan; not UI fields bolted only onto `interview-config.md`. Full schema beyond settled sketch deferred to planning. |
| Interview transcripts (`history/*.md`) | Written **only by the UI** as markdown under entity `history/`. **Not** stored in SQLite. Agents (answer-processor / researcher) **stop writing** transcripts — process/agent changes for that are **in scope for this task**. |
| SQLite session-list schema (v1 sketch) | One **`interview_sessions`** table: display name; status enum `active` \| `archived` \| `complete`; paths to scratchpad/transcript/config; created/updated timestamps. No transcript-body tables in SQLite. |
| New session SQLite row timing | On interview kickoff, UI **inserts** the `interview_sessions` row immediately (status e.g. active/pending paths); **updates paths later** when scaffolding (config/transcript/scratchpad) appears on disk. |
| Interview finished state | **No separate Complete screen.** Show in-place **Complete** only when the bound queue has **no open questions** and replenishment is not waiting (researcher returned no further questions). SQLite `complete` must not short-circuit the UI into Complete while open questions are loaded; if questions reappear, show question body UI and allow answering (reopen Active as needed). Session list still reflects complete status when truly finished. Not a blocking modal; not a dedicated finished-panel screen. |
| Researcher threshold settings | **Global Settings UI**; persist the two replenishment thresholds (defaults 8 and 2) in **`tod.yml`** (YAML, not JSON) under **`.local/.config/tod/`** (same tree as SQLite; not SQLite itself). |
| Answer-processor pool settings | Same Settings UI / **`tod.yml`** tree as researcher thresholds. Two number fields: **maximum session pool size** (default 4), **answers per session** (default 4). Visual: extend accepted `artifacts/visual/settings/` (same row layout as researcher thresholds). |
| Archive persistence | Archive = **SQLite status/metadata** (`archived`); session files stay in place; archive view filters on status; mutation (answer submit / replenish) blocked while archived. Prefer metadata in SQLite; transcripts remain UI-owned markdown in `history/`; on-disk queue/protocol files still required for agents. |
| Answer-processor submission protocol | **Accepted.** One or more concatenated **answer records**. Each record is YAML front matter + optional free-text body, terminated by the next record’s `---` or end of payload. Front matter: `id` required (e.g. `q-016`); `option` **optional** (omit when no MC selection; when present, digit string `"1"` / `"2"` / …). Body after closing `---` is free text (may be empty). Multiple answers = multiple units in one submission; single-answer submit is one unit. Not required to be JSON. |
| Researcher action submission protocol | Same multi-record YAML shape as answer-processor, but researcher actions use **`action`** (not `option`). Front matter: `action` required (`defer` \| `reconsider` \| `more-options`); `id` required; optional free-text body. Multiple actions = multiple units; single action is one unit. UI marks the question pending (like answer submit); researcher deletes or modifies the queue file. |
| Open-question list layout | **Three-column interview workspace** — scrollable question list (**single-line** id + short label; ellipsis OK) \| full question body \| **flexing** response pane (MC → Notes → action dropdown + Submit). Visual: Accepted `artifacts/visual/interview-workspace/` (row shape and no MC truncation per project `user.md` 20/23). |
| Selection visibility | Session-list and question-list selected rows, MC options, and response focus stops use **`gpui_component::list::ListItem` `.selected(...)`** — same control/tokens as tod task list (`theme.list_active` / `list_active_border`). Do not hand-paint accent-tint selection. Cite `refs/process/other/ux-design.md` Color and contrast / States and feedback / Actionable lists. |
| Workspace focus and input model | Focus regions: (A) question list, (B) response column controls in visual order: MC options (top→bottom) → Notes → action dropdown → Submit (and any other interactive response controls in that order). **Middle question-body column has nothing interactive and no keyboard focus stop** (Left ↔ Right only). Visible focus chrome on list row and focused response control (meets selection-visibility bar). **Right** from list → uppermost response control; **Left** from response → list; **Up/Down** among response controls when response-focused. |
| Workspace Escape | Escape does **not** navigate to the session list. Escape exits Notes edit mode when editing; otherwise no-op (unless nested workspace chrome dismisses). Leave via explicit **Back to interviews** (or equivalent). |
| MC option keys | Digits **only**. `options[].key` values are decimal digit strings `"1"`, `"2"`, `"3"`, … matching on-screen labels. **No** letter-key MC support (no dual-accept, no A→1 mapping). UI binds digit keys to **immediate submit** when not in Notes edit mode. Researcher / any agent-authored MC must emit digit keys via updated skills, prompts, and protocol examples. Space/Enter on focused MC submits; mouse click on MC submits immediately. |
| Independent review F1–F6 | **In scope** for this same redesign→implement cycle. If not already fixed, fix in planning/active. Work items remain in `.local/.../journal/2026-08-25-review-independent.md` for planning/active disposition (not open design questions). |
| Notes edit mode | Focus stop on Notes ≠ edit mode. Keyboard: Enter/Space enter edit; Escape exits edit (stays on Notes, not session list). While editing: arrows = text navigation; digit MC shortcuts inactive; **Ctrl+Enter** submits. Mouse click on Notes enters edit immediately. |
| Response column sizing | Flexing response column that shares remaining width after list + body (exact flex recipe open to planning). **Supersedes** prior “compact fixed-width response” construction where that caused truncation. MC option rows: wrap text; no ellipsis truncation on option labels. Notes width tracks the response column. Horizontal fill required; vertical window fill not required. |
| Question action: Reject | **Out.** Do not implement Reject. Replace with a **Consider / Reconsider**-style action (wording may settle as “Consider”). |
| Question action: Consider / Reconsider | Submit consider/reconsider intent to the **researcher** (not the answer-processor) via researcher action protocol (`action: reconsider`). |
| Question action: Defer | Submit defer intent to the **researcher** (not the answer-processor) via researcher action protocol (`action: defer`). |
| Question action: More options | Submit request-more-options to the **researcher** via researcher action protocol (`action: more-options`). |
| Other actions chrome | Deep-dive, defer, and reconsider/consider are the **other actions** set — not unique chrome placements. Prefer a dropdown / “Run other action” (or similar) for those actions, separate from multiple-choice option selection. |
| Answer submit UX | User may submit free-text alone; submit an MC option alone (digit key, Space/Enter on focused option, or click); combine MC + extra text in one submit when applicable; must be able to submit text without choosing an MC option. Digit/click/Space/Enter MC paths submit immediately (not select-only). |
| Non-blocking submit | Answer submit and researcher-action submit are **optimistic on the UI thread**: mark pending, clear response fields, auto-select next question, then run transcript write + agent dispatch on a **background thread**. Failures re-enable the question and show toast/banner error (project req 7b). |
| Ctrl+Enter submit | **While editing Notes**, **Ctrl+Enter** submits the current answer (same submit path as Submit). Single Ctrl+Enter rule for Notes (see Notes edit mode). |
| Auto-select next question | Immediately after the user does something on a question that submits it (answer submit or other submit-like actions that put the question pending / remove it from active work), the UI selects the **next** question in the list. |
| Deep-dive context | When starting a deep-dive, supply rich context: (1) current project and task, (2) current lifecycle state, (3) purpose of this interview, (4) interview phase (initial / design / planning). |
| Deep-dive branch scaffolding | **No special on-disk scaffolding.** Deep-dive is an ordinary agent chat session: UI sends settled context, then ordinary chat — no dedicated transcript / config / scratchpad artifacts required specifically for the branch. |
| Deep-dive closure UX | **No auto-detect** of agent “answer ready”; **no auto-submit** to the parent question. Choosing Deep dive does **not** change the main interview question UI (inputs stay); opens deep-dive **separately elsewhere**. In the deep-dive view: user can select text from the transcript (or similar) and use a **“Use this” / copy-into-answer** control that pastes into the **parent question’s answer text area**. User may **edit** that text, then submit the parent question normally via the answer-processor payload. Supersedes any auto-submit / machine-signal closure construction. |
| Queue directory watcher | **`notify` only** (or equivalent) — OS filesystem events with short debounce; no polling primary path. Prefer notify-only unless Windows notify buffer overflow becomes a real problem (that is the usual argument for hybrid). |
| Queue entry format | Queue files use **YAML front matter + free-text body** (not JSON). Metadata in front matter; prose unescaped in the body. Format may support multiple YAML documents in one file (`---` separators) as a capability; **queue still one file per open question** for concurrent answer processing. Config remains markdown for agents; transcripts are UI-owned `history/*.md` (see persistence). |
| MC option binding for keyboard | MC options live in YAML front matter (`options:` list with digit `key` / `label` only); body/prose is display-only. Update researcher (and related) agent skills/prompts/examples so new queue files never emit letter keys. |
| Interview launch UI | **No separate launch screen.** New interview is started from the **session list / menu** (entity picker + purpose live in that menu flow or lightweight in-menu controls — not a dedicated Launch page). UI builds a typed prompt/payload for the researcher; UI does not create session scaffolding itself (researcher does). UI still inserts the SQLite session row on kickoff (see New session SQLite row timing). |
| Unbootstrapped session (open) | Before opening the workspace, if the session has **no valid bound** `config_path` / `interview-config.md` on disk **and** researcher bootstrap is **not already in flight** for that session, the UI **must not** auto-start bootstrap. Show the **bootstrap confirmation toast** (below) instead. |
| Bootstrap confirmation toast | Standard **`confirm_toast`** helper (`crates/tod/src/ui/toast.rs`): gpui-component **warning** notification, **non-autohide**, title *Interview not set up*, message *“{entity label} has not been set up yet. Do you want me to set it up?”* Entity label from session display name (text before em dash) or entity path basename. **No:** dismiss toast; stay on session list; do not open workspace. **Yes:** reactivate session if needed (`complete` → `active`), start researcher bootstrap, open workspace (bootstrap-in-progress status until scaffolding binds). Aligns with tod `user.md` req 11 (toast for user-visible operations) and project req 19. |
| New kickoff auto-bootstrap | Explicit **new interview launch** from the compose flow (e.g. Shift+Enter) is **exempt** from the confirmation toast — the user just requested creation; bootstrap starts immediately without prompting. Opening that session while bootstrap is in flight proceeds directly to the workspace. |
| Workspace self-healing (bootstrap failed) | If the workspace is open with `scaffolding_pending` and **no bootstrap in flight** (failed, timed out, or app restarted mid-bootstrap), close back to the session list and show the same **bootstrap confirmation toast** so the user can retry. Must not leave the user stuck on “Waiting for researcher scaffolding” indefinitely. |
| Agent-run failure UI | **Errors → toast/banner** (tod `user.md` req 11 — more visible than the status area). Interview agent errors use the same toast/banner pattern as tod; **recovery controls remain available in context** (re-enable submit / researcher kickoff). Not a separate Errors panel; not toasts-only without recovery. |
| Agent-run in-flight / success status | Use tod’s **persistent status location** (tod `user.md` req 10 — status area) for in-flight and **success** (and status-ish history later — not implementing full notification history yet). Quiet success via status is fine for now. Do not toast success. |
| Visual design (process) | Separate **visual design agent** for conversational co-design (agent builds/iterates with human; Whimsical / HTML / sketches). Not a lifecycle state; available in design phase or on demand. Accepted packages under task **`artifacts/`** (prefer `visual/{screen}/`); linked from this `design.md` Links as **required** when Accepted. |

### Answer-processor submission protocol (accepted — q-019)

Payload = one or more concatenated answer records. Each record is YAML front matter + optional free-text body, terminated by the next record’s `---` or end of payload:

```
---
id: q-016
option: "1"
---
Optional free-text notes for this answer.

---
id: q-017
option: "2"
---

---
id: q-018
---
Text-only answer with no MC option.
```

Rules: `id` required; `option` **optional** (omit when no MC selection; when present use digit strings `"1"` / `"2"` / …); body after closing `---` is free text (may be empty); multiple answers = multiple units; single-answer submit is one unit; not required to be JSON.

### Researcher action submission protocol (accepted — q-023)

Same multi-record YAML shape; researcher actions use **`action`** (not `option`):

```
---
action: defer
id: q-016
---
Optional notes for the researcher.

---
action: reconsider
id: q-017
---

---
action: more-options
id: q-018
---
Prefer options that emphasize trade-offs.
```

Rules: `action` required (`defer` \| `reconsider` \| `more-options`); `id` required; body after closing `---` is free text (may be empty); multiple actions = multiple units; single action is one unit; not required to be JSON.

### Spike decision tree — Cursor interaction (resolved — ACP for v1)

**Spike:** How does the UI start researcher / answer-processor runs against Cursor under subscription + Auto-only?

**Settled for v1:** **ACP** (Agent Client Protocol over Cursor agent), not CLI print-mode. Keep **subscription + Auto**. User completed spike: ACP worked successfully with a test script — `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/`. ACP is **in v1**, not a post-v1 upgrade.

| Outcome | Action |
|--|--|
| ACP + Auto + subscription works (user spike verified) | **Adopt ACP as v1 Cursor backend** behind the agent provider interface |
| ACP works but Auto pool does not draw as expected | Stay on ACP; revisit auth/model selection; do not silently switch to pay-per-token or a non-Auto model |
| ACP install/auth too fragile on target machines | Consider SDK Node sidecar or CLI print-mode as fallback behind the same interface |
| Print-mode preferred for a thinner adapter | Superseded — do **not** ship print-mode as v1 primary; ACP is settled |

## Open design needs

| Need | Notes |
|--|--|
| — | None. Legacy MC = digits only (no letter support); F1–F6 in-scope for this cycle; middle column confirmed non-focusable. Planning handoff: revise `plan.md` for H4–H8 / digits / Complete-vs-queue / F1–F6 work items (not a design open need). |

## Links / external references

| Link | Scope | Binding |
|--|--|--|
| `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/` | User ACP spike (verified working with test script) | required |
| `doc/process/projects/interview-ui/doc/spike-cursor-agent-launch.md` | Earlier Cursor launch spike (CLI vs SDK vs ACP); v1 launch path superseded by ACP | guideline |
| `refs/process/other/ux-design.md` | Color and contrast / States and feedback / Actionable lists — selection & focus visibility bar | guideline (obligations stated in project `user.md` §19) |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/interview-workspace/` | Accepted visual — question list + body + answer (`preview.png`); single-line rows + no MC ellipsis per updated reqs | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/sessions-menu/` | Accepted visual — interview sessions menu / list (`preview.png`, `source.canvas.tsx`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/deep-dive/` | Accepted visual — deep-dive chat + Use this (`preview.png`, `source.canvas.tsx`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/complete-inplace/` | Accepted visual — in-place Complete in interview workspace (`preview.png`, `source.canvas.tsx`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/settings/` | Accepted visual — researcher threshold settings (`preview.png`, `source.canvas.tsx`) | required |
| `crates/tod/src/ui/toast.rs` | Shared Yes/No confirmation toast (`confirm_toast`) — bootstrap setup prompt | required |
