# Self-improvement: lessons and the process review

The app records what went wrong in its own process as **lessons**, and a
periodic **process review** turns groups of them into drafted changes that go
through the lifecycle, so the user approves them. Design:
`doc/requests/design.md` section 11 (the view, 11.4, is deferred).

## Lessons

`improvement_lessons` (`tod_store::lessons`, schema v113, synced, journaled).
Each row has a `source`, links (node, request, conversation, send-back,
incident), a `category`, a `target` (where the fix belongs), a `summary`, the
user's own words when given (`user_quote`), a `status` (`new`, `grouped`,
`applied`, `declined`) and `applied_ref` (the commit or task that acted on it).

| Source | Written by |
|---|---|
| `triage` | the triage session's one reflection on a closed request (`record_triage_lesson`) |
| `send_back` | every send-back, automatically from its reason; a dispute is written once and revised when it resolves (`record_dispute_lesson`; a dispute resolved by a fix is `upstream_wrong`) |
| `ladder` | a quick fix that escalated (`record_quick_fix_escalation`); a configured fixer that failed (`record_fixer_failure`, `Op::FixerFailed`) |
| `incident` | a tool incident (`record_tool_incident`) |
| `user` | `tod-cli lessons add` |

Every mechanical writer carries an `event_key` (unique), so a replayed event,
a restart or a second writer never records the same lesson twice. The
migration (`migrate_request_feedback`) moves the rows of the old
`request_feedback` table into lessons on upgrade.

Writes go through `InterviewCommand::Lessons(Op)` on the fleet writer.
`Decline`, `Group` and `FixerFailed` are refused for agent sessions; `Apply` is
allowed only from a process review session, and only for the lessons its own
run took in.

## `tod-cli lessons`

`list`, `show`, `stats`, `add`, `decline`, `apply` (`cli/lessons.md`, checked by
`doc_sync`). `stats` (`tod_store::lesson_stats`) prints the measures of
section 11.3: requests by reason and asking protocol, who settled them,
triage's rates, time to answer, cost at list prices for triage and quick fix,
incidents and the nodes they absorbed, send-backs by target phase. What the
data cannot give is listed under "Not measured" (`Stats::unavailable`) rather
than guessed.

## The process review

`ProtocolKind::ProcessReview` (`tod_core::conversation::process_review`,
recipe `PROCESS_REVIEW`, `surface/process-review.md`, lifecycle config key
`process_review`: Sonnet at low effort unless set). Settings
(`process_review` in the settings file):

- `parent`: the node under which each run's node is made. Unset means the
  review is off.
- `project_parent`: optional; groups whose target is project scoped (a
  project skill, `.claude/skills`) go in a second node under it. Without it
  they are in the one node.
- `every_hours` (24) and `min_lessons` (10): a review is due when the parent is
  set, at least `min_lessons` lessons are new, and `every_hours` have passed
  since the last grouping.

The daemon's tick (`process_review_pass`, `tod-agentd`) calls
`process_review::start_due`: it groups the new lessons by target and cause
(`process_review::groups`; `brief_unclear` lessons with the user's quotes
first), marks them `grouped` with the run node (`run_node_id`, `grouped_at`),
and creates the run node under the parent with Spec and Lifecycle, whose
description lists the groups by lesson id and node slug. The node inherits the
parent's Files location, so the session works in a checkout of the repository
that holds the targets.

The review session runs as the autopilot's `process_review_step` when that node
runs (`process_review::owed`: lessons grouped and no review conversation yet).
It drafts the changes as commits in the node's checkout (context fragments,
role docs, triage's brief guidance, the request template, a project's skills;
the `node-request-brief` skill is where the brief format starts) and marks the
lessons its commits address with `tod-cli lessons apply --ref <commit>`. The
node goes through the lifecycle like any other, so nothing edits a prompt
silently. A lesson the user declines stays out of later groups.

## Known gaps

- The daemon only creates the run node; the review session starts when the
  node is run or started (the autopilot step), not by the daemon.
- Links in the node's description are slug references, not link URLs.
- No daemon-level test of the enabled pass (it needs time travel for
  `created_at`); due, grouping and node creation are tested in `tod-core`.
- Stats cannot give a request's lifecycle phase (the asking protocol is used),
  the share of items rung 0 settled, per-request cost beyond triage and quick
  fix, or the sessions saved by an incident beyond an assumption.
- The self-improvement view is deferred; `tod-cli lessons` and the run nodes
  are how the loop is seen.
