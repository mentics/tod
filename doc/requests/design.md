# Requests for the user: fewer, cheaper, clearer, and self-improving

Status: steps 1 to 8 built (lessons and process review: `doc/lifecycle/self-improvement.md`), and the self-improvement view (11.4, `doc/ui/self-improvement.md`). 2026-11-20.

This replaces how a node asks the user for something today: an agent runs
`tod-cli decisions ask`, the run stops, and the request sits in the task panel
in whatever words the agent chose. Three problems drive the change:

1. **Too many requests.** Most could have been settled by the app, a cheap
   agent, or the asking agent itself.
2. **Too hard to answer.** The user often has to start another agent session
   just to understand the question.
3. **Nothing learns.** The same kinds of question come back, and the existing
   feedback control ("Shouldn't have asked") has never been used: it was not
   clear what clicking it would do.

Cost runs through all of it. With many nodes running at once, agent sessions
are the expensive part, so every step below settles what it can without one,
or with the cheapest one that can do the job.

## 1. Principles

- **The cheapest rung that can settle it.** Mechanical (no model) first, then
  a Haiku session, then a full agent, then triage, then the user. Each rung
  hands up only what it could not settle, along with what it tried.
- **Every climb is a lesson.** A request that reaches triage, a send-back to
  an earlier phase, and a quick fix that had to escalate all mean something
  upstream could have been better. Each one goes into the self-improvement log
  (section 11). The aim is that nothing reaches triage at all.
- **One cause, one request, across the fleet.** Fifty nodes failing the same
  way get one incident and at most one session or one request, never fifty.
- **No agent rewrites what is mechanical.** Access and breakage requests are
  rendered by the app from structured fields. Spending a model on rewording a
  fact is waste.
- **Contexts stay small.** Nothing here adds the ticket or long rules to every
  session. Guidance about wording requests moves *out* of every session's
  context into the one session that writes briefs (triage).
- **Agents write obligations.** That is intended and stays. What changes is
  which agent may change which obligation (section 10).

## 2. Where requests come from today

Live data root, last 14 days, about 190 requests:

| Source (protocol, reason) | Count | What they were |
|---|---|---|
| `check_unrelated`, broken | 34 | "Decide whether the failing check X on PR #N is fixed in that PR or carried on without." Mostly cancelled runs (workflow concurrency), timeouts, or errors also on main |
| phase, access | 34 | Prod actions only the user can take (run a Dagster job, deploy to QA): legitimate, but hard to read |
| fix / phase / implementation / pr, broken | 49 | Hook blocked a call, TLS name wrong, lost branch; several filed twice |
| fix / phase / verification, conflict | 19 | Includes "Apply the two obligation wording changes you already approved … because the app refuses my edits" |
| phase, other / intent | ~12 | Includes four "Decide whether to open a follow-up ticket for …" (one answered "There's already a ticket: ENG-3557") |
| `bot_silence`, broken | 4 | greptile silent on infra#551, raised twice |

Other facts:

- Agents created 996 obligations in conversations and the user created 11.
  That is as intended.
- `request_feedback` has 0 rows, ever.

## 3. The escalation ladder

```
failure or question
  │
  ├─ 0. Mechanical      classify, retry, rerun, run a configured fixer,
  │                      match an open incident, refuse a duplicate
  ├─ 1. Quick fix        Haiku, one failing thing, few turns, rerun only it
  ├─ 2. Full agent       the lifecycle agent / Fix protocol, as today
  ├─ 3. Triage           a fresh session: answer it, fix the cause, or brief the user
  └─ 4. User             a brief they can answer cold, with a chat behind it
```

Not every item goes through every rung. Routing is by what it is:

