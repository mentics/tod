## `tod-cli triage`

What a triage session records about the one request it reads. Every verb names
the request (the 8-character prefix is fine) and works only from the triage
session that was started for it.

```
tod-cli --data-root <DATA_ROOT> triage answer <DECISION_ID> [--option <N>] [--text <TEXT>] --rationale <TEXT>
tod-cli --data-root <DATA_ROOT> triage escalate <DECISION_ID> --brief <TEXT> [--option <TEXT> (repeatable)]
tod-cli --data-root <DATA_ROOT> triage convert-suggestion <DECISION_ID> --title <TEXT> --body <TEXT> --rationale <TEXT>
tod-cli --data-root <DATA_ROOT> triage convert-dispute <DECISION_ID> --to <STATE> --reason <TEXT> --obligation <OBLIGATION_ID> (repeatable) [--evidence <KIND>:<ID> (repeatable)]
tod-cli --data-root <DATA_ROOT> triage settle <DECISION_ID> [--option <N>] [--text <TEXT>] [--rationale <TEXT>]
tod-cli --data-root <DATA_ROOT> triage lesson <DECISION_ID> --category <CATEGORY> --summary <TEXT> [--quote <TEXT>]
```

- `answer` decides the request: the option number it picks (1-based, as
  numbered in the context), or free text, and why, in the user's terms. It is
  refused when the request's options rewrite an obligation or move the task,
  or when the chosen option does: escalate those.
- `escalate` hands the request to the user. `--brief` replaces the question
  (see "Wording a brief" in how you work here) and the agent's own words are
  kept. `--option` replaces the options; when an option moves the task or
  rewrites an obligation, the count must stay the same.
- `convert-suggestion` files the request's point as a suggestion (work for
  later) with a title and a body that stands alone, and closes the request.
- `convert-dispute` sends the task back to `--to` (before `pr_review`) as a
  dispute of the obligations named, and closes the request. The loop guard of
  a send-back applies.
- `settle` records what the user decided with you in the chat, after an
  escalation: their pick or their words.
- `lesson` is the one reflection a closed request gets. `--category` is one of
  `asker_could_decide` (the asking agent could have decided it),
  `app_should_handle` (a rule or a check could have), `upstream_unclear` (an
  earlier phase left the point unclear), `brief_unclear` (the request or the
  brief was hard to take in) or `necessary` (only the user could answer: asking
  was right). `--summary` says what went wrong and what would have prevented
  it; `--quote` is the user's own words about the request, when they said some.
