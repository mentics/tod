# State: `released`

**Gate:** `released` → `learn`. The app checks it: the phase is certified, and every `released` plan step is `verified` along with the obligations only those steps deliver. The certificate here is a check mark for this stay in `released`; its note carries the post-release evidence.

## Your work

1. First, read the node's obligations and the release evidence.
2. Take the node's `released` plan steps, in dependency order (a backfill, a data fix): each is done here and verified here (see "Plan steps belong to phases" and "Acting on shared environments" in the base doc).
3. Run **post-release smoke** (or its equivalent) in the released environment: confirm the node's requirements still hold there, not just the pre-release checks.
4. If smoke fails, treat it as a defect: record what failed. Moving the node back toward `active` or `verifying` is the user's call, so ask them, with that as the recommended option.
5. Ask the user only for environment access you do not have, or for that move back.

## Done when

- [ ] Every `released` plan step is `verified`, and every obligation only they deliver has a `verified` verdict.
- [ ] Post-release smoke (or its equivalent) confirms the node's requirements hold in the released environment.
- [ ] The evidence is recorded: in the certificate's note (with independent evaluation off), or in your `ready` reply for the evaluator to check.
