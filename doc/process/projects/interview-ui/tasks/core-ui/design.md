# Core UI — design

## Intention

Design how Interview UI mounts in the existing tod GPUI app, talks to agents through a swappable provider, persists session metadata, presents the question queue, and handles per-question actions (consider, defer, more-options, deep-dive) without inventing a separate reject path.

## Constructions

| Concern | Construction |
|--|--|
| App navigation / mounting | Replace the main content area; add minimal navigation (tab or menu) to swap task list ↔ Interview UI in the same window. Do not use a separate window or overlay/panel for v1. |
| Agent provider boundary | Interview primary code talks only to an **agent provider interface**. Backends are swappable (Cursor now; Claude later). Cursor is the only v1 backend. |
| Cursor usage constraints | Must work with a **Cursor subscription** (not pay-per-token). User uses the **Auto** model only (special usage availability). |
| Agent launch mechanism (Cursor v1) | **ACP (Agent Client Protocol)** over the Cursor agent — not CLI print-mode (`agent -p`). Fresh session per researcher / answer-processor run; observe in-flight / success / failure via ACP session lifecycle. Keep subscription + Auto. User spike verified ACP with a test script: `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/`. Not the TS/Python SDK sidecar for v1. Prior print-mode spike: `doc/process/projects/interview-ui/doc/spike-cursor-agent-launch.md` (superseded for v1 launch path). |
| Local home / XDG-style layout | Treat repo **`.local`** as the XDG-style home for tod. Persistent tod files live under **`.local/.config/tod/`** (not `.local/config/...`, not `.local/data/tod`). |
| Session / metadata persistence | Shared **SQLite** under **`.local/.config/tod/`** holds session metadata and other durable interview data the UI owns (**not** transcript bodies). On-disk queue/config/protocol files remain for agent compatibility. Not session-list-only; not derive-only from filesystem scan; not UI fields bolted only onto `interview-config.md`. Full schema beyond settled sketch deferred to planning. |
| Interview transcripts (`history/*.md`) | Written **only by the UI** as markdown under entity `history/`. **Not** stored in SQLite. Agents (answer-processor / researcher) **stop writing** transcripts — process/agent changes for that are **in scope for this task**. |
| SQLite session-list schema (v1 sketch) | One **`interview_sessions`** table: display name; status enum `active` \| `archived` \| `complete`; paths to scratchpad/transcript/config; created/updated timestamps. No transcript-body tables in SQLite. |
| New session SQLite row timing | On interview kickoff, UI **inserts** the `interview_sessions` row immediately (status e.g. active/pending paths); **updates paths later** when scaffolding (config/transcript/scratchpad) appears on disk. |
| Interview finished state | **No separate Complete screen.** When the question list is empty and replenishment is not waiting (researcher returned no further questions), the interview workspace shows an in-place **Complete** (or similar) message in that view. Session list still reflects complete status. Not a blocking modal; not a dedicated finished-panel screen. |
| Researcher threshold settings | **Global Settings UI**; persist the two replenishment thresholds (defaults 8 and 2) in **`tod.yml`** (YAML, not JSON) under **`.local/.config/tod/`** (same tree as SQLite; not SQLite itself). |
| Archive persistence | Archive = **SQLite status/metadata** (`archived`); session files stay in place; archive view filters on status; mutation (answer submit / replenish) blocked while archived. Prefer metadata in SQLite; transcripts remain UI-owned markdown in `history/`; on-disk queue/protocol files still required for agents. |
| Answer-processor submission protocol | **Accepted.** One or more concatenated **answer records**. Each record is YAML front matter + optional free-text body, terminated by the next record’s `---` or end of payload. Front matter: `id` required (e.g. `q-016`); `option` **optional** (omit when no MC selection). Body after closing `---` is free text (may be empty). Multiple answers = multiple units in one submission; single-answer submit is one unit. Not required to be JSON. |
| Researcher action submission protocol | Same multi-record YAML shape as answer-processor, but researcher actions use **`action`** (not `option`). Front matter: `action` required (`defer` \| `reconsider` \| `more-options`); `id` required; optional free-text body. Multiple actions = multiple units; single action is one unit. UI marks the question pending (like answer submit); researcher deletes or modifies the queue file. |
| Open-question list layout | **Three-column interview workspace** — scrollable question list (id + short label) \| full question body \| compact response pane (MC → Notes → action dropdown + Submit). Visual: Accepted `artifacts/visual/interview-workspace/`. |
| Question action: Reject | **Out.** Do not implement Reject. Replace with a **Consider / Reconsider**-style action (wording may settle as “Consider”). |
| Question action: Consider / Reconsider | Submit consider/reconsider intent to the **researcher** (not the answer-processor) via researcher action protocol (`action: reconsider`). |
| Question action: Defer | Submit defer intent to the **researcher** (not the answer-processor) via researcher action protocol (`action: defer`). |
| Question action: More options | Submit request-more-options to the **researcher** via researcher action protocol (`action: more-options`). |
| Other actions chrome | Deep-dive, defer, and reconsider/consider are the **other actions** set — not unique chrome placements. Prefer a dropdown / “Run other action” (or similar) for those actions, separate from multiple-choice option selection. |
| Answer submit UX | User may submit free-text alone; select an MC option alone; combine MC selection + extra text in one submit; must be able to submit text without choosing an MC option. |
| Ctrl+Enter submit | When focus is in the question free-form answer text area, **Ctrl+Enter** submits that text as the answer for that question (same submit path as the Submit control). |
| Auto-select next question | Immediately after the user does something on a question that submits it (answer submit or other submit-like actions that put the question pending / remove it from active work), the UI selects the **next** question in the list. |
| Deep-dive context | When starting a deep-dive, supply rich context: (1) current project and task, (2) current lifecycle state, (3) purpose of this interview, (4) interview phase (initial / design / planning). |
| Deep-dive branch scaffolding | **No special on-disk scaffolding.** Deep-dive is an ordinary agent chat session: UI sends settled context, then ordinary chat — no dedicated transcript / config / scratchpad artifacts required specifically for the branch. |
| Deep-dive closure UX | **No auto-detect** of agent “answer ready”; **no auto-submit** to the parent question. Choosing Deep dive does **not** change the main interview question UI (inputs stay); opens deep-dive **separately elsewhere**. In the deep-dive view: user can select text from the transcript (or similar) and use a **“Use this” / copy-into-answer** control that pastes into the **parent question’s answer text area**. User may **edit** that text, then submit the parent question normally via the answer-processor payload. Supersedes any auto-submit / machine-signal closure construction. |
| Queue directory watcher | **`notify` only** (or equivalent) — OS filesystem events with short debounce; no polling primary path. Prefer notify-only unless Windows notify buffer overflow becomes a real problem (that is the usual argument for hybrid). |
| Queue entry format | Queue files use **YAML front matter + free-text body** (not JSON). Metadata in front matter; prose unescaped in the body. Format may support multiple YAML documents in one file (`---` separators) as a capability; **queue still one file per open question** for concurrent answer processing. Config remains markdown for agents; transcripts are UI-owned `history/*.md` (see persistence). |
| MC option binding for keyboard | MC options live in YAML front matter (e.g. `options:` list with `key` / `label`); body/prose is display-only. Agents author that list so the UI can bind keys. |
| Interview launch UI | **No separate launch screen.** New interview is started from the **session list / menu** (entity picker + purpose live in that menu flow or lightweight in-menu controls — not a dedicated Launch page). UI builds a typed prompt/payload for the researcher; UI does not create session scaffolding itself (researcher does). UI still inserts the SQLite session row on kickoff (see New session SQLite row timing). |
| Agent-run failure UI | **Errors → toast/banner** (tod `user.md` req 11 — more visible than the status area). Interview agent errors use the same toast/banner pattern as tod; **recovery controls remain available in context** (re-enable submit / researcher kickoff). Not a separate Errors panel; not toasts-only without recovery. |
| Agent-run in-flight / success status | Use tod’s **persistent status location** (tod `user.md` req 10 — status area) for in-flight and **success** (and status-ish history later — not implementing full notification history yet). Quiet success via status is fine for now. Do not toast success. |
| Visual design (process) | Separate **visual design agent** for conversational co-design (agent builds/iterates with human; Whimsical / HTML / sketches). Not a lifecycle state; available in design phase or on demand. Accepted packages under task **`artifacts/`** (prefer `visual/{screen}/`); linked from this `design.md` Links as **required** when Accepted. |

