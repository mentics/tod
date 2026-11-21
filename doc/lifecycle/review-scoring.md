# Scored review findings

Built from `doc/requests/design.md` section 5.1 (build step 9). Code:
`tod_store::review` (the scores, the `left` status, schema v117),
`tod_core::review_scoring` (what blocks, the lanes, the check suggestions),
`tod_core::autopilot::quick_fix` (`settle_review_round`).

## The scores

Every finding in `review_findings` has three, required when a reviewer adds
one (`tod-cli review add`), checked by the store:

| Score | Values | Meaning |
|---|---|---|
| `importance` | `blocker`, `major`, `minor`, `nit` | How much it matters. A project rule broken without harm is at most `minor`; taste is a `nit`. |
| `impact` | `high`, `medium`, `low` | How much harm leaving it does (reach into users, data, production, other code). |
| `complexity` | `mechanical`, `simple`, `involved` | What the fix takes. |

A `mechanical` finding may also carry a `rule` (below). Migration v117
replaces `severity` (`high` becomes blocker/high, `medium` major/medium, `low`
minor/low, all complexity `simple`); the table is rebuilt (its CHECK cannot be
altered), and the journey and sync triggers are made again. `node_quick_fixes`
is rebuilt the same way to accept the new source `review`.

## What blocks

`review.minor_rounds` in `tod.yml` (default 2):

- `blocker` and `major` always block;
- `minor` blocks in the first `minor_rounds` rounds of a stay (0: never);
- `nit` never blocks.

Every reader (`review_passed`, the `own-review` criterion, the round limit,
`fix_escalation`, `review_progress`, the pull request review checks) asks one
predicate, `ReviewFinding::stands`: not dismissed (`out_of_scope`, `declined`,
`left`) and not a fix a quick fix made that the small review cleared. So what
does not block is settled, not skipped by each reader.

## Settling a round

When a review round is done (both reviewers recorded done),
`Autopilot::settle_review_round` routes each finding it left open
(`review_scoring::route`):

| Finding | Where it goes |
|---|---|
| blocks, `mechanical` or `simple` | the ONE quick fix session of the round |
| blocks, `involved` | Fix, as before |
| `nit`, `mechanical` | the quick fix session (fixed only because it is cheap) |
| `nit`, anything else | `left` at once, reason "nit" |
| `minor` after the blocking rounds | `left`, reason "minor after round N" |

`left` is a status with tod's reason as its response; it is noted on the node's
Activity (one note per round). The user can reopen a left finding in the
findings panel (status menu, "open"): it then blocks like a `major` (a minor or
nit is promoted; a blocker stays one). The panel also shows the three scores,
and a click on a score moves it to its next value
(`RescoreReviewFinding`, recorded as a journey `UserAction`).

### The quick fix lane (`review` only)

The `quick_fix` protocol (step 5) gets a source `review`: the findings, with
their scores, places and details, are its target (one turn, no check to run).
`tod_store::quick_fixes::QuickFix::command` holds the finding ids. The commit
it makes goes to the small-change review (`small_review`), not to a round:

- clean: each finding is settled `fixed` with the quick fix recorded on it
  (`SettleReviewFinding`), so it no longer stands and no new round is owed for
  it; a note says so;
- the quick fix made no commit, or the small review recorded a problem: the
  attempt is `escalated` (which records a `ladder` lesson), the findings that
  block stay open for Fix, and a nit it could not fix is `left`. What the small
  review recorded is open on the node, so Fix takes it.

In `pr_draft` and in tod's review of a change before it is pushed
(`review_change`) nothing is fixed this way (a commit there needs its own push
and review): what blocks goes on as before, and what does not is left.

## Check suggestions

A mechanical finding may cite the rule it breaks (`--rule`). Two findings cite
the same rule when their text is equal after lowercasing, collapsing runs of
whitespace and trimming punctuation at both ends (`review::rule_key`): no
model, no fuzzy matching, so a reviewer must word a rule the same way each
time (the docs for reviewers say so). When mechanical findings on **three
different nodes** cite one rule, `review_scoring::rule_suggestions` yields a
suggestion titled "Add a check for: <rule as first worded>", filed through
`tod_core::suggestions::file` by the node whose round it was, once (no
suggestion with that title may exist on any node).

## Tests

- `tod-store`: `review::tests` (scores validated, `left`, reopening promotes,
  a quick-fixed finding stops standing, rule matching),
  `fleet::schema::tests::v117_*`, `quick_fixes::tests::a_table_from_before_v117_*`.
- `tod-core`: `review_scoring::tests` (blocking, lanes, rule suggestions),
  `autopilot::tests` (quick fix lane, nit left, minor left after round 2, a
  failed quick fix hands its findings to Fix).
- `tod-cli`: `tod_cli::doc_sync` pins `cli/review.md` to the command's USAGE.
- `tod-ui`: `unified::panels::findings::tests` (score click, reopening a left
  finding).