| What happened | Path |
|---|---|
| A check fails (local or CI) | 0 → 1 → 2 (section 4.2) |
| The same mechanical failure hits several nodes | 0 (incident) → one investigation or one user request (section 4.3) |
| An agent needs access only the user has | 0 (incident / duplicate match) → 4, app-rendered (section 7.1) |
| An agent finds tod or the environment broken | 0 → 4, app-rendered (section 7.1) |
| An agent needs a judgment (intent, conflict, risk, missing rule, other) | 3 → maybe 4 (section 8) |
| An agent thinks an obligation is wrong | send-back to the phase that wrote it (section 10) |
| Something out of scope is worth doing | a non-blocking suggestion, never a request (section 9) |

## 4. Mechanical layer (rung 0)

### 4.1 Failure classification and signatures

Every failure the app sees gets a **signature**: its kind plus a normalized
key, with ids, hashes, numbers, timestamps and node-specific paths stripped
(`tod_store::github::failure_signature` already does this for checks).
Kinds:

| Kind | Examples | Default handling |
|---|---|---|
| `ci_infra` | run cancelled by concurrency, job timed out, runner lost, GitHub 5xx | rerun the job, bounded |
| `auth` | GitHub 401, AWS token expired, Blaxel auth rejected | incident; never per-node agent work |
| `service_down` | connection refused, DNS failure, 5xx from a named host | retry with backoff; incident |
| `provider` | model API overloaded, rate limit | retry with backoff; incident |
| `tool_broken` | `tod-cli` or a tod shim failing with the same message | incident; tod bug lesson |
| `work` | everything else: the change's own failure | climb the ladder (4.2) |

### 4.2 Failing checks

This applies to the checks tod runs (`doc/lifecycle/checks.md`) and to a pull
request's CI checks (`doc/lifecycle/pr-readiness.md`). In order:

1. **Infrastructure.** For a cancelled, timed-out or runner-lost CI job, tod
   reruns the failed jobs through GitHub, up to a limit (default 2 per head).
   No agent turn. A job that passes on rerun is recorded as a flake for that
   check, for later stats.
2. **Pre-existing.** tod compares with the base branch head (or the latest run
   on the base branch). If the same check fails there with the same failure
   signature, the change did not cause it. tod waives the check for this pull
   request, notes that in the node's Activity, and files a suggestion
   (section 9): "Open a ticket: check X fails on main". No request goes to the
   user. This replaces the agent's `pr checks unrelated` claim going to the
   user: an agent may still claim it, but the claim is checked this way, and
   only a claim the comparison cannot confirm goes on (to rung 1/2 as work, or
   to triage if the agent insists).
3. **A configured fixer.** A check may name a **fix command** (a new
   `fix` field on `lifecycle_config::Check`, and the same for a CI check
   in `pr_readiness`). Examples: `cargo fmt`, `ruff format`, `ruff check --fix`,
   `prettier --write`. tod runs it, commits the result as its own mechanical
   change, and **reruns only that check**. A repository profile can supply
   defaults for common formatters.
4. Otherwise it is `work`, and goes to a quick fix (section 5).

**What a mechanical or quick fix does not trigger.** Today a check failure
goes to the Fix protocol, and the node then goes round Review again. That is
wrong for a formatting fix:

- A commit produced only by a configured fix command is exempt from review.
  It is the tool's own output, recorded as such (a new `pr_pushes` kind for a
  mechanical change).
- A quick fix never starts a new review round by itself. Its change is covered
  by the next round that happens anyway. In `pr_draft`, where nothing reaches
  the pull request unreviewed, it gets a **small-change review**: one
  reviewer, a cheap model, the diff only. It does not get the two-reviewer
  round.

### 4.3 Cross-node incidents

The goal is that 50 nodes failing the same way cost one session, not 50.

- **Store.** A machine-local `incidents` table (signature, kind, first seen,
  status `open` / `investigating` / `resolved`, sample errors) and
  `incident_nodes` (which nodes are parked on it, and the operation each was
  doing). Both are journaled.
- **Matching.** When a node fails with a non-`work` signature:
  - if an open incident has that signature, the node **parks** on it: no
    retry, no session, no request;
  - if not, the node retries with backoff, and a second node with the same
    signature within 30 minutes opens an incident covering both.
