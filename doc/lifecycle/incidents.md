# Incidents: one failure many nodes share

Spec: `doc/requests/design.md` 4.1, 4.3, 4.4 (build step 6). Code:
`tod_store::incident_class`, `tod_store::incidents`, `tod_core::incident`,
`tod_core::conversation::incident`, the daemon's `incident_pass`
(`tod-agentd/src/runners.rs`), the status bar's Incidents button
(`tod_ui::views::incidents_dialog`).

Fifty nodes hitting an expired GitHub token must be one problem, not fifty
retry ladders and fifty requests. An **incident** is that one problem. The
tables are this machine's and its daemon's (`tod_core::incident::set_enabled`,
switched on by `tod-agentd` at startup). A cloud supervisor never parks by
itself: it reports what it sees and the daemon parks and holds it (see
**Cloud nodes**).

## Classification

`incident_class::classify(source, error)` gives a `Kind` and a signature
`<kind>:<key>`:

| Kind | Examples | Parks |
|---|---|---|
| `auth` | `auth:github`, `auth:aws`, `auth:blaxel`, `auth:agent` | yes |
| `service_down` | `service_down:github`, `service_down:<host>` | yes |
| `provider` | `provider:overloaded`, `provider:rate_limit` (agent turns) | yes |
| `tool_broken` | `tool_broken:<hash>` (`tod-cli`, shims) | yes |
| `ci_infra` | `ci_infra:<kind>` (`github::infra_kind`) | no: rerun per job |
| `work` | everything else, including a spent GitHub rate limit | no |

Unknown is `work`: the change's own failure, which climbs the ladder as
before. The hash comes from `failure_signature::of_text`, so ids, times and
paths do not make identical failures differ.

## Matching and parking

