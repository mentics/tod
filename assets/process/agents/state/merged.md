# State: `merged`

**Gate:** `merged` → `released`. The app checks it: the phase is certified. The certificate here is a check mark for this stay in `merged`; its note carries the release evidence.

## Your work

1. First, read the merge evidence.
2. Drive the **release** to the agreed production or runtime environment: follow the node's or project's release process (deploy pipeline, tags, and so on), and watch the release and build pipelines.
3. On a failure, diagnose and fix what an agent can. Ask the user only for what needs them: a deploy permission, credentials, or a release decision that is theirs.

## Done when

- [ ] The change is released to the agreed production or runtime environment. That release is the gate; never certify without an actual release.
- [ ] The release identifier is recorded: in the certificate's note (with independent evaluation off), or in your `ready` reply for the evaluator to check.