- **Who is next.** A parked node is `Others`, with "who" naming the incident
  ("GitHub auth failing, 12 nodes") and its retry count (`doc/lifecycle/next-actor.md`).
- **Probe.** At the backoff interval, the incident reruns one parked node's
  failed operation. When it succeeds, the incident is resolved and the parked
  nodes resume, staggered so they do not all hit the service at once.
- **Escalation, once per incident:**
  - `auth` or access: **one** user request, app-rendered. It says what is
    failing, the exact fix where tod knows it ("Run `gh auth login`"), and the
    nodes affected.
  - anything else: **one** Haiku investigation session (protocol `incident`)
    with the samples. It fixes what it can, else files one user request.
- **Agent-filed breakage joins incidents too.** A `decisions ask --reason
  broken` carries the failing command and error as structured fields (7.1).
  The app computes their signature and, on a match, attaches the node to the
  open incident instead of filing a request.
- **Status bar.** Open incidents are shown fleet-wide, not only on each node.
- **Cloud nodes.** Supervisors already forward problems through the outbox
  (`doc/problems.md`). They also send failure signatures, so the daemon groups
  cloud and local nodes together. Built: the supervisor reports through the
  problem channel, the daemon ingests, parks and holds the node in its sandbox
  with an `incident:<id>` wait, and a release wakes it
  (`doc/lifecycle/incidents.md`, "Cloud nodes").

### 4.4 Review bot silence

This is fully mechanical: no agent, and no request by default. Once a bot has
been asked as many times as allowed (`max_asks`) and is still silent, the node
proceeds without it. tod records this as an answer by tod in the node's
Answers (section 8.4), with what was tried. When the same bot is silent on
several pull requests at once, that is an incident (4.3): one notice in the
status bar, not one per pull request. A project can set `on_silence: ask` per
bot to get today's request back.

### 4.5 Duplicates

A request whose cause is already pending or recently answered is refused, and
the refusal hands back the existing request's id, or its answer. This applies:

- across all of a node's sessions, not only within one conversation;
- by normalized question text, by the same `--blocks` steps, or by the same
  failure signature;
- to answered requests on the node at any age, not only the last 30 minutes.
  `--new-evidence` stays the way to ask again when something really changed.

## 5. Quick fix (rung 1)

A new protocol, `quick_fix`. Haiku at low effort by default, set by the
lifecycle config key `quick_fix`.

- **Gets:** one failing thing (a check's name, command and output tail), the
  part of the diff that touches the files in the output, and the repository's
  usual commands. Nothing else: no obligations, no plan, no design.
- **May:** edit files in the node's checkout and run that one check's command.
  It may not touch obligations, the plan, or decisions.
- **Limit:** 3 turns by default. Success means the check passes when tod
  reruns it.
- **Escalates** to the full Fix protocol (rung 2) on failure, passing its notes
  along so the Fix session does not start from nothing. Each escalation is
  recorded, so how often each check's quick fixes fail can be measured. A
  check whose quick fixes usually fail can be set to skip rung 1.

### 5.1 Review findings are scored, and routed by their scores

Failing checks climb the ladder above, but a reviewer's findings did not:
every finding went to the full Fix session, and every fix to another
two-reviewer round. ENG-3557 ran 20 rounds and hit its 60-session budget on
findings such as "a six-line docstring for a three-line function" and a
stale comment. Those were nits a reviewer judged against a project rule,
which no linter could have caught.

**Three scores per finding**, recorded by the reviewer with each finding
(`tod-cli review`), and shown and editable in the findings panel:

| Score | Values | Meaning |
|---|---|---|
| `importance` | `blocker`, `major`, `minor`, `nit` | How much it matters: `blocker` is wrong or breaks an obligation or production; `major` should be fixed before merge; `minor` is worth fixing and fine to leave; `nit` is style, wording or taste |
| `impact` | `high`, `medium`, `low` | How much harm if it is left: its reach into users, data, production, other code |
| `complexity` | `mechanical`, `simple`, `involved` | What the fix takes: `mechanical` is a tool or a one-line rule (formatting, length, a rename, a stale comment); `simple` is a local edit with no design; `involved` is design, several files, or tests |

They replace `severity` (migrated: `high` → `blocker`, `medium` → `major`,
`low` → `minor`; impact from the same, complexity `simple`). A project rule
broken without harm is at most `minor`; a matter of taste is a `nit`.

**What blocks.** The review gate (`review::review_passed`), the PR's
`own-review`, the round limit and `fix_escalation` count only `blocker` and
`major` findings, plus `minor` ones in the first `review.minor_rounds` rounds
of a stay (default 2). Anything else still open when the review is otherwise
clean is **left**: a new finding status, `left`, with tod's reason ("nit",
"minor after round 2"), shown in the findings panel and the Activity. The
user can reopen one, which makes it block like a `major`.

