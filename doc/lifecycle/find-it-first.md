# Find it first: every issue caught before anyone else looks

Status: plan, partly built (items 9 and 12, and item 10 in `pr_review`, on
2026-11-19; see the work items). Written 2026-11-19 from the post-mortem of
Cofactr/kb PR 2888 (node "DigiKey productdetails 404s: ~50% of lookups
failing, most avoidable").

## Goal

No person and no outside bot (Greptile or any other) finds an issue that tod
could have found first. A pull request that leaves tod should be one we would
merge.

That needs two things at once: the work is built right the first time, and
the review catches what slipped through anyway. Neither is enough alone.

**Measure:** for every PR, count the findings made by anyone outside tod
(reviewers, review bots) that were real and that tod could have found. The
target is zero. The `learn` phase records the count and each miss's cause
(see [Learn](#11-learn-account-for-every-miss)).

## What happened on PR 2888

tod ran 3 phase agents and their evaluators, 2 implementations, 3
verifications, 30 reviews and 18 fixes on this PR. A bot reviewed it before
it was marked ready. After ready, a coworker and the bot still found six real
issues, two of them P1. An independent review of the head afterwards found
more.

| # | Found after ready | Began in | Why tod missed it |
|---|---|---|---|
| 1 | SKU 404 → MPN fallback's "Duplicate" answer cached 30 days as Not Found | First implementation | No one traced how the fallback interacts with the 30-day negative cache |
| 2 | A wrong org manufacturerId turns a working lookup into a Not Found cached 30 days; 113 orgs carry several DigiKey ids and the helper picks one arbitrarily | Design | "One DigiKey id per org" was never checked against data; the reused helper was never read; no regression run on lookups that work today |
| 3 | MPN-correction queries seq-scan an 11.4M-row table with `FOR UPDATE` on the prod writer | Fix rounds | EXPLAIN was run only on an early shape of the query |
| 4 | No test of the cancellation path | An ad-hoc push | That push had no review and no checks |
| 5 | Five pyright errors | Fix rounds | Pushed before checks ran |
| 6 | Bare retry can adopt another maker's part and attach its offers (data corruption) | Fix 78271879 | 11 delta-only reviews; a redesign done inside the fix loop |

Still wrong at the head (fca2cca7), checked against the code: gap-fill sends
only the first candidate id; the negative cache ignores an MPN change; the
negative-cache gate never runs on the background path. Several more are
plausible (see the post-mortem conversation).

Numbers behind the root causes:

- Design took 66 seconds at default effort, and in its own words had not
  "run anything against DigiKey or the databases". Its evaluator certified it
  in 18 seconds.
- Verification's evidence was the implementer's mocked unit tests. No DigiKey
  credentials were available, so the success measurement was moved to after
  release instead of asking for them.
- **0 of 30 reviews** read the project's `REVIEW.md` or any `CLAUDE.md`, both
  of which the project's review skill says to read first. 2 used `git-info`.
  9 opened any file the routing table points to, mostly the schema skill.
- Reviews averaged **one finding per round**. The multi-id problem alone was
  found in five pieces (findings 17–22), one per round.
- Ten fix sessions ran `git push` before the review was clean.
- After the coworker's comment showed a design assumption was false, ten fix
  commits in 70 minutes added three new mechanisms (candidate iteration, bare
  retry, maker check). Nothing went back to design or to the user.

## Principles

1. **Every phase challenges what it was given.** A ticket, a requirement, a
   design obligation, a reviewer's comment: each is a claim to check. Asking
   the user is not checking.
2. **Investigate first, ask last.** An agent never asks what it can find out
   itself (the code, the data, the live API, the history). It asks only what
   it cannot settle convincingly after looking, and says what it found.
3. **Evidence comes from the real system.** A unit test with a mock is
   evidence that the code does what the test author believed. It is not
   evidence that the belief is true.
4. **Any phase can send the work back as far as the cause.** A false
   assumption found in review goes back to the phase that made it, not to a
   patch.
5. **A review is comprehensive in one round.** It sweeps the whole change
   against a fixed set of lenses and keeps going after the first finding.
6. **What matters is enforced or recorded, never only asked for.**
   Instructions that nothing checks are the first ones dropped.
7. **Nothing reaches the pull request without tod's review and checks.**

## Changes by phase

### 1. Proposed: challenge the ticket

Today `proposed.md` turns the ticket into requirements as written. A ticket is
someone's understanding at one moment; it can be wrong about the cause, the
numbers, or the fix it suggests.

The proposed agent:

- Lists the ticket's **claims**: what is broken, how much, why, and the fix it
  suggests ("~50% of lookups failing, most avoidable"; "pass the
  manufacturerId").
- **Checks each claim** it can: reads the code involved, queries the data
  (`tod-cli db`, read-only), calls the live API where it is cheap, reads the
  history. It records what it found on the node (the details, or a requirement
  with the evidence).
- Writes requirements from what is **true**, not from what the ticket said.
  Where the ticket's suggested fix is a means, not the end, the requirement
  names the end (fewer avoidable 404s), and the means goes to design.
- **Pushes back.** A claim that turns out false or doubtful becomes a finding
  in the details. If it changes what the task is for, that is a question for
  the user (`decisions ask --reason intent`), with the evidence and a
  recommendation. Anything it could settle itself it settles.

New **Done when** items in `assets/process/agents/state/proposed.md`:

- [ ] Every factual claim in the ticket that the requirements depend on was
      checked against the code, the data, or the live system, or is recorded
      as unchecked with the reason.
- [ ] No requirement rests on a claim found false.
- [ ] No question was asked that the code, data, or history could answer.

The evaluator (`crates/tod/media/context/surface/evaluate.md`) gets the same
items and must see the evidence, not prose that says it was checked.

### 2. Design: validate every assumption and analyze every failure

Design is where PR 2888's biggest defect began. The design agent:

- Lists the **assumptions** the design rests on: about data (cardinality,
  uniqueness, ordering, nulls, scale), external systems (an API's answers,
  errors, limits, quota), and the existing code it reuses (what a helper
  really returns). "Each org has one DigiKey id" and "a manufacturerId only
  narrows" were two such assumptions here; one query would have shown 113
  orgs with several ids.
- **Checks each one** against the real thing: the code it calls (read it, not
  its name), production data (read-only), the live API. One it cannot check
  becomes a spike with a decision tree, as today.
- **Analyzes failure modes:** for every new path that can turn a success into
  a failure, or that **persists** a negative result (a cache, a TTL, a stored
  status), it states what a wrong input does, how many items it reaches, how
  long it lasts, and how it is undone. A wrong filter in front of a 30-day
  negative cache is the shape of three of PR 2888's defects.
- **Analyzes interactions** between the behaviors it adds (fallback × status
  keying, filter × cache, retry × maker identity).
- Writes the results as obligations where an implementer would otherwise get
  it wrong, as today, plus a verifying-phase obligation for each risk that
  only a live run can show.

New **Done when** items in `assets/process/agents/state/design.md`:

- [ ] **Assumptions checked:** every assumption about data, external systems,
      or reused code is checked against the real thing, with the evidence on
      the node, or is a spike with its decision tree.
- [ ] **Failure modes:** every new failure path and every persisted negative
      result has its blast radius and its undo stated.
- [ ] **Interactions:** every pair of new behaviors that touch the same item
      was considered together.
- [ ] **Live checks named:** each risk only a live run can show has a
      `verifying` obligation.

The evaluator checks these with evidence. Design runs at high effort (already
set on `all-tasks` since 11-19).

### 3. Planning: line up what verification will need

Live verification fails late when credentials, data, or quota are missing
(PR 2888 had no DigiKey credentials and moved the check to after release). The
planning agent:

- Lists what live verification needs: credentials (`tod-cli secrets list`),
  read access (`tod-cli access list`, `tod-cli db`), test data, API quota.
- Asks for anything missing **now**, with `decisions ask --reason access`, so
  it is ready by `verifying`. A missing credential never moves a check to
  `released`.
- Plans the regression replay (below) as a `verifying` step.

Done-when item in `assets/process/agents/state/planning.md`: everything live
verification needs is available, or requested.

### 4. Active: depth, not speed

Time was not the problem: 4.4 minutes is fine for a change this size. Depth
was: the reused helper was never opened, and the data script was never run.

In `assets/process/agents/state/active.md` and
`crates/tod/media/context/surface/implement.md`:

- **Read what you reuse.** Before a new path calls an existing function, read
  that function (its query, ordering, cardinality, error cases). A function
  used for a new purpose is new code for review purposes.
- **Code that will be kept gets automated tests**, including code a script
  imports from a library.
- **One-off data remediation scripts** need no unit tests, but they must be
  **run live** before review: against a local database seeded with crafted
  data that covers each case (including collisions and the cases it must
  skip), and a read-only dry run against production. Any query on a large
  table gets an EXPLAIN on production first, and a query that locks rows says
  why the plan is safe.
- Run the project's checks (lint, types) before recording a step
  implemented.
- `active` runs at high effort (set in the lifecycle config; today it runs at
  default).

### 5. Verifying: live, against the real system

This is what `verifying` was meant to be, and it is what `verifying.md`
already says ("Code reading and unit tests are not evidence that a
requirement holds"). It did not happen because nothing enforces it.
`verifying` has no evaluator, and its gate checks only that each verdict says
`verified`. The verifier judges its own evidence, which is often the
implementer's tests. This is a major change, in four parts.

**1. Live checks are planned from design, as obligations of their own.**

- For each risk only a live run can show, design writes an obligation that
  acts in `verifying` (`--acts-in verifying`; already supported, rarely used).
  It is written as a check: what to run, against what, and what result shows
  it holds. On PR 2888 these would have been:
  - "A sample of lookups that succeed on main still succeed, by SKU and by
    MPN, against the live API."
  - "For a manufacturer with several DigiKey ids, a lookup that 404s scoped
    to the wrong id still finds the part."
  - "The correction script's queries use an index on production (EXPLAIN)."
- **Every obligation is live by default.** Only design may mark one
  **code-only** (a pure function, a refactor, a rename), with the reason.
  Planning cannot (deliberately: it builds the plan and should not decide how
  much proof the plan needs).
- Planning gives each `verifying` obligation a `verifying` plan step, and
  lines up what it needs: credentials, access, test data, quota (section 3).

**2. The verdict says how it was shown.**

- `tod-cli verdicts record` gains `--method live|local-data|static|unit` and
  `--ran <what was run>` (`tod_store::verification`).
- The `obligations-verified` gate (`tod_core::gate::derived`) refuses a
  `verified` verdict on a live obligation whose method is only `unit` or
  `static`.
- What "live" means:
  - **External APIs:** the real API (`tod-cli secrets run`).
  - **Reads of production data:** production, read-only (`tod-cli db`,
    `tod-cli access`).
  - **Writes:** a local database seeded with crafted data covering each case,
    plus a read-only dry run on production.

**3. Credentials are the user's action, never a reason to skip.**

- A credential or access the verifier lacks is a request
  (`decisions ask --reason access`) that blocks the verdict.
- Moving a check to `released` because access is missing is not allowed.
  `released` is only for what truly needs the release.
- Planning should have asked already, so this is the fallback.

**4. An independent evidence evaluator.**

- When the verifier has recorded every verdict, a fresh session that did not
  verify (`ProtocolKind::Evaluate`, as for the phase agents) audits the
  evidence before the gate can pass.
  - For each obligation: does the method fit, did what ran really exercise
    the claim, does the output show it?
  - Is the regression replay there and complete?
- It either accepts or reopens the verdicts it rejects, with what is missing.
  A second rejection of unchanged evidence stops the run for the user, as for
  phases.
- The review's evidence audit (6f) stays as the second net.

**Regression replay.** A change to how existing work is done (lookups,
matching, pricing) is replayed on a sample of real items: ones that succeed
today and ones that fail. It compares old and new results, within any quota
(DigiKey: 5,000 calls/day). An item that worked before and fails after is a
failed verdict. This alone would have caught issue 2. Design writes it as a
`verifying` obligation whenever the change alters existing behavior.

Files:
- `assets/process/agents/state/{design,planning,verifying}.md`
- `crates/tod/media/context/surface/{verify,evaluate}.md`
- `crates/tod/media/context/cli/{verdicts,obligations}.md` (the code-only
  marker)
- `tod_store::verification`, `tod_core::gate::derived`
- the verify protocol (`tod_core::conversation::protocol::verify`), to
  hand off to the evaluator

`verifying` runs at high effort.

### 6. Review: the whole change, every lens, one round

#### 6a. Remove the language that discourages findings

In `crates/tod/media/context/surface/review.md`:

- "Functional correctness should already have been established in
  `verifying`, so read the code rather than re-running the verification"
  → replaced by the evidence audit (6f).
- "hardening nobody required" → removed. A missing guard against a real
  failure is a defect.
- "every finding that is not a real defect keeps that loop turning" and "Do
  not invent one to look thorough" → replaced by: *a missed defect costs far
  more than a false one. Record what you believe is wrong; when unsure, say
  what would confirm it.*
- "review them like the rest, for the defects they introduced, **and only
  those**" → removed. This line told every round after the first to review
  only the delta, and it is the main reason most rounds were delta reviews.

In `assets/process/agents/state/review.md:10`, "Functional correctness should
already be established—do not treat this state as primary QA" becomes:
*the review does not run QA itself, but it judges whether QA was done
(6f).*

#### 6b. Every round reviews the whole change

Every review, including one after a fix, reviews the branch's whole change
against its base. Fixes get extra attention, since they are written fast and
under pressure, but they are never the only thing reviewed.

#### 6c. A lens sweep with a coverage record

The reviewer sweeps the change through a fixed list of **lenses** (below) and
records, for each lens and each changed file, what it checked and what it
found, or why the lens does not apply. That record is the review's
**coverage**:

```
tod-cli review coverage --lens data-assumptions --file digikey.py \
  --checked "org id lookup: queried prod, 113 orgs with >1 id" --result finding:<id>
```

`review done` is refused until every lens has an entry for the change
(`tod_store::review`, a `review_coverage` table keyed by review round). The
task panel shows coverage beside the findings. This makes a skipped step
visible instead of silent, and gives the next round something to check.

The lenses (tod's own, the same for every project):

1. **Obligations:** does every path make each obligation true?
2. **Data assumptions:** cardinality, uniqueness, ordering (a `LIMIT 1` or
   "first row" with no `ORDER BY`), nulls, scale. Checked on production,
   read-only.
3. **External systems:** an API's answers on every error, rate limits, quota,
   cost per call, retries.
4. **Persisted results:** caches, negative caches, TTLs, stored statuses.
   What wrong value can be stored, for how long, and how it is undone.
5. **Interactions:** each pair of new behaviors touching the same item.
6. **Reach:** every caller and path that reaches the changed behavior
   (background jobs, gap-fill, scripts), not only the one the task named.
7. **Data writes:** transactions, locks (and whether any are held across a
   network call), query plans on production-sized tables (EXPLAIN),
   idempotency, rollback.
8. **Concurrency and cancellation.**
9. **Error paths:** each new failure classified and handled.
10. **Tests:** would each test fail if the fix were reverted? Does a mock
    assume the answer under test?
11. **Verification evidence** (6f).

#### 6d. Variant analysis: one finding, its whole class

The main cause of one-finding-per-round is that a reviewer finds an instance
and stops. For every finding, the reviewer:

1. Names the **root cause**: the assumption or pattern that made it possible
   ("the org's DigiKey id is treated as unique").
2. **Searches for every other place** that assumption or pattern holds in the
   change and the code it reaches.
3. Records each as its own finding in the same round, linked by root cause
   (`review add --root-cause <text>`).

The fixer then fixes the class, not the instance (section 7).

#### 6e. A saturation pass before done

After the sweep, the reviewer makes one more pass on the premise that it
missed at least three defects, looking specifically where its findings
cluster and in the lenses it found nothing in. It records the review done
only after a pass that finds nothing new. The coverage record shows the pass.

#### 6f. Audit the verification, and send it back

The reviewer does not run QA. It reads the verdicts and their evidence
(`tod-cli verdicts list`) and judges whether each obligation was really
shown. If the evidence is insufficient (mocked where live was needed, a case
not exercised, a regression replay missing), it **sends the task back to
`verifying`**, naming the obligations and what evidence is missing (section
9). If a finding shows a design or requirement assumption is false, it sends
the task back to `design` or `proposed`.

#### 6g. Make the project's reading happen

Why reviewers skipped the reading the project's review skill asked for:

- **tod's own surface contradicted it.** The skill says to read `CLAUDE.md`,
  `REVIEW.md`, and the routed skills first. tod's surface, which comes earlier
  in the prompt, says a review after a fix looks at "the defects they
  introduced, and only those". A delta review does not need the setup reading,
  so agents skipped it.
- **Placement.** The skills' text is near the end of a ~1,340-line opening
  (line ~980). It is introduced with "Everything else in this message still
  applies", which ranks it below tod's surface.
- **Nothing checks it.** Reading leaves no record, so it is the first step
  dropped under pressure. Effort was not the cause: the opus/high rounds
  skipped it too.
- **Indirection.** The routing table makes the agent match its diff against
  25 rows and then open files. That is easy to defer and forget.

Fixes:

- The contradiction goes (6a, 6b).
- tod decides **which** of the project's guidance applies, records it on the
  node, and inlines exactly that (section 12). Pasting every routed skill
  would make an already huge opening far larger. Selecting first keeps it to
  what the change touches.
- The coverage record lists the files read for each lens
  (`review coverage --read <path>`). `review done` is refused when a selected
  file is neither inlined nor listed.
- The Skills block moves ahead of the surface's procedural sections, and its
  introduction says the project's instructions are part of the job, not an
  appendix.

#### 6h. Two reviewers per round

Thirty rounds by the same model with the same prompt make the same mistakes,
and a reviewer asked to cover everything spreads its attention thin. Each
round therefore runs two reviewers at once, in fresh sessions, recording into
the same findings list.

1. **The main review** covers the whole change, every lens, with the full
   coverage record. It answers *is this code correct?*
2. **The risk review** covers the same whole change, but only through the
   lenses where code that reads correctly is still wrong. It is wrong about
   the world outside the diff:
   - data assumptions;
   - external systems;
   - persisted results;
   - interactions;
   - reach (every caller and path);
   - data writes, when the guidance selection found any.

   It answers *what does this do to real data and real systems?* With fewer
   lenses it goes deeper on each. It queries production read-only, runs
   EXPLAIN, and traces every caller. Most of PR 2888's defects were of this
   kind: a filter that is wrong for 113 orgs, a 30-day cache in front of it,
   a background path nobody traced, a sequential scan under `FOR UPDATE`.

There is no separate third reviewer. When a change writes production data,
"data writes" is simply part of the risk review's assignment.

**Same model, shared opening, to start.** Both reviewers get a byte-identical
opening (stance, surface, node, obligations, the selected guidance, the
change). Only the last short section differs: the assignment, meaning its
lenses. They start together, so the second reads the shared part from the
cache at about a tenth of the price (section 13). The independence comes
from the different assignment and a fresh session, not from a different
model.

A different model would add a second kind of independence (different blind
spots) but loses the cache, since the cache is per model. Which is worth more
is not known. So it starts on the same model, and on a sample of PRs the risk
review runs on a second model (Sonnet 5.5 at high) for comparison. The miss
accounting (section 11) records which reviewer made each finding and what
each session cost. A model change is made when the numbers show unique
findings worth the price.

A reviewer whose lenses produce nothing the other did not, over about 20 PRs,
has its assignment changed.

Review effort goes back to high. It was lowered to medium on 11-19, just
before the worst round.

#### Lenses versus routing skills

These are separate and complementary:

- The **lenses** are tod's: *what kinds of failure every review must sweep*,
  the same for every project. They make the review comprehensive.
- The **routing skills** are the project's: *what this codebase's answers and
  traps are* in a given area (`db-lock-ordering` for writes, `sql-patterns`
  for queries, `new-integration` for an API client). They make each lens deep.

A lens says "check lock behavior". The routed skill says how locks must be
taken in this repository. The coverage record ties them: each lens entry
lists the project files read for it. A project with no routing skills still
gets the full lens sweep.

### 7. Fix: fix the class, escalate a redesign

In `crates/tod/media/context/surface/fix.md`:

- **Fix the root cause and every variant** the finding names, not the line it
  points at. If the fix reveals more instances, fix them and say so.
- **Escalate instead of redesigning.** A fix that would add a mechanism no
  obligation or plan step describes, or that a finding shows is needed because
  an obligation is false, is a design change. The fixer sends the task back to
  `design` with the reason (section 9) and makes no patch. On PR 2888 the
  coworker's comment invalidated "use the org's DigiKey id". The right move
  was back to design, not ten fix commits.
- **The app backs this up:** when the findings in one stay share a root cause
  across three rounds, or a fix round adds more findings than it closes twice
  in a row, the autopilot sends the task back to `design` itself, with the
  findings as the reason (`tod_core::autopilot`).
- **Never push** (section 8).

### 8. Nothing reaches the PR unreviewed

- `crates/tod/media/context/workspace/codebase.md:12–15` says "Make every
  change, commit and push right there". It is appended to every agent,
  including the fix agent in `pr_draft`, which must not push before the
  review is clean. It becomes: *commit; the app pushes when the work has
  passed its review and checks.* Any role that pushes itself (the PR agent)
  says so in its own surface.
- **Enforced, not only asked for.** Before and after each fix turn in
  `pr_draft`, the autopilot compares the remote branch head. A push it did
  not make is recorded on the node and in the Activity as a process failure
  (for the miss accounting), and the review runs on that head at once. There
  is no `pre-push` hook: the process itself should make one unnecessary, and a
  push detected here is a defect in the process to fix.
- **A push asked for in a conversation goes the same way.** When the user asks
  a chat to change a branch that has a PR (058b0148 on PR 2888), the
  conversation commits. The app then runs the review and checks before it
  pushes, and says so. The user can still say "push now", which pushes and
  records that the head is unreviewed.
- **Ready only when the gate would pass.** tod never marks a PR ready while
  its own review of the head or its checks are not clean. If someone else
  marks it ready, the task panel shows a warning with what is outstanding.
- **The reviewer criterion counts people only.** `has_reviewer`
  (`crates/tod-core/src/gate/derived.rs:690`) counts any non-bot reviewer,
  including the account tod posts as, so tod's own review satisfied "A person
  assigned to review it?". The criterion excludes tod's own account (the
  `gh` identity the app uses).

### 9. Sending work back, by any agent

Today only a person can send a task back: an agent offers an option with
`--moves-to`, and the user picks it (`tod_core::lifecycle::send_back`). That
makes every send-back wait on the user, so agents patch instead. The user's
direction: empower the agents to kick work back up the process, as far as
the cause, up to `proposed`.

**Design:**

- A new command `tod-cli lifecycle send-back --to <state> --reason <text>
  [--evidence <kind>:<id>] [--obligation <id>]...` is allowed for phase
  agents, evaluators, verify, review and fix sessions. It calls the same
  `send_back` function with the conversation as actor. Reopening is unchanged:
  steps and verdicts reopen at `active` or earlier. A send-back to
  `verifying` reopens only the verdicts of the named obligations.
- The target phase's opening carries the reason, the evidence and who sent it
  (the phase status block), so it starts from the cause, not from scratch.
- **Where to:** the phase that made the false assumption.
  - Evidence insufficient → `verifying`.
  - The build is wrong but the design holds → `active`.
  - The approach is wrong → `design`.
  - A requirement rests on a false claim → `proposed`.
- **Requirements stay the user's on intent.** Back in `proposed` the agent
  corrects a requirement's facts with the evidence (a recorded change,
  reversible). A change to what the task is *for* is still a decision for the
  user, with the edit attached (`--edit-obligation`).
- **Guard rails:**
  - A second send-back to the same state for the same reason within one stay
    stops the run for the user (`NeedsHuman::SendBackLoop`).
  - Every send-back is shown in the node's Activity and recorded in the
    journey (`UserAction`-like record with the agent as actor).
  - An open PR stays open. The `pr` agent reviews it again on re-entry, as it
    already does after a move back.

Files: `crates/tod-core/src/lifecycle/send_back.rs`, the `lifecycle` noun in
`crates/tod-cli`, `crates/tod/media/context/cli/lifecycle.md`, each surface
that may use it (`phase.md`, `evaluate.md`, `verify.md`, `review.md`,
`review-on-pr.md`, `fix.md`), and `tod_core::autopilot` for the stop rule.

### 10. Configuration

In the lifecycle config on `all-tasks` (the user's data; changed through
`tod-cli capabilities set … lifecycle-config` once approved):

- `active`, `verifying`: high effort (today: default).
- `review`, `fix`: back to high (today: medium).
- `proposed`, `planning`: high, since they now investigate.

### 11. Learn: account for every miss

- The work history the `learn` agent gets (`node_context::render_work_history`)
  gains the findings made outside tod (PR review threads and bot reviews,
  `tod_core::pr_readiness`). For each one it records which phase should have
  caught it, and why that phase missed it.
- A finding in a later review round on code that an earlier round already
  covered is counted as a miss by that earlier round.
- The counts per PR (outside findings missed, findings per review round,
  rounds to clean) feed the goal's measure. A rise in any one is a signal to
  look at that phase's prompt.

### 12. Selecting the guidance that applies

A project's guidance is large: PR 2888's workspace routes to 25 rows of
skills, plus `REVIEW.md`, each submodule's `CLAUDE.md`, and the per-phase
checks. Pasting all of it into every review would make the opening larger
still. Leaving the choice to the reviewer is what failed. So tod selects the
guidance once, records it on the node, and every later phase gets exactly that
list, inlined.

**What gets selected:** the routed skills, the always-read files (`REVIEW.md`,
the `CLAUDE.md` of each submodule touched), the per-phase checks that apply,
and the lenses that apply (section 6c; for a change with no data writes, that
lens is recorded as not applying, with the reason).

**When, and from what.** The selection follows the work as it becomes known:

1. **End of design: predicted.** The design names the areas the change will
   touch ("a KB integration client", "raw SQL with row locks on a large KB
   table", "a KB data fix"). Selection runs against those. The same list goes
   to `active` and `verifying`, so the implementer builds with the guidance
   the reviewer will judge by. The guidance is there to build it right, not
   only to review it.
2. **Entering review: actual.** Selection runs again against the real change
   (its files and diff summary). Anything the design did not predict is added.
   A guidance file the change no longer needs is dropped. The difference is
   worth recording: an area design did not foresee is a sign design missed
   something.
3. **After a fix: only if the change grew.** Selection runs again only when
   the change's files or areas grew beyond what was selected for.

**Who selects: a session of its own, never the reviewer.** Selection is a
separate step the autopilot runs **before** the phase that uses it: after
design is certified (before `active`), and before each review round. Its only
job is to pick. It reads no skill, reviews nothing, and builds nothing. Its
output is recorded before the next session exists, and the review's opening is
built from it, so the reviewer has no choice to skip. A reviewer asked to pick
its own reading at the start of its session would be the same failure again.

- **The app first.** Where a skill has machine-readable triggers in its front
  matter (`applies_to: { paths: [...], terms: [...] }`), the app matches them
  against the change. That needs no model and is free.
- **Haiku for the rest.** The prose rows of the routing table ("raw SQL, row
  locks, or batch processing over large KB tables") go to a one-turn session
  on **Haiku**. It gets:
  - the routing table;
  - the list of always-read candidates (`REVIEW.md`, each touched
    submodule's `CLAUDE.md`);
  - the lens list;
  - a change summary the app builds itself, with no model: the files changed,
    `git diff --stat`, the functions and SQL statements touched, and, before
    `active`, the design's named areas instead.

  It records each match with a one-line reason (`tod-cli guidance select`) and
  each lens that does not apply, with the reason. This is matching, not
  judgment, which is what Haiku is for, and it costs a fraction of a cent.
- **Recorded on the node** (`tod_store`, a `node_guidance` table: item, kind,
  reason, stage, which change it was made against). It is shown in the task
  panel, and the user can add or remove an item, like the lifecycle config.
- **A miss is visible.** If a reviewer later finds a defect in an area that a
  skill covers but the selector did not pick, the miss accounting records it
  against the selector, not the reviewer.

**The budget.** Selection is the main trimming: only what the change touches
goes in. Every reviewer in a round gets the **same** selected guidance, so
their openings stay identical and share the cache (section 13). It is split
between reviewers only if it is still too large for one session.

**What it replaces.** The reviewer no longer matches its diff against the
routing table at all. The selection is visible before the review starts, so a
wrong or missing pick can be seen and corrected rather than silently skipped.

### 13. Cost: much smarter, not much heavier

This plan adds steps, and a process too expensive to run will not be run. The
rules:

- **Judgment on the strong model; mechanics on Haiku; arithmetic in the app.**
  - Design, implementation, verification, review and fixes: the strong model.
  - Guidance selection, and any later step whose job is matching or
    classifying: Haiku.
  - What needs no model at all: the app. That covers change summaries,
    trigger matching, coverage checks, push detection, and the gate.
- **Fewer rounds pay for deeper ones.** PR 2888 took 30 reviews and 18 fixes,
  most of them finding one thing. A review that sweeps every lens and runs a
  saturation pass costs more per round. If it ends the loop in two or three
  rounds instead of thirty, it costs far less overall. The miss accounting
  records rounds and cost per node so this can be checked rather than
  assumed.
- **Smaller openings.** Selection drops guidance a change does not need.
  `cli/` nouns are already situational. Every turn of a session re-sends the
  whole context, so each line cut from an opening is saved on every turn,
  not once.
- **Use the cache.** Cached input costs about a tenth of fresh input. A prefix
  is cached only while it is byte-identical, on the same model, within the
  cache's lifetime.
  - **Stable first, volatile last.** Every opening is ordered from what never
    changes to what changes each round:
    1. stance, domain, `cli/`, surface: identical for every session of a kind;
    2. the role doc;
    3. the node, its obligations and ancestors, and the selected guidance:
       identical for every session on the node until they change;
    4. last, what changes each round: the PR, the verdicts, the findings, the
       change.

    Today `REVIEW_SESSION`'s blocks (`tod_core::context_recipes`) put the PR
    and the verdicts *before* the skills, so every round pays full price for
    the skills.
  - **Reviewers in a round share one opening.** Both reviewers get the same
    opening and differ only in a short final section (their assignment), and
    they start at the same moment. The second one reads the shared part from
    the cache. That only works on the same model (6h).
  - **Check what the agent's CLI puts ahead of us.** Claude Code adds its own
    context (working directory, git status, date) before our opening. If
    that changes between sessions, it defeats the cache for everything after
    it. Measure with the cache-read token counts per session before relying
    on this.
- **Measure it.** Record input, cached input and output tokens per session,
  with the phase and the model (if not recorded already). Cost per node sits
  beside the miss count, so the improvement process can trade one against
  the other.

## Work items

In order. Items 1–4 are prompt changes and stop most of the damage at once;
5–9 are app changes that enforce them.

**Items 1–4 are done (2026-11-19).** Until the app changes land, the prompts
use what exists:

- **Send-back** (item 5 makes it a command of its own): an agent asks a
  decision whose option carries the move.
- **Root cause** (item 6 adds `--root-cause`): a finding's detail starts with
  `Root cause:`, in the same words for every finding that shares it.
- **Coverage** (item 6): the lens sweep is asked for but not yet recorded.
- **Fix surface** (from item 9): `surface/fix.md` already says to fix the
  class and to send a redesign back to `design`. The autopilot's own
  escalation is still item 9.
- **Pushing:** an agent pushes only until the task's branch has a pull
  request. That keeps work in a cloud sandbox from being lost. After that,
  only the pull request agent pushes.

**Items 7 and 15 are built (2026-11-19).** The store is `tod_store::guidance`
(schema v104). The app's trigger matching and the selector's material are in
`tod_core::guidance`, and the selector is the `guidance` protocol
(`conversation/guidance.rs`, haiku at low effort unless the lifecycle config
key `guidance` sets them). The CLI is `tod-cli guidance`. The task panel has a
Guidance section with Remove and Add. Some of it differs from the plan:

- The predicted selection runs lazily, before the first work session after
  design is certified, not as a step on the way into `active`.
- The selection goes into the Skills block (`skills_context::for_work`),
  which keeps the recipes' cache order. It does not have a block of its own.
- The selector loops only while a lens is not recorded either way.
- A `pr_draft` review owes an actual selection once per stay, as `review`
  does.
- Selector sessions count toward the run's session budget.
- A selected file that is missing at inlining time fails the session as an
  agent failure. No supervisor-specific handling was added.

| # | Item | Files | Done when |
|---|---|---|---|
| 1 | Review prompts: remove the discouraging and delta-only text; whole change every round; lenses; variant analysis; saturation pass; evidence audit | `surface/review.md`, `surface/review-on-pr.md`, `state/review.md` | The surface asks for all of 6a–6f; context tests pass |
| 2 | Push rule | `workspace/codebase.md`, `surface/fix.md`, `state/pr_draft.md`, `surface/pr.md` | No surface but the PR agent's tells an agent to push |
| 3 | Proposed and design challenge assumptions; planning lines up verification access | `state/proposed.md`, `state/design.md`, `state/planning.md`, `surface/phase.md`, `surface/evaluate.md` | Done-when items in place; evaluator requires evidence |
| 4 | Verifying is live; remediation-script rule; read-what-you-reuse | `state/verifying.md`, `surface/verify.md`, `state/active.md`, `surface/implement.md` | Text in place |
| 5 | Agent send-back | `lifecycle/send_back.rs`, `tod-cli` lifecycle noun, `cli/lifecycle.md`, surfaces, autopilot stop rule | An agent can send a task back with a reason; the loop guard stops a second identical one; tests |
| 6 | Review coverage record and `review done` check | `tod_store::review`, `tod-cli review`, `cli/review.md`, task panel | `review done` refused without coverage for every lens; shown in the panel |
| 7 | Guidance selection (section 12): predicted at design, actual at review, re-run when the change grows; inlined per phase and per lens group; Skills block placement | `tod_store` (`node_guidance`), selector step in `tod_core::autopilot`, `tod_core::skills_context`, `context_recipes`, task panel | The selection is on the node with reasons; each phase's opening holds exactly it; coverage checks it |
| 8 | Verifying rework (section 5): live checks as `verifying` obligations from design; code-only marker (design only); verdict method; independent evidence evaluator | `state/{design,planning,verifying}.md`, `surface/{verify,evaluate}.md`, `tod_store::verification`, `tod-cli verdicts`/`obligations`, `gate::derived`, verify protocol | A unit-only `verified` on a live obligation fails the gate; the evaluator audits every verdict before the gate passes |
| 9 | Fix escalation backed by the autopilot | `surface/fix.md`, `tod_core::autopilot` | Repeated root cause or a growing finding count sends the task to design |
| 10 | Unreviewed-push detection; conversation pushes through review; ready-only-when-clean warning | `autopilot`, conversation driver, task panel | Each case shows on the node; a pushed head is reviewed at once |
| 11 | `has_reviewer` excludes tod's account | `gate/derived.rs` | Test with tod's login as the only reviewer fails the criterion |
| 12 | Two reviewers per round (main and risk) with one shared opening and a per-reviewer assignment; effort settings | autopilot (`review` step), `context_recipes`, `surface/review.md`, lifecycle config (user's data) | Both run in parallel; each finding records which reviewer made it |
| 13 | Learn accounts for outside findings | `node_context`, `state/learn.md` | The learn phase records each miss and the phase that owned it |
| 14 | Find why the runner did nothing on PR 2888 from 16:08 to 18:37 | daemon logs, autopilot | Cause known and fixed or explained |
| 15 | Guidance selector: app trigger matching, then a one-turn Haiku session before `active` and before each review round | `tod_core::autopilot`, a `guidance` protocol, `tod-cli guidance`, `tod_store` | Selection recorded with reasons before the next session starts |
| 16 | Cost: stable-first ordering of every recipe's blocks; token usage per session recorded; check the agent CLI's own changing context | `tod_core::context_recipes`, `tod_store` (session usage), measurement | Cache-read share per session measured before and after |

## Decided

- **No `pre-push` hook.** The process should make it unnecessary. A push the
  autopilot did not make is detected and treated as a process defect.
- **Only design may mark an obligation code-only.** Planning might lean toward
  using it to make its plan easier to finish.
- **Two reviewers per round** (main and risk, 6h), on the same model with a
  shared opening to start. A second model is tried on a sample, and adjusted
  by what the miss accounting shows.
- **PR 2888** was moved back and stopped by the user. It waits until this
  plan is implemented.

## Where the improvement process will plug in

A separate process will improve this one from what the miss accounting
shows. It is not part of this plan, but these are the places it is expected
to change, so each is kept as data or a single file rather than spread
through code:

- **The lens list** (6c): one list, read by the review surface and the
  coverage check.
- **Each phase's Done when** checklist (`assets/process/agents/state/*.md`).
- **The guidance selection** (section 12): the selector's rules and a
  project's `applies_to` triggers.
- **The reviewers and their models** (6h): lifecycle config.
- **The thresholds:** fix-loop escalation (section 7), send-back loop guard
  (section 9), evaluator rejections (section 5).
- **Its input:** the miss records the `learn` phase writes (section 11).

## Open questions

- None open. New ones go here as implementation finds them.
