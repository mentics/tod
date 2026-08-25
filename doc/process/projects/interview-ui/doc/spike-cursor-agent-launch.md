# Spike: Cursor agent launch for Interview UI

**Date:** 2026-08-23  
**Task:** `interview-ui` / `core-ui`  
**Scope:** How tod (Rust GPUI) should programmatically start Cursor agents (researcher / answer-processor) under subscription + Auto constraints.  
**Non-scope:** Final shape of the swappable agent-provider interface (sketch only); Claude adapter.

## Goal constraints (from product)

| Constraint | Implication |
| --- | --- |
| Subscription billing (no separate token-only pay path) | Prefer IDE/CLI/SDK paths that debit the **user plan / request pools**, not a distinct Cloud API meter unless proven identical |
| **Auto** model only (special availability) | Must select Auto explicitly; do not hard-depend on Composer / third-party model IDs |
| Fresh session per researcher replenishment and per answer-processor submit (v1) | One new conversation per run; no `--resume` / `Agent.resume` in v1 |
| Input ≈ config path + short prompt | Prompt text can point at `interview-config.md` (and related paths); agent reads disk |
| UI observes in-flight / success / failure | Need process or API lifecycle signals, not fire-and-forget |

Architecture already decided: Interview code → **agent provider interface** → v1 Cursor adapter.

---

## Options investigated

### A. Cursor SDK (`@cursor/sdk` / `cursor-sdk`)

- **Languages:** TypeScript and Python only (public beta). No first-party Rust SDK.
- **Runtimes:** `local` (cwd on caller machine) or `cloud` (Cursor-hosted VM). Interview protocol is on-disk local files → **local runtime**.
- **Auth:** `CURSOR_API_KEY` / `apiKey`, or `Cursor.auth.login()` (browser mint → `~/.cursor/sdk/auth.json`). User API keys and service-account keys supported; Team Admin keys not yet.
- **Billing (official docs):** SDK runs follow the **same pricing, request pools, and Privacy Mode rules** as IDE and Cloud Agents. User API keys bill to that user’s plan. Spend appears under the usage dashboard with an SDK tag. `chargedCents` can be `0` for plan-included usage.
- **Caveat:** Marketing/blog copy sometimes says “token-based consumption pricing.” Treat **docs over blog**. Still validate with one real Auto run against the usage dashboard (see risks).
- **Auto model IDs:**
  - `{ id: "auto" }` (TS) / `model="auto"` (Python) — server-selected **Auto** (legacy / non-Router path). **v1 default for individual subscription + Auto availability.**
  - `auto-smart` + `optimize_for`: `cost` | `balanced` | `intelligence` — **Cursor Router** (documented primarily for Teams/Enterprise; must appear in `Cursor.models.list()`). Prefer `cost` if Router is available and the goal is classic Auto-style bundled pricing.
- **Observation:** `agent.send` → stream + `run.wait()` → `result.status` (`finished` / `error` / …); distinguish `CursorAgentError` (never started) vs run failure. Cancel via `run.cancel()` when supported.
- **Rust embedding:** Requires a **Node (≥22.13) or Python sidecar** (or embedding a JS runtime). Extra process, packaging, and lifecycle complexity for a GPUI desktop app.