Seen from `autopilot::github_look_failed` (every failed look at GitHub, and
babysit's), an agent turn that ends in an error (`Autopilot::agent_failed`),
and an agent-filed `decisions ask --reason broken|access` whose `--error`
matches (`InterviewCommand::AskDecision`, before the request is created).

- An **open incident** with the signature: the node is parked. No retry, no
  session, no request. The run ends `Waiting` (fallback due time 6 h; the
  daemon resumes it when the incident clears). `tod-cli decisions ask` is
  refused with "parked on incident X" (a committed result, not a store error,
  so the park survives the transaction).
- Otherwise the node retries with backoff as before (a first sighting of a
  parking kind retries even where the error was not transient), and the
  sighting is kept (`incident_sightings`). A **second node with the same
  signature within 30 minutes** opens an incident covering both. If the first
  node had already filed a pending request for the cause, it becomes the
  incident's one request.

A parked node is a row in `incident_nodes` with no `resumed_at`.

## Who is next

`next_actor`: a parked node is `Others` (`WaitKind::Other`), `who` = "GitHub
auth failing, 12 nodes", the detail names the operation and the failed-probe
count, `next_check_ms` is the next probe. It is placed after `run_end` and
before the node's own waits, whose failures it explains.

## Probe and release

The daemon tick (`incident::tick` then `take_released`) runs `Op::Pass`:

- The first probe is due 2 minutes after opening, then backs off to 30
  minutes. The probe is **one parked node released to run**: its own
  operation is the test.
- That node's success (`incident::node_ok` after a real look at GitHub)
  resolves the incident, and the other nodes are released **staggered**
  (5 s apart), each started by the daemon (`start_auto`). A node that cannot
  be started is parked again (`Op::Repark`) and counted as a failed probe.
- A probe node that fails again re-parks and doubles the backoff. A probe with
  no verdict after a 3-minute grace counts as success where the operation has
  no hook (provider, tool).
- Restart-safe: `resumed_at` and `consumed_at` mark released but not started.

## Escalation, once per incident

- `auth` (access): ONE user request (decision, reason `access`, rendered by
  `request_brief` with need and, where there is exactly one, the command, e.g.
  `aws sso login`; the parked nodes are listed) on one representative node.
  Answering it makes the next probe due now.
- Anything else: after 15 minutes open (at once for `tool_broken`), ONE
  investigation session, protocol `incident` (Haiku, low effort, lifecycle
  config key `incident`), given the samples. It fixes what it can or files one
  request. It runs on the autopilot of the node the claim went to
  (`Autopilot::incident_step`); the claimed node is also the probe.
- A claim is made inside `Op::Pass` so two ticks never both escalate; a failed
  escalation releases it (`ClearEscalation`) for the next tick.

## Notices (4.4)

A review bot silent on several nodes' pull requests is one **notice**
incident (`bot_silent:<bot>`), shown in the status bar. It parks nothing and
resolves with the last node's bot review or when dismissed. The per-node
bot-silence request (design 4.4's auto-proceed, step 1) is unchanged.

## Status bar

Open incidents show as **Incidents (N)** in the status bar; it opens a list of
each incident, the nodes parked on it, a sample error, and Dismiss
(`Op::Dismiss`, which resolves and releases the nodes).

## Storage

Machine-local (schema v111, not synced): `incidents`, `incident_nodes`,
`incident_sightings`. `incident_nodes` has a `journey_changes` trigger.
`incidents` and `incident_sightings` do not: they are this machine's
bookkeeping, and a node's membership is what shows in a journey.

## Cloud nodes

`tod_core::incident::cloud`. One channel, the problem forwarding of
`doc/problems.md`; no parallel path.

- **Report.** The supervisor switches on `incident::set_cloud_reporting`. A
  failure of a kind an incident can hold (not `work`) that it would have
  observed goes to the node's problem directory as a problem line carrying an
  `incident` object (event, node, source, kind, signature, operation, error).
  A look at GitHub still retries first; an agent turn goes to the supervisor's
  own failure guard. When the operation goes through again after a report, one
  `ok` line says so (a marker file in the problem directory keeps it to one).
- **Ingest.** The daemon's cloud sync reads the lines (`cloud_sync::sync_once`,
  `cloud::ingest`) and feeds each to the same `observe` a local failure goes
  through, so cloud and local nodes with one signature are one incident. The
  daemon classifies from the error itself and warns when the sender's
  signature differs (build skew). A report that cannot be used (a node that is
  not a cloud node here, an unknown source) is an error in the log panel, since
  the cursor has moved on.
- **Hold.** A parked cloud node gets an ordinary synced `event` wait
  `incident:<id>` (`cloud::hold`, `hold_parked`: at every ingest and every
  daemon tick, so a node parked by someone else's report is held too). The
  supervisor already stops at its next boundary and sleeps while any wait is
  pending: no retry, no session, no request, nothing spent in the sandbox. The
  wait's deadline (`PARKED_FALLBACK_MS`, 6 h) only keeps a lost release from
  holding it forever; the user's "run" cancels it as any wait.
- **Release and probe.** For a cloud node the probe is the release: the daemon
  ends the hold and wakes the supervisor as Check now does
  (`cloud::release`, `wake_node_requesting`). It resolves when the supervisor
  reports `ok`, or when it reports no new failure within
  `CLOUD_PROBE_GRACE_MS` (20 min, longer than the local grace since a sandbox
  has to wake). A wake that fails parks the node again and holds it again.
- **Who is next.** Parked is `Others`, naming the incident, as for a local node.
- **Escalation.** An investigation needs a local autopilot; an incident whose
  escalation node is a cloud node becomes the user's one request instead, and
  the representative prefers a local parked node.

Needs the orchestrator deployed with the app: an older orchestrator re-encodes
problem lines without the `incident` field, so the reports are lost. Not
forwarded: a `decisions ask --reason broken|access` an agent files from a cloud
node.

## Deferred

- A daemon-level conversation runner: the investigation borrows a node's
  autopilot.
