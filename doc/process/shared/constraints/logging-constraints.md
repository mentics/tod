# Logging constraints

Reusable logging practices for applications. Projects that adopt this document treat it as a binding constraint.

## Levels

| Level | When to emit |
|--|--|
| **error** | Failures and unexpected conditions. Include enough troubleshooting detail to analyze what went wrong. |
| **info** | Successful completion of a significant action (see Canonical log lines). Major lifecycle events. |
| **debug** | Pre-call markers before external calls (see Desktop / non-request environments). Temporary investigation detail. |
| **trace** | Finer temporary investigation detail. |

Default configured level: **info** (info and above).

Temporary **debug** / **trace** lines added while diagnosing a problem should be cleaned up once the issue is fixed.

## Canonical log lines

A **canonical log line** is one structured, information-dense log line for a unit of work (the analogue of a web request), emitted when that work is resolved. Fields are colocated so the line is easy to parse, filter, and aggregate without reconstructing a story from many sparse lines.

Emit a canonical log line:

- At **info** when the action succeeds.
- At **error** when the action fails (with troubleshooting detail).

Do **not** emit a canonical log line for every UI click, navigation, or other low-level interaction.

Background (pattern origin): [Stripe — Canonical log lines](https://stripe.com/blog/canonical-log-lines).

## Desktop / non-request environments

Adapt the same idea when there is no HTTP request boundary:

1. Treat a **significant action** as the unit of work (user- or system-initiated work with a clear start and resolution).
2. Emit a canonical log line when that action resolves (**info** on success, **error** on failure).
3. For **external calls** (network, OS integrations, subprocesses, etc.):
   - Immediately **before** the call: a canonical-style line at **debug** (useful while things are still unreliable in development).
   - **After** the call resolves: a canonical log line at **info** (success) or **error** (failure).
4. **Major lifecycle events** (process start/stop, significant mode or environment transitions, and similar) also get an **info** canonical log line.

## Runtime level control

The process must support selecting the minimum emit level (for example info and above, debug and above, or trace and above) via:

1. A **command-line argument**, and
2. An **in-application settings** control where the product has settings.

Product-specific requirements may spell out how settings persist or appear in the UI; this document only requires that both controls exist when applicable and that they select the same kind of level threshold.
