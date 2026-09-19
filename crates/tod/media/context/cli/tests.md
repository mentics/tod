## `tod-cli tests`

How the app learns whether the work's tests pass. It only exists inside an
implementation or verification conversation; outside one it fails.

```
tod-cli --data-root <DATA_ROOT> tests record --command <TEXT> --passed <N> [--failed <N>] [--errors <N>]
```

`record` stores the counts from the test run you just made: `--command` is
the command you ran, and `--errors` counts tests that could not reach a
verdict (a setup panic, a timeout). Counts you leave out are zero. A later
`record` in the same turn replaces the earlier one, so the last one you record
is what the app reads — record the final run, after your last change.
