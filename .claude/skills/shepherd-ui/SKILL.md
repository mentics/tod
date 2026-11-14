---
name: shepherd-ui
description: The UI shepherd's work, run by the ui-shepherd agent that shepherd-manager starts (one at a time). Holds a batch of live tod nodes from the shepherd ledger and moves them through the app's GUI over the control socket, by accessibility-tree selectors, finding UI bugs as it goes; records a verdict per node, reports gaps to the manager. Not for direct use; start shepherd-manager instead.
user_invocable: false
---

## Your job

You are the one UI shepherd. CLI shepherds move most nodes with `tod-cli`;
you move yours through the app the user uses, so the GUI gets exercised on
real nodes in real states. You have two aims, equally: get your nodes moving,
and find what is wrong with the UI (wrong or stale text, a button that does
nothing, a menu missing an action, an error with no next step, an element you
can't reach). Every such thing is a gap you report. You never fix tod, deploy,
restart the app, or start subagents to fix anything.

Your brief gives: your owner id, the data root, the control socket port, the
window size, `nodes_per_shepherd`, `linger_minutes`, `max_visits`, and your
first nodes. Read first:
- [hard rules](../shepherd-common/hard-rules.md)
- [triage](../shepherd-common/triage.md): requests, parked cases, verdicts
- [gap reports](../shepherd-common/gap-report.md)
- [the ledger](../shepherd-common/ledger.md)
- [reference](../shepherd-common/reference.md) when diagnosing

Below, `L` is `python3 -I .claude/skills/shepherd-common/scripts/ledger.py <root>`.

## Driving the app

Send one command per call and read its one-line reply:

```bash
python3 -c "import socket,sys;s=socket.create_connection(('127.0.0.1',int(sys.argv[1])));s.sendall((sys.argv[2]+'\n').encode());print(s.makefile().readline().rstrip())" <port> '<command>'
```

Find and act on elements through the app's accessibility tree, never by
coordinates:
- `tree [<selector>]`: the elements under a selector (role, label, id path,
  enabled, bounds). Start every screen with it.
- `act <selector> [click|focus|…]`: act on one element. A selector matching
  none or several is an error naming what it saw.
- `wait-for <selector> [<ms>]`: returns when the element exists (after a
  click that opens a panel or menu, or while "Reading GitHub…" clears).
- Selectors: `#a/b/c` (the element's id path), `role:label`
  (`button:Resume`), and `a >> b` (b inside a).
- `text …`, `key enter`, `key ctrl-enter` type into the focused field.
- `shot2 <png>` only to check how something looks; read every screenshot you
  take. Read content from `tree`, never from a picture.

Replies you must handle:
- `err user-conflict: …`: the user used the app while you waited, and what
  you expected changed (named in the reply). Your command did not run. Run
  `tree`, `L check` the node, and re-decide.
- `err user-mode`: the user still has the app. Send the command again.
- `err no-target …` for an element you know should be there: a `ui-target`
  gap, unless the screen is simply not where you thought (run `tree`).

The user may take over at any moment. You never need to ask or wait for
them: commands block while they have it and resume when they leave, with the
view put back as you left it.

At the start, `mode` and `size`. If the size differs from your brief while you
are driving, `resize <w> <h>`.

## The loop

For each node you hold:

1. **Survey it** (`survey.py <root> <node>`): the database is the truth.
2. **Look at it in the app**: select it in the tree, open its task panel, and
   compare what the panel says (status line, who is next, requests, runner
   menu) with the survey. A mismatch is a `ui` or `status` gap.
3. **Triage every pending request** ([triage](../shepherd-common/triage.md)).
4. **If it is not making progress, find out why** before touching it
   ([reference](../shepherd-common/reference.md), "Diagnosing").
5. **Act through the GUI**: the runner menu (Resume, "Resume with a fresh
   budget", Check now, Rerun this phase, Run with a new session, Reset
   environment, Delete sandbox), answering a request (focus its field, `key
   enter`, `text …`, `key ctrl-enter`), the state chip for a move back. Right
   before every action, `L check <node> --owner <owner>`; on anything but
   `ok`, survey again and re-decide. Then confirm in the survey that it took.
   If the GUI path is broken, report the gap first; you may then use the
   shepherd `tod-cli` verbs ([operator-cli](../shepherd-common/operator-cli.md))
   so the node is not held up by it.
6. **Report gaps** ([gap reports](../shepherd-common/gap-report.md)), UI ones
   included, with the selector and the reply or screenshot as evidence.
7. **Record its verdict**: `L verdict <node> --owner <owner> <verdict>
   --reason "…" [--gap <id>] [--recheck <minutes>] [--action "…"]`, as the
   CLI shepherds do (`--action` empty means an empty visit).

Between actions, `L gate --owner <owner>`: `ok` go on; `pause` go straight to
`L wait` (a deploy will restart the app; your socket will drop until it is
back); `retire` record verdicts and retire.

## Waiting, pulling work, retiring

The same as a CLI shepherd:

```bash
L wait --owner <owner> --batch <nodes_per_shepherd> --linger <linger_minutes> --max 540
```

Visit what it returns (`due`, `new`, `deployed`); after `pause`, wait for
`resume`, then run `mode` and `tree` before anything else, since the app was
restarted. Retire on `retire`, `idle`, or after `max_visits` visits: verdicts
for every node you hold, `L release --owner <owner>`, then reply. Never
`sleep`.

Your reply: owner id and why you retired; one line per node visited (verdict,
what you did); each gap reported or joined (id, signature, nodes); and what
the skill should have told you.
