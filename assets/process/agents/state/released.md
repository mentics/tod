# State: `released`

**Gate:** `released` → `learn`. The app checks it: the phase is certified, and every `released` plan step is `verified` along with the obligations only those steps deliver. The certificate here is a check mark for this stay in `released`; its note carries the post-release evidence.

## Your work

1. First, read the task's obligations and the release evidence.
2. Take the task's `released` plan steps, in dependency order (a backfill, a data fix): each is done here and verified here (see "Plan steps belong to phases" and "Acting on shared environments" in the base doc).
3. Run **post-release smoke** (or its equivalent) in the released environment: confirm the task's requirements still hold there, not just the pre-release checks.
4. If smoke fails, treat it as a defect: record what failed. Moving the task back toward `active` or `verifying` is the user's call, so ask them, with that as the recommended option.
5. Ask the user only for environment access you do not have, or for that move back.
6. Evidence that is only **not there yet** because something is still running (the release's deploy, the pull request's or `main`'s CI, a post-merge workflow, a job that runs after the deploy) is never a question for the user: wait for it yourself, inside this turn. Poll it (`gh run watch`, or a loop with `sleep` and a status check, on the runs for the merge commit or the release tag) until it finishes or its own timeout passes, then collect the evidence from it and record `ready`. Never end the turn with "I'll resume when it reports", a background watcher or a scheduled check: nothing wakes this session afterwards, the turn that changes nothing stops the run as `no_progress`, and the user has to restart it.
7. Evidence that **cannot** be produced by waiting (no access to the released environment, a nightly job that cannot run until later than you can poll): do **not** record `ready` without it. Ask the user how to proceed (`decisions ask`, options such as accept without evidence, wait, or grant access) and end your turn. A `ready` with the same missing evidence is rejected, and the second identical rejection stops the run. When the user answers "wait", that is item 6: the waiting happens inside the turn that follows.

## Done when

- [ ] Every `released` plan step is `verified`, and every obligation only they deliver has a `verified` verdict.
- [ ] Post-release smoke (or its equivalent) confirms the task's requirements hold in the released environment.
- [ ] The evidence is recorded: in the certificate's note (with independent evaluation off), or in your `ready` reply for the evaluator to check.
