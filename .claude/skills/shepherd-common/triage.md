# Triage: requests to the user, and when a node is parked

## Requests to the user

A node waiting on the user is a case to triage, not just to relay.

These are tod's bugs, not questions, whatever the request says:
- an obligation that can only hold after rollout or a production run, asked
  in an earlier phase: its `acts_in` is wrong;
- "unreachable", "not connected", "not found", or a missing tool, while tod is
  running: the plumbing failed.

For each request, decide:

- **Can a shepherd resolve it?** Access, connections, network and
  credentials plumbing, configuration, a missing tool in the sandbox, or a
  broken setup. Resolve what you can from the node (reset its environment,
  rerun), report the request itself as a gap (tod should have handled it, or
  never needed it), and once the fix is deployed answer it ("Fixed, try
  again") so the node continues. This is the one kind of request a shepherd
  answers itself; say so in the node's verdict.
- **Is the request unclear, or does it ask for something tod could have
  decided?** Requests must follow "Wording a request" in
  `crates/tod/media/context/stance/autonomous-session.md`. Report a gap
  against the prompt or context that produced it.
- **Only the user can answer** (a product decision, an obligation that is
  wrong, a priority): the node is `parked`. Say so in its verdict, with the
  request id, so the manager can make a brief with the `node-request-brief`
  skill.

## Parked: nothing to do until a person acts

Only in these cases, and only while every condition holds:

- `pr_draft`, waiting for the user to mark the PR ready and pick a reviewer:
  checks green, the review bot's verdict current for the head, no thread
  open, the PR mergeable, tod's own review clean.
- `pr_review`, waiting only for a person's review: the same conditions. A
  person's comment or "changes requested" is work for tod's babysitter, not a
  stop.
- `approved`, waiting only for a person to merge: the PR still clean and
  mergeable (a base that moved on or a new conflict is tod's to fix).
- `merged`, waiting only for a release whose notes name the PR.
- A request that only the user can answer (above).

Anything else is progress to be made. In `merged` (after the release),
`released` and `learn` the phase agent does the work and the gate checks it;
a stop there is a gap like any other.

A node is **finished** when it reaches `done` (and its sandbox is retired).

## Verdicts

Every node you hold ends each visit with one verdict in the ledger:

| Verdict | When | Comes back |
|---|---|---|
| `in_motion` | tod is working, or waiting on CI, a bot, a webhook | at `--recheck-minutes`, or when its fingerprint changes |
| `parked` | one of the parked cases above | when its fingerprint changes (a person acted) |
| `blocked_on_gap` | stuck on a tod gap you reported | when the gap is deployed |
| `ceiling` | stuck, and no gap or action of yours can move it | when the shepherd changes, or it is cleared |

`ceiling` is rare: almost every stuck node is a gap to report. Use it when the
cause is outside tod (a broken upstream service, a repo that needs a person's
decision) and say exactly what would unblock it.