Sources: [TypeScript SDK](https://cursor.com/docs/sdk/typescript), [Python SDK](https://cursor.com/docs/sdk/python), Cursor SDK skill (`skills-cursor/sdk`).

### B. Cursor Agent CLI (subprocess)

- **Binary:** `agent` (install via cursor.com/install). On this machine a shim `cursor-agent` exists but currently points at a **missing Node runtime** — install health is an operational risk, not a product blocker.
- **Auth:** `agent login` (browser, recommended for local desktop) **or** `--api-key` / `CURSOR_API_KEY` for automation. Forum guidance: CLI uses **Cursor account / subscription** models (no BYOK provider keys).
- **Headless / scripted run:**
  ```bash
  agent -p --force --trust --workspace <repo-root> --model auto \
    --output-format json \
    "<short prompt referencing interview-config.md path>"
  ```
  - `-p` / `--print`: non-interactive.
  - `--force` / `--yolo`: auto-approve commands (needed for unattended researcher/processor).
  - `--trust`: trust workspace in headless mode.
  - Omit `--resume` / `--continue` → **fresh session** per process (matches v1).
- **Auto model:** `--model auto` (confirm exact id via `agent models` / `--list-models` for the logged-in account). Router Cost/Balance/Intelligence, if exposed in CLI catalog, should be verified the same way — prefer plain `auto` until catalog proves otherwise.
- **Observation from Rust:**
  - Spawn child → **in-flight** while process alive.
  - Exit code 0 + parseable stdout → **success** (optionally inspect JSON).
  - Non-zero exit / timeout / spawn failure → **failure** (stderr + exit code to UI).
  - Optional: `--output-format stream-json` for richer progress later.
- **Billing:** Same account as IDE when using login or user API key; not a separate “pay tokens outside subscription” product surface. Still validate Auto pool drawdown once.

Sources: [CLI overview](https://cursor.com/docs/cli/overview), [parameters](https://cursor.com/docs/cli/reference/parameters), [authentication](https://cursor.com/docs/cli/reference/authentication), [using](https://cursor.com/docs/cli/using).

### C. CLI ACP mode (`agent acp`)

- Spawns Cursor CLI as an **ACP (Agent Client Protocol)** server over stdio (JSON-RPC).
- Designed for custom clients (JetBrains, Neovim, Zed, etc.): `initialize` → `authenticate` → `session/new` → `session/prompt` → `session/update` stream; permission callbacks.
- **Pros:** Structured status, cancel, permissions, streaming — better long-term client UX than print-mode exit codes.
- **Cons:** Heavier protocol to implement in Rust for v1; still depends on CLI install + auth; docs mark ACP as advanced/hidden.
- **Fit:** Strong **v1.5 / v2** upgrade path if print-mode proves too coarse; not required to ship first adapter.

Source: [ACP](https://cursor.com/docs/cli/acp).

### D. Cloud Agents REST API (`/v1/agents`)

- Cloud VM + cloned repo; API-key oriented cloud product.
- Poor fit for Interview UI’s **local on-disk** queue/config/transcript protocol and for “same machine as tod.”
- Not recommended for v1 Cursor adapter.

Source: [Cloud Agents API](https://cursor.com/docs/cloud-agent/api/endpoints).

---

## Critical questions

### 1. Does SDK local runtime work under subscription + Auto without a separate token-billing path?

**Documented answer: yes, with caveats.**

- Official SDK docs: same pricing / request pools as IDE; user API key → user’s plan.
- Select Auto via `{ id: "auto" }` (or Router `auto-smart` + `optimize_for: "cost"` when available and desired).
- Plan gates: e.g. Cursor **Start** plan explicitly **excludes Auto and the Cursor SDK** — Pro (or whatever plan grants Auto) is required.
- **Not fully proven in this spike without a live metered run** against the user’s Auto-special account. First implementation milestone should fire one Auto local run and confirm usage dashboard attribution (subscription pool / Auto Cost rates, not unexpected on-demand-only meter).

### 2. Does CLI work under the same constraints?

**Documented answer: yes — preferred path for this app.**

- Auth via `agent login` or user API key; models through Cursor account.
- `--model auto` selects Auto.
- Same validation step as SDK: one real Auto print-mode run, check usage.

### 3. What model id selects Auto?

| Surface | Selection |
| --- | --- |
| SDK (individual / classic Auto) | `{ id: "auto" }` / `model="auto"` |
| SDK (Cursor Router, Teams+) | `auto-smart` + `optimize_for: "cost"` (bundled Auto-like) / `balanced` / `intelligence` |
| CLI | `--model auto` (confirm with `agent models` / `--list-models`) |

Do **not** default to `composer-2.5` in Interview UI — that burns non-Auto allowance.

### 4. Can Rust spawn and observe runs cleanly?

| Path | Spawn | In-flight | Success / failure |
| --- | --- | --- | --- |
| **CLI print mode** | `std::process::Command` / `tokio::process` | Child alive | Exit status + stdout/stderr |
| **CLI ACP** | Same + JSON-RPC over pipes | Session updates | `stopReason` / errors / cancel |
| **SDK** | Spawn Node/Python sidecar; IPC | Sidecar reports run status | Map SDK `result.status` / errors |

All three are workable. CLI print mode is the **least moving parts** for Rust GPUI v1.

---

## Pros / cons

| Criterion | CLI `-p` | SDK local (+ sidecar) | CLI ACP | Cloud REST |
| --- | --- | --- | --- | --- |
| Subscription + Auto (docs) | Strong | Strong (same pools) | Same as CLI | Different product; skip |
| Rust-native fit | Excellent | Poor (TS/Python only) | Good (stdio) | OK HTTP, wrong runtime |
| Fresh session / run | New process, no resume | New `Agent.create` + one `send`/`prompt` | `session/new` each time | N/A for local protocol |
| Status UX (in-flight/ok/fail) | Adequate (process + exit) | Excellent (typed run API) | Excellent | N/A |
| Packaging / deps | `agent` on PATH | Node 22+ or Python + package | `agent` on PATH | API key only |
| Complexity for v1 | Low | High | Medium–high | Wrong fit |
| Future richness | Upgrade to ACP | Already rich | Already rich | — |

---

## Recommendation (v1)

**Primary: Cursor Agent CLI in print mode, wrapped by a Rust `CursorCliAgentProvider`.**

1. Resolve `agent` (or documented install path); fail clearly in UI if missing/broken.
2. Prefer existing `agent login` credentials on the machine; optionally allow `CURSOR_API_KEY` for power users.
3. Each researcher replenishment / answer-processor submit: **new subprocess**, `--model auto`, `--workspace` = interview/repo root, prompt includes absolute path to `interview-config.md` (+ role-specific short instruction).
4. Map child lifecycle → provider status events: `InFlight` → `Succeeded` | `Failed { message }`.
5. Use `--force --trust` for unattended tool use; tighten later if auto-review/sandbox policy is needed.
6. Keep the **provider trait** abstract so a later Claude adapter (and optional Cursor ACP or SDK sidecar) can swap in without UI rewrites.

**Do not** take SDK-as-primary for v1 solely for richer APIs — the Node/Python sidecar tax outweighs benefits for a Rust desktop host that only needs start + terminal status. Revisit SDK or ACP if:

- Print mode cannot surface failures reliably, or
- We need streaming token/progress UI, cancel mid-run with conversation fidelity, or
- CLI Auto selection diverges from IDE Auto for this account.

### Minimal provider sketch (illustrative)

```rust
#[async_trait]
trait AgentProvider: Send + Sync {
    async fn start(&self, req: AgentRunRequest) -> Result<AgentRunId, AgentProviderError>;
    fn status(&self, id: &AgentRunId) -> AgentRunStatus; // InFlight | Succeeded | Failed(String)
    async fn cancel(&self, id: &AgentRunId) -> Result<(), AgentProviderError>;
}

struct AgentRunRequest {
    cwd: PathBuf,
    prompt: String,           // includes config path + short instruction
    model: AgentModelHint,    // CursorAuto for v1
    kind: AgentKind,          // Researcher | AnswerProcessor
}
```

Cursor adapter constructs the CLI argv; Claude adapter later implements the same trait differently.

---

## Decision tree (if still uncertain after first dogfood)

```
Need programmatic Cursor agent from tod?
├─ Local interview files on disk? ─ no ─► reconsider Cloud Agents (out of v1 scope)
└─ yes
   ├─ Is `agent` installed + authenticated + `--model auto` in catalog?
   │  ├─ no ─► fix CLI install / login; if Auto missing on plan, stop (plan gate)
   │  └─ yes ─► run one Auto print job; check usage dashboard
   │            ├─ billed like IDE Auto / plan-included ─► ship CLI provider
   │            └─ unexpected separate meter / Auto rejected
   │               ├─ try SDK local `{ id: "auto" }` once (sidecar prototype)
   │               └─ if both fail ─► escalate (account/plan), do not invent BYOK path
   └─ CLI status too weak for UX? ─► upgrade same binary to ACP; keep provider trait
```

---

## Open risks

1. **Live billing proof missing** — Docs say subscription pools; blog language differs. Confirm with one Auto CLI run on the operator account.
2. **Auto id / Router drift** — Catalog may expose `auto`, `auto-smart`, or renamed modes; always discover via `agent models` / SDK `Cursor.models.list()` at integration time.
3. **CLI install fragility** — Broken `cursor-agent` shim (missing bundled Node) observed on spike machine; tod must detect and guide repair/update.
4. **Unattended permissions** — `--force` is blunt; researcher/processor may need sandbox or allowlists later.
5. **Concurrent researchers (req max 2)** — Two CLI children is fine; ensure cwd/locking so both don’t corrupt the same queue writes (protocol/agent skill concern, not launch concern).
6. **Start / gated plans** — Plans without Auto/SDK cannot use this adapter; surface a clear UI error.
7. **SDK as future option** — Still valid if CLI Auto billing or reliability disappoints; design provider boundary so sidecar can be added without UI redesign.

---

## References

- Cursor SDK skill: `C:\Users\joel\.cursor\skills-cursor\sdk\SKILL.md`
- https://cursor.com/docs/sdk/typescript  
- https://cursor.com/docs/sdk/python  
- https://cursor.com/docs/cli/overview  
- https://cursor.com/docs/cli/reference/parameters  
- https://cursor.com/docs/cli/reference/authentication  
- https://cursor.com/docs/cli/using  
- https://cursor.com/docs/cli/acp  
- https://cursor.com/docs/models-and-pricing  
- https://cursor.com/help/models-and-usage/cursor-router  
