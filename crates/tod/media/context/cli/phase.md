## `tod-cli phase`

Whether the node's current lifecycle phase is done. Every command acts on the
state the node is in now; there is no option to name another.

```
tod-cli --data-root <DATA_ROOT> phase status  [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> phase ready   [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> phase certify [--node <NODE_UUID>] --note <TEXT>
tod-cli --data-root <DATA_ROOT> phase reject  [--node <NODE_UUID>] --fix <TEXT> (repeatable)
```

Inside a conversation `--node` defaults to the node it is about.

- `status` shows the node's state, each gate criterion for leaving it with
  the app's verdict right now, and the phase's certificate: none, current
  (who certified it, and their note), or stale, with what changed since it
  was certified.
- `ready`: the phase's work is done; have it evaluated. A separate session
  then judges it and certifies it or sends it back.
- `certify --note`: the phase is done. The app records a digest of what you
  judged, and the gate passes only while it is unchanged: any later edit to
  what the phase covers makes the certificate stale, and the phase comes back
  to be judged again. The note is required: one line saying why it is done,
  or, for a release, the evidence.
- `reject --fix`: send the phase back to its agent with the fixes it must
  make, one per `--fix`. Use this for anything an agent can fix confidently;
  ask the user (`decisions ask`) only for what no agent can settle.
