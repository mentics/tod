# State: `verifying`

**Forward gate:** `verifying` → `review`

## On entry

This runs as the node's verification conversation, started from the lifecycle panel's **Verify** button, not automatically on entry.

1. Read lifecycle state, resolved obligations (including inherited and design-phase) and plan steps, implementation, and evidence from `active`.
2. Ship-with-code tests should already exist from `active`.
3. **Verify every obligation and record the verdict on the obligation itself**, through the `verdicts` noun, with the evidence. The plan exists to satisfy the obligations, so they — not the steps — are what verification rules on: a plan whose every step checks out can still add up to work that does not run.
   - Exercise each obligation (success criteria when present, otherwise the measurable statement) end to end in the running work, the way its user would meet it. Code reading and unit tests are not evidence that a requirement holds.
   - Holds: `verified`, with what you ran and saw. Does not hold, or could not be exercised: `failed`, with the same.
   - Do the same for each inherited constraint that applies to this work.
4. **Then verify every plan step and record the verdict on the step itself**, through the `plan` noun — this is how verification's findings get back to implementation, so a finding that is only in your reply is lost:
   - Check each step that is not already `verified` against what it says and every obligation it `--satisfies`, by running it. A step whose obligation failed because of it is `failed`.
   - Passed: set it `verified`.
   - Not done, done wrong, or not working: set it `failed` with a note. The note is what the implementation agent starts from on its next attempt, and the latest note is the only one it is shown, so make it complete on its own: what you checked, how (the command or steps), what happened, and what was expected. Name the obligation it falls short of.
   - A failed obligation that no `failed` step satisfies: fail the step that should have delivered it, or add a plan step that `--satisfies` it and set that step `failed` with a note the same way. Implementation works from failed steps, so a failure recorded only on the obligation never reaches it.
   - A step already `verified` whose code changed since: check it again, and set it `failed` if it no longer holds.
5. A step's earlier notes are its history (`plan show`). If a step has failed before, read them: a failure that repeats deserves a note that says so, and says what the earlier attempts missed.

## Responsibilities

### Run verification

The **agent** runs the checks — do not hand primary verification to the human. Human look-over (interactive mode) and external approval (`review` → `approved`) are approval / safety net, not the first time the work is exercised.

Runtime exercise of slices claimed complete should already have happened in `active`; this state finishes the **full** obligation sweep.

Execute **every requirement**: run attached success criteria when present; otherwise verify the measurable requirement statement itself. Also run applicable inherited constraints. Record each verdict and its evidence through the `verdicts` noun — the forward gate reads those verdicts, and a requirement with none counts as unchecked.

Re-exercise in **running context** as needed for the full sweep (prefer automated end-to-end; otherwise drive the running system). Build and run **local-only** harnesses and one-off checks here when gaps remain.

### Test strategy

- **Rule out the environment first** — When a running-context / integration / E2E check fails: first rule out environment (wrong build, stale process, harness/focus, fixture paths) before calling it a product defect.
- **Record defects, don't fix them here** — A product defect fails its plan step, with a note precise enough to reproduce it; implementation, back in `active`, makes the fix. Narrow it down as far as you can (the smallest input or command that shows it) so the note points straight at the cause.
- **No bug hiding** — Do not weaken tests, guess constants, or special-case fixtures to pass.
- **Evidence** — Pass/fail and how verification was run belong in notes; ship-with-code tests belong in the repo.

### Traceability

Results must trace upstream through plan steps (`--satisfies` links, each step `verified`) and obligations, including design-phase ones. The obligation's verdict and the step's status are the record: `verified` or `failed` with evidence or a note, never only a line in your reply.

### Revalidate conformance

Re-check upstream conformance when artifacts changed since last gate.

### Self-code review

Complete self-review before `review`. **`review` is not where functional bugs are found**—enter `review` only when near-certain of release readiness.

## Forward gate rules (`verifying` → `review`)

**Critical gate — do not treat `review` as the place that finds bugs.** Builder/verifier responsibility is near-certainty of release readiness.

Apply these prose rules in addition to checklist criteria the app sends:

- Verification is complete: **every requirement** in applicable obligations (node + ancestors as bound, including design-phase) has been checked (success criteria when present, otherwise the measurable statement) and is **traceable** upstream through the plan steps that satisfy it (each `verified`) and those obligations.
- Verification was **agent-executed** in the work’s running context (harness built if needed); not deferred to human look-over as the primary check.
- **Constraints, both directions** — check this node's constraints and every inherited one (listed under Inherited context). Answer two questions; the criterion passes only if both are yes:
  1. Is the implementation free of anything a constraint forbids? (Many constraints say what must *not* be done.)
  2. Does the implementation do everything a constraint requires?
  If either is no, set that criterion's `gate_results` row to `outcome: fail` (the reply is then `result: blocked`) and, in the row's `detail`, name each constraint and what in the implementation breaks or misses it.
- Upstream conformance **revalidated** (or short-circuited only for unchanged file pairs).
- **Self-code review** completed.
- Ancestor or node-specific verification extras (static analysis, etc.) satisfied when defined as obligations.
- Entering `review` then runs an **independent** code review (clean subagent not involved in construction/docs; use a code-review skill when available).

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `verifying` → `review` gate passes (checklist included), return `forward_lifecycle: review`.

## Blockers

Failed verification, untraceable results, or known functional defects → stay in `verifying` or move back to `active` for fixes; do not enter `review` hoping review will catch bugs.

Any `failed` or unchecked obligation, and any `failed` plan step, blocks the gate. The fix is not made here: the user moves the node back to `active`, where implementation works every `failed` step again from its note, and back in `verifying` the user's **Verify** runs this state's verification over the result.
