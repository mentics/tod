# State: `verifying`

**Forward gate:** `verifying` → `review`

## On entry

1. Read lifecycle state, resolved obligations (including inherited), design and plan content, implementation, and evidence from `active`.
2. Ship-with-code tests should already exist from `active`.

## Responsibilities

### Run verification

The **agent** runs the checks — do not hand primary verification to the human. Human look-over (interactive mode) and external approval (`review` → `approved`) are approval / safety net, not the first time the work is exercised.

Runtime exercise of slices claimed complete should already have happened in `active`; this state finishes the **full** obligation sweep.

Execute **every requirement**: run attached success criteria when present; otherwise verify the measurable requirement statement itself. Also run applicable inherited constraints. Record pass/fail evidence.

Re-exercise in **running context** as needed for the full sweep (prefer automated end-to-end; otherwise drive the running system). Build and run **local-only** harnesses and one-off checks here when gaps remain.

### Test strategy

- **Narrow failures for iteration** — When a running-context / integration / E2E check fails: first rule out environment (wrong build, stale process, harness/focus, fixture paths). If it’s a product defect, **write or extend a focused unit (or narrow) test that fails for that cause, fix against that loop, then re-run the broader check once.** Do not use the slow UI/integration loop as the primary edit–run cycle.
- **No bug hiding** — Do not weaken tests, guess constants, or special-case fixtures to pass.
- **Evidence** — Pass/fail and how verification was run belong in notes; ship-with-code tests belong in the repo.

### Traceability

Results must trace upstream through plan, design (if any), and obligations.

### Revalidate conformance

Re-check upstream conformance when artifacts changed since last gate.

### Self-code review

Complete self-review before `review`. **`review` is not where functional bugs are found**—enter `review` only when near-certain of release readiness.

## Forward gate rules (`verifying` → `review`)

**Critical gate — do not treat `review` as the place that finds bugs.** Builder/verifier responsibility is near-certainty of release readiness.

Apply these prose rules in addition to checklist criteria the app sends:

- Verification is complete: **every requirement** in applicable obligations (node + ancestors as bound) has been checked (success criteria when present, otherwise the measurable statement) and is **traceable** upstream through plan, design (if any), and those obligations.
- Verification was **agent-executed** in the work’s running context (harness built if needed); not deferred to human look-over as the primary check.
- Upstream conformance **revalidated** (or short-circuited only for unchanged file pairs).
- **Self-code review** completed.
- Ancestor or node-specific verification extras (static analysis, etc.) satisfied when defined as obligations.
- Entering `review` then runs an **independent** code review (clean subagent not involved in construction/docs; use a code-review skill when available).

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `verifying` → `review` gate passes (checklist included), return `forward_lifecycle: review`.

## Blockers

Failed verification, untraceable results, or known functional defects → stay in `verifying` or move back to `active` for fixes; do not enter `review` hoping review will catch bugs.
