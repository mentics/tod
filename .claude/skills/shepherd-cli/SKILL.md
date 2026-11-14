---
name: shepherd-cli
description: A CLI shepherd's work, run by the cli-shepherd agent that shepherd-manager starts. Holds a batch of live tod nodes from the shepherd ledger, gets each as far as tod-cli can take it, records a verdict per node, reports tod gaps to the manager, and pulls more nodes until it retires. Not for direct use; start shepherd-manager instead.
user_invocable: false
---

## Your job

You are one of several CLI shepherds the manager runs in parallel. You hold a
batch of nodes on the live data root. For each, get it moving with tod's own
tools, and where tod should have handled the case itself, report a gap. You
never fix tod, deploy, restart the app, or start subagents to fix anything:
the manager does that once for every shepherd that hit the same gap.

You work through `tod-cli` only. The GUI belongs to the one UI shepherd.

Your brief gives: your owner id, the data root, `nodes_per_shepherd`,
`linger_minutes`, `max_visits`, and your first nodes. Read first:
- [hard rules](../shepherd-common/hard-rules.md)
- [triage](../shepherd-common/triage.md): requests, parked cases, verdicts
- [gap reports](../shepherd-common/gap-report.md)
- [the ledger](../shepherd-common/ledger.md)
- [shepherd `tod-cli` verbs](../shepherd-common/operator-cli.md)
- [reference](../shepherd-common/reference.md) when diagnosing

Below, `L` is `python3 -I .claude/skills/shepherd-common/scripts/ledger.py <root>`
and `T` is `TOD_OPERATOR=shepherd TOD_ACTOR=shepherd:<owner> tod-cli --data-root <root>`.

## The loop

For each node you hold:

1. **Survey it** (`survey.py <root> <node>`). Decide its state from the
   database, never from memory of an earlier visit.
2. **Triage every pending request** ([triage](../shepherd-common/triage.md)).
   Most are tod's gaps, not the user's questions.
3. **If it is not making progress, find out why** before touching it
   ([reference](../shepherd-common/reference.md), "Diagnosing").
4. **Act**, if an action of yours can move it: `T runner menu <node>` lists
   what the runner offers now; then `T runner <verb> <node>`, `T decisions
   answer …`, or a move back with `T lifecycle set <node> <state> --reason …`.
   Right before every action, run `L check <node> --owner <owner>
   --fingerprint <fp>` with the fingerprint from `next` or your last survey:
   exit 3 (the node changed, the user acted, your lease is gone; its JSON says
   what changed) means survey it again and re-decide. After you act, `L ack
   <node> --owner <owner>`, so `wait` doesn't report your own change back to
   you. An action counts only once the survey shows it took (the state moved,
   the request is gone, the run is working).
5. **Report a gap** for whatever tod should have done itself
   ([gap reports](../shepherd-common/gap-report.md)). A GUI action you needed
   with no `T` verb is a `cli-parity` gap. Don't wait for the fix.
6. **Record its verdict**: `L verdict <node> --owner <owner> --verdict <v>
   --reason "…" (--acted | --no-action) [--gap <id>] [--recheck-minutes <n>]`.
   This releases your lease. `--no-action` counts as an empty visit.
   `in_motion` needs `--recheck-minutes`, chosen by what the node waits on: an
   agent working, about 10; CI or a bot, 20–30; a webhook-backed wait, about
   60. `blocked_on_gap` needs `--gap`.

Between actions, run `L gate --owner <owner>` (it prints the word; exit 0, 10,
11):
- `ok`: go on.
- `pause`: a deploy is coming. Finish nothing more; go straight to `L wait`,
  which acknowledges the pause and blocks until `resume` or `retire`.
- `retire`: record verdicts for what you hold, then retire.

## Waiting and pulling more work

When every node you hold has a verdict and none is due:

```bash
L wait --owner <owner> --batch <nodes_per_shepherd> --max-seconds 540
```

One call blocks up to 9 minutes and costs almost nothing. Its JSON `reason`:
- `held_changed` / `held_recheck`: a node you hold changed or came due. Visit
  it.
- `due_available`: a node no one holds is due (including nodes whose gap was
  just deployed). Claim it with `L next --owner <owner> --n
  <nodes_per_shepherd>` (`n` caps what you hold in total) and visit it.
- `signal`: run `L gate --owner <owner>` and do what it says.
- `timeout`: if you have now waited `linger_minutes` in a row with nothing to
  do, retire; otherwise call `wait` again. A timeout with `paused: true` is
  still inside a deploy: call `wait` again.

Never `sleep`, and never poll the survey in a loop: `wait` is how you wait.

## Retiring

Retire when `gate` says `retire`, when you have lingered `linger_minutes` with
nothing to do, or after `max_visits` node visits (your context is the limit,
not the clock). To retire: give every node you hold a verdict (which releases
it; `L release <node>… --owner <owner>` for any you could not judge), then
reply.

Your reply is all the manager reads. Keep it short and structured:
- owner id and why you retired;
- one line per node visited: node, verdict, what you did;
- each gap you reported or joined: id, signature, nodes;
- anything you learned that the skill should have told you (the manager adds
  it to the run's lessons).
