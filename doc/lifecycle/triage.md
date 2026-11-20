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

## The session

`ProtocolKind::Triage` (`tod_core::conversation::triage`): one fresh session per
request, Sonnet at low effort (lifecycle config key `triage`), recipe
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

- One session per request: requests sharing a cause are not grouped into one
  session.
- No `--facts` / `--recommend` flags on `decisions ask`.
- No settings switch to turn triage off.
- `request_feedback` rows are kept readable but are not migrated into lessons
  (step 8).
