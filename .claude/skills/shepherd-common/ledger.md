# The shepherd ledger

Shared memory and coordination for parallel shepherds. A shepherd never keeps
its own list of nodes: it asks the ledger what to look at, and tells the ledger
what it concluded.

```
python3 -I .claude/skills/shepherd-common/scripts/ledger.py <data-root> <command> …
```

- State: `<data-root>/shepherd/ledger.db` (SQLite, WAL, created on first use;
  `--ledger PATH` overrides it). Every write is one short `BEGIN IMMEDIATE`
  transaction, so concurrent shepherds never double-claim.
- The ledger holds **only what is current**: nodes in scope and not done,
  open gaps (`open`, `fixing`, `verified`), running shepherds, the latest
  signal. Everything else goes to `history.jsonl` next to it, one JSON record
  per line, and leaves the ledger: every verdict as it is recorded, a gap
  when it is `deployed` or `wontfix`, a node when it reaches `done` or leaves
  the scope, a shepherd 10 minutes after it retired or 30 minutes after it went
  silent (with its visit counts), and every signal. Read it with `history`.
- `tod.db` is only ever read, read-only. The ledger never writes it.
- Output is JSON; `--text` (anywhere on the line) prints it readably. Errors go
  to stderr with a non-zero exit; nothing is swallowed.
- `<node>` is a ticket (`ENG-2925`), slug or UUID that names exactly one node.
- `<owner>` is your shepherd id (`cli-1`, `ui`); pick it once and keep it.

Exit codes: `0` ok, `1` error, `2` usage, `3` `check` refused, `10` `gate` says
pause, `11` `gate` says retire.

## Fingerprint and version

A node's **fingerprint** hashes what changes when something meaningful
happens: lifecycle state; run state (`phase`, `reason_kind`, `reason_text`);
ids of pending requests; pending waits (`match_spec|state`); `linked_prs`; the
PR heads tod's pr_draft review last reviewed (tod.db has no live PR head);
the sandbox name. Heartbeats, `updated_at`, `since`, wait due times and lease
times are left out so a quiet node's fingerprint stays put. `survey.py` prints
it in brackets and saves it with `--save`.

The **shepherd version** hashes the files under `.claude/skills/shepherd-cli/`,
`shepherd-ui/`, `shepherd-common/` (not `__pycache__`) and
`.claude/agents/shepherd/*.md`. A verdict recorded under another version no
longer counts: the node is due again.

A node is **due** when it has no verdict, its fingerprint differs from the one
its verdict was recorded with, its `recheck_at` passed, its verdict's version is
stale, or the gap it waits on was deployed. A node that reached `done` is
`finished`: it moves to the history and is never due.

## Shepherd commands

| Command | Use |
|---|---|
| `next --owner ID --n K [--kind cli\|ui] [--lease-minutes M]` | Sync, then lease up to K due, unheld, unleased nodes (K is the cap on what you hold, so a restart gets your own held nodes back, flagged `"new": false`). Order: stopped run, pending request, wait past due, routine recheck, never visited; grouped by lifecycle state within a rank. Each node carries its `fingerprint` and `due_reason`. |
| `check NODE --owner ID --fingerprint FP` | Before **every** mutating action. Exit 0 if the fingerprint is still `FP` and you hold the lease; else exit 3 with JSON `{reason, fingerprint, changed[]}`: stop, re-read the node, act on the new state. |
| `ack NODE… --owner ID` | After you acted on a node you still hold: rebaseline it so `wait` does not report your own change. |
| `verdict NODE --owner ID --verdict V --reason TEXT [--recheck-minutes N] [--gap ID] [--acted\|--no-action]` | Record the conclusion with the node's current fingerprint and the current version, release the lease, count the visit (`--no-action` also counts an empty visit). `V` is `ceiling` (nothing more tod or I can do), `parked` (waiting on a person or outside event), `in_motion` (tod is working; needs `--recheck-minutes`), `blocked_on_gap` (needs `--gap ID`). |
| `renew --owner ID [--lease-minutes M]` | Heartbeat; extends all your leases (default 20 minutes). |
| `release NODE… --owner ID` | Give nodes back without a verdict (they stay due). |
| `gate --owner ID` | Call at the top of every loop. Renews, prints `ok` (0), `pause` (10) or `retire` (11). On `pause`, go to `wait`; on `retire`, release and stop. |
| `wait --owner ID [--max-seconds N] [--batch K]` | Block (default 540 s, under a 10-minute tool timeout) until: a node you hold changed fingerprint (`held_changed`) or reached `recheck_at` (`held_recheck`); an unleased due node exists and you hold fewer than K (`due_available`; it is not leased, call `next`); a signal for you changed (`signal`); or `timeout`. While a `pause` is set it keeps blocking until `resume`/`retire`. Renews your leases while it waits. |
| `gap report --owner ID [--node N] --signature S --summary T` | Report a tod gap. An open gap with the same signature is reused: you get its id, and the node and owner are added. A new one whose signature was closed before carries `recurrence` (how many times) and `closed_before` (ids, commits): the fix did not hold. Pick a short, stable signature. `--node` may be left out for a gap no node shows (a `measure`). |
| `gap list` / `gap show ID` | Open gaps only (closed ones are history); `gap show` of a closed id reads it from the history. |

Nodes under `hold` are the user's: never leased, never touched.

## Manager commands

| Command | Use |
|---|---|
| `scope add ITEM…` / `scope remove ITEM…` / `scope list` | Items are tickets, slugs, UUIDs. A node with children means its whole subtree of Lifecycle nodes, re-resolved on every sync, so new children join. An item that matches nothing is an error. |
| `sync` | Resolve scope, fingerprint, finish `done` nodes, recompute due. Prints `in_scope`, `due`, `leased`, `by_verdict`, `held`, `finished` and `warnings`. |
| `signal pause\|resume\|retire (--owner ID \| --all)` | Tell shepherds. A shepherd acknowledges `pause` by being in `wait` or calling `gate`. `pause --all` also reaches shepherds started later (a deploy is under way); `retire --all` reaches only the ones running when it was sent, so fresh shepherds can start right after it. |
| `signal status` | Per owner: signal, `ack`, staleness; `all_paused` (owners that retired or went silent for 30 minutes without leases are ignored) and `awaiting_pause`. Wait for `all_paused` before a deploy. |
| `gap set ID --status open\|fixing\|verified\|deployed\|wontfix [--commit SHA] [--note T]` | `deployed` makes every node `blocked_on_gap` on it due, with the reason "fix for gap ID deployed". `deployed` and `wontfix` move the gap to the history. |
| `hold NODE… [--reason T]` / `unhold NODE…` | The user's own controls. |
| `clear (--all \| --node N \| --verdict V \| --reason-like S)` | Void verdicts; the nodes become due. |
| `status` | Counts by verdict, due, leased, held, open gaps, owners, version. Does not sync. |
| `owners` | Each running owner: heartbeat age, held nodes, visits, empty visits, signal, ack. |
| `history [--event E] [--node N] [--owner ID] [--signature S] [--since-hours H] [--limit N]` | Past records, oldest first (the last 50 by default, `--limit 0` for all). `E` is `verdict`, `gap_closed`, `node_done`, `node_left_scope`, `owner_gone` or `signal`. For reports and diagnosis, never for deciding what to do next: that is the ledger's. |

## The shepherd loop

```
gate → (pause: wait) → next → for each node: look, check before each action, verdict
     → wait (until something changes) → gate → …
```
