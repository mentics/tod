# Triage

A request an agent files for the user is read first by a fresh, small, read-only
session, so the user sees only what only they can decide, in a brief written to
stand on its own. Design: `doc/requests/design.md` section 8 (and 7.2).

## What gets triaged

A `decisions ask` from a lifecycle protocol (implementation, verification,
review, fix, a phase agent, the PR agent) with reason `intent`, `missing_rule`,
`conflict`, `risk` or `other` is filed with status `triage` instead of
`pending` (`tod_store::decisions::DECISION_TRIAGE`, schema v112). Access,
broken, capability, `check_unrelated` and `bot_silence` requests stay `pending`:
the app wrote them, or only the user can act on them. `Decision::is_open` is
pending or triage; the user's list (`tod_core::attention`) counts only
`pending`.

While a request is in triage the node is **Working**, not **You**: the
autopilot's triage session holds a live activity lease, which `next_actor`
reads as `Work::Conversation(Triage)`. No `next_actor` code was needed. A triage
request with no live run (the run died, the app was closed) reads as the node's
ordinary standing; the next autopilot round starts or resumes the session.

## Turning triage off

`lifecycle.triage` in `tod.yml` (Settings, Lifecycle: "Triage requests before
they reach you"), default **on**. Off, a judgment request is filed `pending`, as
the agent asked it, as before triage existed (`tod_store::decisions::triage_enabled`,
read where the request is filed; an unreadable settings file fails the ask
rather than guessing). A request already in triage when it is turned off is
handed to the user as asked by the autopilot's next round (`triage::Op::Release`;
the app only, never a session), a group with its lead. No lesson is recorded for
a request triage never read, except by the user's flag (below).

## Requests that share a cause

A session may read a group of requests: the agent names what several of its
questions share with `tod-cli decisions ask --cause <KEY>` (`decisions.cause`,
schema v115, trimmed and lowercased; `cli/decisions.md`). The requests on one
task that are in triage with no session and give the same key are one group
(`DecisionRepo::triage_group`; a request whose options move the task or edit
obligations is never grouped). Why only an explicit key: the other ways the app
knows two requests are one (same question, same held-up plan steps, same
failure signature) are refused at ask time as duplicates, and a failure
signature only comes with the access, broken and capability requests that are
never triaged. Across tasks is not grouped: a session is about one task.

- `Op::Start` makes the session for the oldest request, the **lead**
  (`DecisionRepo::for_triage_conversation`: the oldest still open), and links the
  others to it; the context lists them under "Requests that share this cause".
- Answering the lead settles the others, however it is answered: triage's
  `answer` or `convert-*`, the user's pick on the escalated card, or `settle`
  (`answer_by` in `tod_store::decisions`). Each gets the lead's option text (or
  the free text) as its answer, by the same hand (`decided_by`), the same
  session, and a rationale "Same cause as request <lead>: ...".
- An escalated group is **one card**: only the lead is `pending`. The others
  wait in `triage` behind it (`is_triage_follower`), so they are not on the
  user's list, the autopilot starts no session for them
  (`Autopilot::triage_step` skips followers), and the asking agent still waits
  on all of them.
- One reflection for the group: the lead's (`triage_reflections` skips
  followers). A request filed after the session started is not added to it;
  it waits for a session of its own.

## The session

`ProtocolKind::Triage` (`tod_core::conversation::triage`): one fresh session per
request (or per group sharing a cause, below), Sonnet at low effort (lifecycle config key `triage`), recipe
`stance/triage-session`, `surface/triage`, `cli/intro`, `cli/triage`. The
context is small: the request and its facts, the asking session's last turns,
the task's state, the obligations (with who owns them), the design summary and
what the user answered earlier on the task. It may read files, the pull request
and run read commands of `tod-cli`; it edits nothing (`conversation::settled`
refuses its outline writes). "Wording a request" lives in `surface/triage.md`:
only triage writes the brief.

`Autopilot::triage_step` (`autopilot/triage.rs`) starts it (`Op::Start`, which
creates the conversation), runs it, and waits. It does not count against the
run's session budget. The asking agent waits for the answer, which reaches it
like the user's, attributed "Decided by triage".

## Ending a request: `tod-cli triage`

Each verb works only from the session the request was given to
(`tod_store::triage::check_session`):

- `answer` decides it, with a rationale. Refused for a request whose options
  carry obligation edits or moves: the user decides those.
- `escalate` hands it to the user with a brief (and optionally rewritten
  options). The agent's own words are kept (`decisions.asked`). The session
  stays linked (`decisions.triage_conversation_id`).
- `convert-suggestion` files it as work for later.
- `convert-dispute` sends the task back as a dispute of the obligations named.
- `settle` records what the user decided with the session in the chat.
- `lesson` records the one reflection (below).

Answers record who decided (`decision_answers.decided_by`: `user`, `tod`,
`triage`), the rationale, and the session link.

## What the user sees

- An escalated request: the brief, the options, a **Discuss** button that opens
  the triage session in the chat drawer. Picking an option answers it directly;
  the number keys never do. Highlighting words and choosing **This is confusing**
  sends them into the session (see `doc/ui/task-panel.md`, "Request feedback").
- **This is confusing** is on every request's text, the access, broken and
  capability briefs included. On one with no triage session (it went straight
  to the user, or triage is off) there is no one to explain it: the flag records
  an improvement lesson (`lessons::Op::FlagRequest`: source `user`, category
  `brief_unclear`, linked to the request and the conversation that asked it,
  the highlighted words as the user's remark, target the asking phase's request
  wording) and the card says it was noted.
- The drawer's session list shows the focus node's triage sessions first when
  their request is open, with a request icon and a "Request: <subject>" title
  (`tod_core::session_name::request_title`).
- The Answered drawer shows who decided and how, with a **Triage session**
  button. **Change** works as for any answer and also offers to send the task
  back to the phase that asked when it has moved past it.

Every new button records a journey `UserAction` with a `Presented` snapshot.

## Reflection and lessons

When a triaged request closes (the user answers, or triage answers it) its
session gets one short turn (`Autopilot::triage_reflections`) and records one
lesson: a category (`asker_could_decide`, `app_should_handle`,
`upstream_unclear`, `brief_unclear`, `necessary`) and the user's own words when
they said something. The single writer is
`tod_store::lessons::record_triage_lesson` into `improvement_lessons`; step 8 of
the design reads it. A reflection that records nothing leaves a note on the
node's Activity.

## Not done

- No `--facts` / `--recommend` flags on `decisions ask`.
- Requests are grouped only by an explicit `--cause`, and only on one task.
- `request_feedback` rows are kept readable but are not migrated into lessons
  (step 8).
