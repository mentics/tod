---
name: verification
description: Verification after implementing a new feature
disable-model-invocation: true
user_invocable: true
---

Launch a subagent that runs the app with `--agent mock` and drives it via the control socket on a fresh random port, iteratively fixing and verifying until the behavior is confirmed correct.