**Who fixes.** A round's fixes are split by complexity:

- `mechanical` and `simple` findings that will be fixed go together to **one
  quick fix** (section 5, Haiku), which gets just those findings and the
  lines they name. Its commit gets the small-change review, never a full
  round of its own.
- `involved` findings go to the Fix protocol as today; a Fix commit is what
  starts the next full round.
- A `nit` is fixed only when it is `mechanical`, and only by the quick fix.
  Otherwise it is left at once.

A quick fix that fails passes its findings on to the Fix protocol, as in
section 5, and records a `ladder` lesson.

**Turning recurring findings into checks.** A `mechanical` finding that
cites the same rule on three findings (across nodes) files a suggestion
(section 9) to add a check, with a fix command where one exists, so the next
one is caught at rung 0 before any reviewer sees it. Reviewers are told that
what a configured check covers is not theirs to report.

## 6. Model tiers

The default for every job is the cheapest tier that does it well. Each is
configurable through the lifecycle config (`resolve_agent`).

| Job | Default |
|---|---|
| Classify, retry, rerun, fixer command, incident match, duplicate check, bot silence | no model |
| Quick fix, incident investigation, guidance selection (already Haiku), suggestion duplicate search when not mechanical | Haiku, low effort |
| Triage | Sonnet, low effort (open question 1) |
| Small-change review in `pr_draft` | Haiku or Sonnet, low effort |
| Phase agents, Implement, Verify, Review, Fix | as configured today |

## 7. Requests an agent files

### 7.1 Access, breakage, capability: straight to the user, app-rendered

When an agent reports that it lacks access, it has already checked its skills
and the `access` services and found it cannot continue. No other agent can do
better, so these skip triage. The agent fills in **structured fields**, and
the app renders the request from a template. No model rewrites it.

`decisions ask --reason access|broken|capability` gains:

| Field | Meaning |
|---|---|
| `--need` | What is needed, in one line ("Read access to the prod-kb Redis", "Run the Dagster job `repoint_dangling_class_instance_of_job` on prod") |
| `--command` | The exact command to paste, every value filled in, when there is one |
| `--error` | The exact error, for `broken` (it feeds the incident signature, 4.3) |
| `--tried` | What the agent tried, one line per attempt (repeatable) |
| `--after` | What the agent will do or check once it is done |

The rendered request reads: the need on the first line; the command in a copy
block; the error; what was tried; what happens after. Options default to
"Done, continue" and "Do not do it". The template lives in one place
(`tod_core::request_brief`), so the self-improvement loop has one file to
improve.

Before filing, these go through rung 0: an open incident or a duplicate
absorbs them.

### 7.2 Judgment questions go to triage

Reasons `intent`, `missing_rule`, `conflict`, `risk` and `other` go to
triage (section 8). The asking agent states:

- the question;
- the facts it has;
- the options it sees, its recommendation, and why;
- evidence links.

It does not need to write a polished brief, because triage writes that. So
"Wording a request" moves out of the `autonomous-session` stance, which every
autonomous session carries, and into triage's surface. That makes every
autonomous session's context smaller.

