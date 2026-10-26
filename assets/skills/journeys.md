# Reviewing a journey

A **journey** is the app's record of what happened: for a node, its
lifecycle transitions, gate results, what the user clicked (and whether it
was the highlighted button), agent turns, session rotations, and the data
that changed. The **project** journey is the user's own recent activity
across the app. A journey is recorded on this computer; nothing in it has
been sent anywhere.

## Reading one

```
tod-cli journey show <node-slug|node-uuid|project> [--since 2h] [--last 200] [--full]
```

The user may give you a reference instead: `journey:node/<slug>` or
`journey:project`. The part after `journey:` is the target.

Start narrow. A long journey is expensive to read whole: begin with
`--last 100`, or `--since` the time of interest, and widen only when you need
to. Each line is `time  gap-since-previous  seq  actor  event`.

## What to look for

- **Hesitation and backtracking**: long gaps, a transition followed by a
  move back, the same action repeated.
- **Ignored guidance**: a user action marked `[primary: X]` means the user
  chose something other than the highlighted button. Note what they chose
  and what was on screen.
- **Gates that failed**: `gate A -> B (N failing criteria)`, and what came
  next.
- **Validity regressions**: `validity regressed: <target>` after a state was
  reached.
- **Agent trouble**: repeated agent turns for one phase, `session rotated`,
  decisions that stopped a run.
- **Who it was waiting on**: `next actor` changes show where time went.

## Reporting

Answer what you were asked. When asked "how did it go", give the sequence in
a few sentences, the points where it went wrong or slowly (with the `seq`
numbers so the user can find them), and what you think caused each. Say when
the journey is too short or too old to support a conclusion. If the user gave
specific direction, follow it rather than this list.
