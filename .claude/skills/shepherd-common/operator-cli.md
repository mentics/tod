# The operator tier of tod-cli

A shepherd drives the work the task panel's runner menu and requests do,
without the app's window, through a hidden tier of `tod-cli`. It is not in what
an agent working a node is told, and it does not exist unless you turn it on.

## Turning it on, and who you are

| Variable | Meaning |
|---|---|
| `TOD_OPERATOR=shepherd` | Shows the tier in `tod-cli --help`, `tod-cli help <words>` and `tod-cli <noun> --help`, and lets it run. Without it `runner` is an unknown noun and `decisions answer` an unknown command, like any other. |
| `TOD_ACTOR=shepherd:<id>` | Who is acting; `<id>` is yours to choose (a short name for this shepherd). Unset, the act is the operator's, as `tod-cli lifecycle set` is. Anything else is an error. |
| `TOD_INTERVIEW_ACTOR` | Set only inside an agent session (a node's own conversation). With it set every verb below is refused, even with `TOD_OPERATOR=shepherd`: a node's agent must not drive its own node. Do not set it. |

Always pass `--data-root <PATH>`, as for any `tod-cli`. Use the live data root only
when the task says to, and read it first (`runner menu`) before changing anything.

Who acted is left on the node's activity as a note by `operator` or `shepherd <id>`
(the app records a journey action for each click; `tod-cli` has no journey sink, so
the note is the record). `decisions answer` also records the actor on the answer
itself: `shepherd:<id>` instead of `user`.

## The runner menu

`<NODE>` is a node slug or full UUID.

```
tod-cli runner menu <NODE>
tod-cli runner start <NODE>
tod-cli runner resume <NODE>
tod-cli runner resume-fresh-budget <NODE>
tod-cli runner run-fresh <NODE>
tod-cli runner pause <NODE>
tod-cli runner stop-now <NODE>
tod-cli runner check-now <NODE>
tod-cli runner rerun-phase <NODE>
tod-cli runner migrate <NODE> --to <LOCATION> [--commit | --discard]
tod-cli runner reset-environment <NODE>
tod-cli runner update-workspace <NODE>
tod-cli runner delete-sandbox <NODE> [--stop]
tod-cli runner delete-sandbox-anyway <NODE> [--stop]
tod-cli runner sync-now <NODE>
tod-cli runner adopt-sandbox <NODE>
```

`--json` gives JSON instead of text.

**`menu`** prints what the split button would offer now: the node's lifecycle state,
where it runs, how its run stands (not started, running, paused, stopped, failed,
stopped for a request, budget spent, waiting on something else, done), the default
action, and the menu entries, each as the verb that does it. It is decided by the same
function and facts as the panel (`tod_core::runner_menu`). Read it first: the verbs
refuse what it does not offer.

**Every other verb is one menu entry**, and does what the panel's click does where
the node's runner runs:

| Verb | Menu entry | What it does |
|---|---|---|
| `start` | Start | Starts the autopilot (a cloud node: makes its sandbox if it has none and starts the supervisor). |
| `resume` | Resume | Continues the same agent session. |
| `resume-fresh-budget` | Resume with a fresh budget | Continues with the turn and time budget reset. |
| `run-fresh` | Run with a new session | The current step in a new agent session. |
| `pause` | Pause | Stops once the current turn ends. |
| `stop-now` | Stop now | Cancels the turn in flight and stops. |
| `check-now` | Check now | Checks the phase's gate with no agent; moves the node on when it is clear; starts the phase agent when only its work is left. A failing gate or an unreadable GitHub is the result, printed as the check's finding (`failed` in JSON), not an error. |
| `rerun-phase` | Rerun this phase | A new stay in the current state, then the run starts in a new session. |
| `migrate` | Migrate to... | Moves the node's files to the location `--to` names (a name or an id; `menu` lists them). Its sessions are kept, its current files are removed with each branch pushed first, nothing is started. |
| `reset-environment` | Reset environment | Sets the node's environment up again, deleting nothing. |
| `update-workspace` | Update workspace to latest main | Brings the node's checkout up to the latest main, or says why it left it alone. |
| `delete-sandbox` | Delete sandbox | Deletes the node's cloud sandbox after a checkpoint of its uncommitted work. |
| `delete-sandbox-anyway` | Delete sandbox without a checkpoint | The same without the checkpoint: uncommitted work in the sandbox is lost. |
| `sync-now` | Sync now | Syncs with the orchestrator and reports what it shows of the node's supervisor. |
| `adopt-sandbox` | Adopt existing sandbox | Records the sandbox an older build started the node in as its location, after checking it is the node's and answers. |

Where the node runs decides how: a node whose files are on this machine is run by
the resident daemon `tod-agentd` (`start`, `resume`, `run-fresh`, `pause`,
`stop-now` and `rerun-phase` go to it); a node in a cloud sandbox is run by its
supervisor there (the same verbs become requests to it).

### Refusals and what answers them

- **A verb the menu does not offer now** is refused, naming what it does offer.
- **The daemon is not running** for a node that runs on this machine (including
  `menu`, which needs its run state): the command fails saying so. Start the app
  (it starts the daemon) or `tod-agentd` for that data root and run it again. It is
  never started for you.
- **`migrate`** refuses while the run is active (`pause` or `stop-now`, wait for it to
  end), without `--to`, for a node with children, and when the files being left have
  uncommitted changes, unless `--commit` commits them on the branch first or
  `--discard` throws them away (the dialog's Commit and Discard; give at most one).
  Files that something is using are refused too.
- **`delete-sandbox` and `delete-sandbox-anyway`** refuse while the run is active
  unless `--stop` stops it first (the panel's "Stop and delete").
- **Not in an agent session**, as above.

A cloud job's progress goes to stderr as it happens; its result is the one line on
stdout. A failure names what failed and what to do next, as the panel's does.

What the panel does that a command does not: a conversation a user started by hand
in the app is not seen (the menu never shows the "hand turn" state), and the panel's
own short-lived "busy" state while a job runs does not exist here.

## Answering requests

```
tod-cli decisions answer <ID> --option <N>
tod-cli decisions answer <ID> --text <TEXT>
```

Answers request `<ID>` (the full id or the 8-character prefix `decisions list`
shows) exactly as the task panel does: `--option` picks option N (1-based), `--text`
gives free text, and at least one is required. What the panel does, this does:

- An option that was asked with `--moves-to` moves the node back (as the user's own
  move, reopening implementation steps when the state is `active` or earlier) before
  the answer is recorded, and fails, recording nothing, when it cannot move.
- A permission request's answer is a standing grant (or a deny): this exact action,
  or file actions under its folder. The runner reopens the step by itself.
- A silent review bot's question: the options leave the bot out of the node's
  settings or keep waiting, and an answer releases the run parked on it.
- A node that runs in a cloud sandbox is woken after the answer, replacing a sandbox
  that expired. A failure to wake it is printed to stderr; the answer stays recorded.
- The plan steps the question held up open again.

The in-app conversation that asked is not sent a message (the app delivers that); the
answer reaches the agent's next turn, in the node's answered questions.

Answering is the user's decision. A shepherd answers only what it was told it may,
and says so in its report.

## Checking your work

After any verb, read `tod-cli runner menu <NODE>` again: the run state and the
entries show what the verb did. The node's activity (the task panel's Watch) shows
the note naming who did it.