### Answer-processor submission protocol (accepted — q-019)

Payload = one or more concatenated answer records. Each record is YAML front matter + optional free-text body, terminated by the next record’s `---` or end of payload:

```
---
id: q-016
option: A
---
Optional free-text notes for this answer.

---
id: q-017
option: B
---

---
id: q-018
---
Text-only answer with no MC option.
```

Rules: `id` required; `option` **optional** (omit when no MC selection); body after closing `---` is free text (may be empty); multiple answers = multiple units; single-answer submit is one unit; not required to be JSON.

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
| — | None. Visual packages Accepted; ready for planning. |

## Links / external references

| Link | Scope | Binding |
|--|--|--|
| `doc/process/projects/interview-ui/doc/spikes/acp-auto-billing-test/` | User ACP spike (verified working with test script) | required |
| `doc/process/projects/interview-ui/doc/spike-cursor-agent-launch.md` | Earlier Cursor launch spike (CLI vs SDK vs ACP); v1 launch path superseded by ACP | guideline |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/interview-workspace/` | Accepted visual — question list + body + answer (`preview.png`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/sessions-menu/` | Accepted visual — interview sessions menu / list (`preview.png`, `source.canvas.tsx`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/deep-dive/` | Accepted visual — deep-dive chat + Use this (`preview.png`, `source.canvas.tsx`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/complete-inplace/` | Accepted visual — in-place Complete in interview workspace (`preview.png`, `source.canvas.tsx`) | required |
| `doc/process/projects/interview-ui/tasks/core-ui/artifacts/visual/settings/` | Accepted visual — researcher threshold settings (`preview.png`, `source.canvas.tsx`) | required |