### 7.3 Out-of-scope work is a suggestion

"Should we open a follow-up ticket for …?" is never a decision: it is a
suggestion (section 9). If one arrives as a decision anyway, triage converts
it.

## 8. Triage

### 8.1 What it is

A new protocol, `triage`: a fresh session per request (or per group of
requests with one cause), separate from the lifecycle agent. The lifecycle
agent stays aimed at the work. Triage is aimed at the user, and reads the
question cold the way the user would, which also tests whether the question
stands on its own.

A decision filed for triage gets a new status, `triage`, next to `pending`
(`pending` keeps meaning "waiting on the user"). While a request is in
triage, the node is `Working`, not `You`.

### 8.2 What it gets

Kept small:

- the request (question, facts, options, recommendation, evidence, reason,
  blocked steps);
- the last few turns of the asking session, bounded;
- the node's title, state, obligations with their authority (section 10), and
  a design summary;
- the node's answered requests, so it never asks twice;
- read-only access to the node's files and the PR, and the read verbs of
  `tod-cli`.

It does not get the ticket.

### 8.3 How it ends

1. **Answers it itself.** The information was there, or a check settles it.
   It records the answer with a rationale.
2. **Fixes the cause, then answers.** Example: it reruns something, or finds
   the obligation that was misread.
3. **Converts it.** A follow-up question becomes a suggestion; an obligation
   dispute becomes a send-back (section 10).
4. **Escalates to the user.** It writes the brief (decision first, then only
   the context needed to answer, then short options with its recommendation
   first) and the request becomes `pending`. Triage then becomes the user's
   chat partner for that request (8.5).

Triage may not answer:

- an obligation change that needs the user's approval (section 10.2);
- a question whose options include moving the node, beyond what send-back
  allows an agent to do.

In both cases it escalates.

### 8.4 Answers

Every answer is recorded the same way, whoever made it. It appears in the
task panel's Answers, just as if the user had answered. `decision_answers`
gains:

| Column | Meaning |
|---|---|
| `decided_by` | `user` (picked an option, or settled it in the chat), `triage` (triage answered without the user), or `tod` (mechanical, as in 4.4) |
| `conversation_id` | The triage session whose transcript is the evidence for the answer |
| `rationale` | For `triage` and `tod`: how it was decided, in a line or two |

Triage writes the answer back to the asking agent; the user never has to
retype it. The link to the session means "why was this decided?" can always
be answered by opening the transcript.

The Answers drawer shows who decided and how, with the session link. **Change**
works as today: a new answer is sent to the runner as new input. When the node
has moved past the phase that asked, it also offers to send the node back
there.

### 8.5 The request card and the chat

An escalated request shows the brief, its options, and a **Discuss** button.
Discuss opens the triage session in the chat drawer.

- Picking an option answers it directly. No turn is spent on triage, except
  the short closing reflection (8.7).
