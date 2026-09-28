# State: `released`

**Gate:** `released` → `learn`. The app checks it: the phase is certified. The certificate here is a check mark for this stay in `released`; its note carries the post-release evidence.

## Your work

1. First, read the node's obligations and the release evidence.
2. Run **post-release smoke** (or its equivalent) in the released environment: confirm the node's requirements still hold there, not just the pre-release checks.
3. If smoke fails, treat it as a defect: record what failed. Moving the node back toward `active` or `verifying` is the user's call, so ask them, with that as the recommended option.
4. Ask the user only for environment access you do not have, or for that move back.

## Done when

- [ ] Post-release smoke (or its equivalent) confirms the node's requirements hold in the released environment.
- [ ] The evidence is recorded: in the certificate's note (with independent evaluation off), or in your `ready` reply for the evaluator to check.
