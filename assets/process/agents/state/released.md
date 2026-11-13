# State: `released`

**Gate:** `released` → `learn`. The app checks it: the phase is certified, and every `released` plan step is `verified` along with the obligations only those steps deliver. The certificate here is a check mark for this stay in `released`; its note carries the post-release evidence.

## Your work

1. First, read the task's obligations and the release evidence.
2. Take the task's `released` plan steps, in dependency order (a backfill, a data fix): each is done here and verified here (see "Plan steps belong to phases" and "Acting on shared environments" in the base doc).
3. Run **post-release smoke** (or its equivalent) in the released environment: confirm the task's requirements still hold there, not just the pre-release checks.
4. If smoke fails, treat it as a defect: record what failed. Moving the task back toward `active` or `verifying` is the user's call, so ask them, with that as the recommended option.
5. Ask the user only for environment access you do not have, or for that move back.
6. If the evidence cannot be produced yet (no access to the released environment, a nightly job that has not run since the deploy), do **not** record `ready` without it: ask the user how to proceed (`decisions ask`, options such as accept without evidence, wait, or grant access) and end your turn. A `ready` with the same missing evidence is rejected, and the second identical rejection stops the run.
7. When the user's answer is to **wait** (for a deploy, a job, a nightly run), the waiting happens inside this turn: poll it yourself (`gh run watch`, or a loop with `sleep` and a status check) until it finishes or its own timeout passes, then collect the evidence and record `ready`. Never end the turn with "I'll resume when it reports" or a background watcher: nothing wakes this session afterwards, the turn that changes nothing stops the run as `no_progress`, and the user has to restart it. If the wait is longer than you can poll, say so in a new `decisions ask` instead of leaving.

## Done when

- [ ] Every `released` plan step is `verified`, and every obligation only they deliver has a `verified` verdict.
- [ ] Post-release smoke (or its equivalent) confirms the task's requirements hold in the released environment.
- [ ] The evidence is recorded: in the certificate's note (with independent evaluation off), or in your `ready` reply for the evaluator to check.