- In the chat, the user can ask anything ("what does this mean?", "show me the
  code"). Triage can read the code and the PR. When it is settled, triage
  records the answer (`decided_by = user`) and the lifecycle agent continues.
- The lifecycle agent never takes part in the chat. It only gets the final
  answer as its next turn, as today.
- The number keys answering the top request are kept only if that stays
  simple. They are not a constraint on this design.

**Chat drawer.** The drawer's sessions list includes the focus node's triage
sessions, marked as request sessions by an icon (and a "Request:" prefix where
icons are not shown). Each has a meaningful title made from the request's
subject (`tod_core::session_name`), for example "Request: lock_timeout for
the v2_org_part index". Pending ones sort first. This separates them clearly
from ad hoc conversations, and makes them easy to find again.

### 8.6 Feedback in the conversation

The "Shouldn't have asked" control and its separate verdicts go away. Feedback
happens where it is clear what it does:

- **Highlight to flag.** Select text in a brief or a triage reply, and pick
  "This is confusing" from its menu. That sends a chat message quoting the
  passage, and triage explains it and records the confusion.
- **Say it.** "You could have figured that out from X", or "this shouldn't
  have come to me": triage records it in the user's own words.

There are no whole-request chips: "confusing" with no indication of what
tells nobody anything.

### 8.7 Closing reflection

When a request closes, triage takes one short turn: what would have kept this
from reaching triage, or the user? It records one lesson (section 11) with one
of these categories:

| Category | Meaning | Lesson for |
|---|---|---|
| `asker_could_decide` | The asking agent had, or could have got, what it needed | the asking phase's role doc, stance, or a project skill (named) |
| `app_should_handle` | It was mechanical, or a tod bug | tod (rung 0 or a fix) |
| `upstream_unclear` | An earlier phase left it unclear or wrong | that phase's role doc |
| `brief_unclear` | The user had to ask what the request meant | triage's own brief guidance, quoting what confused them |
| `necessary` | Only the user could answer, and the first brief was enough | none (counted for the stats only) |

A request answered at once from the brief is `necessary`, with no lesson about
wording: it was fine. `brief_unclear` applies only when the chat shows the user
needed clarification. Anything the user said about how it could have been
avoided is quoted in the lesson.

## 9. Suggestions: non-blocking actions

Something out of scope but worth doing, the commonest being a follow-up
ticket, is a **suggestion**. It never blocks the node.

- **Store.** `node_suggestions` (synced, journaled): kind (`follow_up_ticket`
  for now), title, the exact ticket text (title, description, acceptance
  criteria), possible duplicates found, status (`open` / `accepted` /
  `dismissed`), the session that made it, and the result (the created
  ticket's id).
- **Filed by** an agent with `tod-cli suggest ticket --title … --body …`, by
  triage converting a question, or by the app itself (a check failing on main,
  4.2).
- **Duplicates.** Before it is shown, the tracker is searched (Linear, by
  title terms: mechanical first, Haiku only to judge close matches). Likely
  duplicates are listed on the suggestion ("ENG-3557 looks like this").
- **Shown** in the task panel's own Suggestions section, below requests. It
  never makes the node `You` and never stops the runner, so it outlives the
  run: a node at `done` keeps its open suggestions.
- **Accept** creates the ticket exactly as written (through the existing
  Linear integration) and links it. **Dismiss** records that it was declined.
  Either way the decision is the user's, made whenever suits them.
- Later: a fleet-wide list of open suggestions.

## 10. Obligation authority and disputes

### 10.1 Authority

A new column, `node_obligations.authority`:

| Value | How it is set |
|---|---|
| `agent` | The default. Every existing obligation starts here |
| `user` | The user created or edited it (through the app, or an answer carrying `--edit-obligation`) |
| `approved` | The user said it is correct: by picking an option that affirms it (a new `--affirm-obligation N:<id>` on `decisions ask`), by saying so in a triage chat (recorded with the session link), or with "Approve" in the obligation's row menu |

Approval is **never required**. Almost every obligation stays `agent` from
start to finish. It becomes `approved` only when the user is asked about it
and says it is right, or chooses to approve it. Since `user` and `approved`
obligations are changed only by the user, they need no text digest.

### 10.2 Who may change what

`tod_store::conversation::settled` changes from "nothing past `planning`" to
this:

- A `user` or `approved` obligation: no agent, in any state, may change or
  remove it, and none may ignore it. A change is proposed through a request
  carrying `--edit-obligation`. Triage may not answer that request; it goes to
  the user.
- An `agent` obligation may be changed (created, edited, moved, deleted) only
  by the agent of the phase that wrote it, or of an earlier phase:
  - in `proposed`: requirements and design obligations;
  - in `design`: design obligations;
  - in `planning` and later: none. The agent sends the node back instead
    (10.3).
- These are unchanged:
  - the user-directed conversations (outline, chat) are not checked;
  - verification may still move an obligation to a later `acts_in`;
  - the `code_only` rule stays.

So a code reviewer can never rewrite an original requirement: it is past
`design`, so it can only dispute one.

### 10.3 Disputes go back to the phase that wrote the obligation

An agent past the obligation's phase that finds it wrong or unclear sends the
node back with a **dispute**. This is `tod-cli lifecycle send-back` with:

- `--obligation <id>` (repeatable);
- what is wrong, and the evidence (the code, the test, the conflict).

The target is the phase that introduced the obligation: `design` for a design
obligation, `proposed` for a requirement. The Implement protocol joins
`send_back::SENDERS` for disputes.

The receiving phase agent gets the dispute in its opening, and then does one
of these:

- **Fixes it**: edits or removes the obligation, and writes a **resolution**
  saying why.
- **Upholds it**: keeps the obligation, and writes a resolution explaining how
  it is meant. It may add a clarifying constraint, or sharpen the
  obligation's wording, so the implementer sees it the next time.
- **Passes it up.** A design obligation that only restates a requirement goes
  on to `proposed`, carrying the whole chain: the original dispute plus the
  design agent's note.
- **In `proposed`,** the phase agent compares the chain with the ticket,
  which only that phase reads. It fixes the requirement, upholds it with a
  resolution, or, when the ticket itself is in doubt, asks the user. That is
  a real `conflict` or `intent` request: "The ticket says X, the code shows
  Y; which holds?"

When the node comes forward again, the disputing phase's opening contains the
dispute and its resolution, so the agent sees why.

`node_send_backs` gains `obligations` (the disputed ids) and `resolution`
(written by the phase that settled it). The existing loop guard
(`send_back::guard`) still stops a dispute bounced back and forth with nothing
changed.

### 10.4 Keeping send-backs cheap

A send-back clears the certificates of the states being redone
(`lifecycle_reset`). For a dispute that ends upheld, with only a clarification
added, re-running every phase in between is waste. So certificates are
**suspended rather than deleted**: when the node comes forward again, a phase
whose certificate digest still matches what it covers gets its certificate
back with no agent. Only a phase whose inputs actually changed runs again.

### 10.5 What protects the ticket

There is no `ticket` authority, and the ticket is not added to any other
session's context. The ticket is protected by the chain: a doubtful obligation
ends up in front of `proposed`, the one phase that reads the ticket, together
with the reason it was doubted. Faulty translation from ticket to obligations
is caught there, and fixed without asking the user unless the ticket itself is
in question.

Every dispute is a lesson (section 11): an earlier phase was unclear or wrong.

## 11. The self-improvement loop

### 11.1 Lessons

A new table, `improvement_lessons` (synced, journaled). It replaces
`request_feedback`, migrating its rows (none in the live data).

| Field | |
|---|---|
| `source` | `triage` (8.7), `send_back` (every dispute and every send-back, written automatically from its reason and resolution), `incident` (tod bug candidates), `ladder` (a quick fix that had to escalate, a fixer that failed), `user` (a highlight or chat remark) |
| links | node, decision, conversation (transcript), send-back, incident, as they apply |
| `category` | as in 8.7, plus `upstream_wrong` for a dispute resolved by a fix |
| `target` | where the improvement belongs: a context fragment, a process role doc, a project skill, triage's brief guidance, the access/broken template, or tod code; named as specifically as known |
| `summary` | what went wrong and what would have prevented it |
| `user_quote` | the user's own words, when given |
| `status` | `new`, `grouped`, `applied`, `declined` |
| `applied_ref` | the commit or node that acted on it |

### 11.2 Process review

A `process_review` session runs often: daily, or once 10 new lessons
accumulate, whichever is first (configurable).

- It groups new lessons by target and cause.
- It drafts changes to the targets: tod's own context fragments and role docs,
  the triage guidance, the request template, a project's skills. The brief
  format is the first thing expected to need this: even the hand-run
  `node-request-brief` skill does not yet produce briefs that are easy to
  take in, so triage's brief guidance starts from it and is improved from the
  `brief_unclear` lessons.
- Its output is a node per run, under a configured parent, listing the lesson
  groups with links and carrying the diff as its work. It goes through the
  lifecycle like any node, so the user approves the change. Prompts are never
  edited silently.
- A lesson acted on is marked `applied` with the commit, so every change can
  be traced back to the lessons behind it.

Changes to tod's own files land in the tod repository; changes to a project's
skills land in that project. One run may therefore make two nodes.

### 11.3 Measures

Recorded from the start, and readable with `tod-cli lessons stats` until there
is a view:

- requests per node, by reason, protocol and phase;
- for each rung, the share of items it settled;
- triage: how often it answers itself, how often the user changes its answers,
  and how often the brief was unclear;
- time to answer;
- agent cost per request, at list prices (`tod_agent::pricing`);
- incidents: the number of nodes each one absorbed, and the sessions saved;
- send-backs, and disputes by the phase they went back to.

### 11.4 The self-improvement view (built)

The status bar's **Lessons** button opens a dialog showing the feedback
gathered, how it was grouped, what was changed because of it, and the measures
(`doc/ui/self-improvement.md`). Lessons are sectioned as new, one section per
process-review run, one per commit or node that acted on them, and declined; the
Measures tab is the same computation as `tod-cli lessons stats`. A lesson links
to its node (opening its task panel); an applied commit's changes are shown as
`git show --stat` text, or its checkout opens in the code editor. The user can
decline a lesson or reopen a declined one.

## 12. What this removes or replaces

- The task panel's **Shouldn't have asked** control and the `request_feedback`
  verdicts. Their role is taken by 8.6 and 11.1.
- `check_unrelated` requests to the user. The agent's claim is checked
  mechanically (4.2); a claim that cannot be confirmed is work or goes to
  triage.
- `bot_silence` requests, by default (4.4).
- "Obligations are frozen after `planning`". It becomes authority plus the
  phase rule (10.2).
- "Wording a request" in the `autonomous-session` stance. It moves to triage's
  surface, and the access/broken template replaces it for those reasons.

## 13. Build order

Each step stands alone and is worth having without the next.

1. **Mechanical checks.** Rerun infrastructure failures, waive pre-existing
   ones, configured fixers that rerun only their check, and the review
   exemption for fixer commits (4.2). Bot silence proceeds without (4.4).
2. **Duplicates and structured requests.** Duplicates across sessions and at
   any age (4.5); structured access, broken and capability requests with the
   app template (7.1).
3. **Obligation authority.** Authority (10.1), the phase rule (10.2), disputes
   as send-backs with resolutions (10.3), and suspended certificates (10.4).
4. **Suggestions** (section 9).
5. **Quick fix** (section 5) and the small-change review in `pr_draft`.
6. **Incidents** (4.3), local and cloud (done).
7. **Triage**: the protocol, the `triage` status, answers with `decided_by`
   and session links, Discuss in the chat drawer with the request-session
   labels, highlight to flag, and the closing reflection (section 8).
8. **Lessons and process review** (section 11), without the view.
9. **Scored review findings** (5.1): importance, impact and complexity on every
   finding; only blockers and majors (and minors early) block; mechanical and
   simple fixes go to one quick fix with the small-change review; recurring
   mechanical findings suggest a check.

## 14. Open questions

1. **Triage's model.** Sonnet at low effort, or Haiku with escalation to
   Sonnet when it is about to answer itself?
2. **Planning and design obligations.** Under 10.2, a planning agent that
   finds a design obligation missing must send the node back to `design`.
   Should `planning` be allowed to *add* (not change) design obligations?
3. **Bot silence default.** Is proceeding without the bot right for every
   project, or should `on_silence: ask` be the default for some bots?
4. **Small-change review.** Is one cheap reviewer on the diff enough for a
   quick fix in `pr_draft`, or should it be the full round?
5. **Process-review cadence.** Daily or at 10 lessons, as proposed, or more
   often?
