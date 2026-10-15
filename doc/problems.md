# Problems: every executable's errors reach the app

Every executable tod builds reports its failures to the app's **log panel**
(status bar, "Log"), and an ERROR also raises a **toast** naming its source
(repeats of the same source and message within 60s collapse to one toast with a
count). A WARN goes to the log only, highlighted.

## Local executables

`crates/tod-problems` (a leaf crate, `serde_json` only) appends one JSON line
`{time, level, process, message[, label]}` to `<data root>/problems/<process>.jsonl`
(rotated at 256 KB to `.jsonl.1`). The app tails the directory every 3s on a
background thread (`tod_core::logging::buffer::start_problem_tail`), starting at
the end, and records each line with the `LogSource` of its process.

| Executable | What it reports | Data root |
|---|---|---|
| `tod` | tracing WARN/ERROR in process (`RingLayer`) | in process |
| `tod-agentd` | its `agentd.log` WARN/ERROR (tailed), and a fatal exit | the data root |
| `tod-cli` | error exit; ignored-option warnings | `--data-root`/resolved root |
| `tod-sandbox-cli` | error exit | `--data-root`/resolved root |
| `tod-zed-shim` | failures | `TOD_DATA_ROOT`, which the app sets when it launches Zed |
| `tod-journeys` | not wired: a standalone receiver with no tod data root | — |

Where there is no data root (a shim run by hand), the report is skipped and the
failure still goes to stderr, which is unchanged.

## Remote executables (cloud sandboxes)

`tod-relay`, `tod-supervisor` and `tod-orchestrator` run in sandboxes the app
must never wake to ask how they are. So the route only uses contact that
already happens:

1. A remote process appends to the node's problem directory
   (`TOD_PROBLEMS_DIR`, else `/tmp/tod-problems`). The supervisor also logs its
   own WARN/ERROR there (`tod_supervisor::problems::ProblemLayer`).
2. The supervisor forwards that directory (`Outbox`, persisted, sent once) with
   a replica **pull** or a **push that has changes**: requests it makes anyway.
   Failure leaves the problems queued and logs a warning. It is never a reason
   to contact the orchestrator.
3. The orchestrator stores them per user (`POST /users/<u>/nodes/<n>/problems`,
   labelled `node <n>`), also records its own 5xx responses, and treats growth of
   that store as a "changed" notice (data-free, already awake).
4. The app reads them (`POST /users/<u>/problems/pull`, cursor in
   `cloud-sync.json`, first read starts from now) **inside a notify-driven or
   manual sync only**, and records each as `[<node>] message`.

The app never polls a sandbox and never calls one for logs.

## Not covered / open

- A sandbox whose supervisor never talks to the orchestrator again keeps its
  relay problems queued locally until it does. Surfacing them sooner would need
  the app to wake the sandbox, so it does not.
- The orchestrator's own startup failure (no user yet) is not reported.
- Problems from before a tail starts are not shown (tails start at the end).
