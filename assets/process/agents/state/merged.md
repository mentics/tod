# State: `merged`

**Gate:** `merged` → `released`. The app checks it: the phase is certified, and every `merged` plan step is `verified` along with the obligations only those steps deliver. The certificate here is a check mark for this stay in `merged`; its note carries the release evidence.

## Your work

1. First, read the merge evidence.
2. Take the node's `merged` plan steps, in dependency order, as part of the release: each is done here and verified here (see "Plan steps belong to phases" and "Acting on shared environments" in the base doc).
3. Drive the **release** to the agreed production or runtime environment: follow the node's or project's release process (deploy pipeline, tags, and so on), and watch the release and build pipelines.
4. On a failure, diagnose and fix what an agent can. Ask the user only for what needs them: a deploy permission, credentials, or a release decision that is theirs.

## Done when

- [ ] Every `merged` plan step is `verified`, and every obligation only they deliver has a `verified` verdict.
- [ ] The change is released to the agreed production or runtime environment. That release is the gate; never certify without an actual release.
- [ ] The release identifier is recorded: in the certificate's note (with independent evaluation off), or in your `ready` reply for the evaluator to check.
