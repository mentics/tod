# State: `proposed`

**Gate:** `proposed` → `design`. The app checks it: the task has at least one requirement of its own, and the phase is certified.

## Your work

Turn whatever the task says it is for into requirements-phase obligations: the concrete task this task exists to do.

1. Read the task's title, details, and any ticket text, its own obligations, and what it inherits.
2. **Challenge the ticket.** A ticket is someone's understanding at one moment, and it can be wrong about the cause, the numbers, or the fix it suggests. List its claims (what is broken, how much, why, the fix it proposes) and check each one you can against the real thing: read the code involved, query the data read-only, call the live system where that is cheap, read the history. Append what you found to the task's details with the `content` noun: each claim, what you checked, and what it showed. A claim you cannot check is recorded as unchecked, with why.
3. Write the requirements from what is **true**, with the `obligations` noun: specific enough to act on, each one checkable. Where the ticket's suggested fix is a means rather than the end, the requirement names the end ("fewer lookups fail when the part exists"), and the means is left to design. Fill in the task's details when it has none.
4. **Push back.** A claim that turned out false or doubtful goes in the details with the evidence. If it changes what the task is for, ask the user (`intent` reason) with the evidence and your recommendation. Anything you could settle yourself, settle; never ask what the code, the data, or the history can answer.
5. If they do not say what the task is for (a title alone, a placeholder, a ticket with no body), **ask the user** through the `decisions` noun: a free-text question, `intent` reason, such as "What is this task for? Nothing on it says what to build." When their answer comes back, write the requirements from it. Ask a follow-up only for what the answer left genuinely open, with options where you can list them.

6. **Sent back here.** When a later phase sent the task back because a requirement rests on a false claim (your opening's phase status says so, with the reason and evidence), start from that cause: check the claim against the code, the data, or the live system, record what it showed in the details, and correct the requirement's facts yourself, with the evidence. The facts are yours to fix; what the task is *for* is not. If the correction changes what the task is for, ask the user, with the requirement's new wording offered as an option that edits the obligation (see the `decisions` noun's `--edit-obligation`) and the evidence attached.

- **Inherit, do not duplicate.** Record only task-specific requirements and exceptions. Never copy or paraphrase inherited obligations here.
- Obligations are in effect as soon as they are written; never ask the user to confirm them.
- Every obligation you write here is introduced in the requirements phase (`--phase requirements`). Leave where it acts as `active`, unless nothing has to be built for it: one that only needs checking acts in `verifying`, and one only the release can deliver acts in `merged` or `released`.
- Anything about *how* it gets built stays a requirement as the user said it; design refines it.
- Never invent product intent. A task with no stated purpose gets a question, not guessed requirements.

## Done when

- [ ] The task's own requirements define a real, concrete task: someone could start designing it without asking what it is for.
- [ ] Each requirement is specific enough to check whether it was met.
- [ ] Each requirement acts where it can be shown true: one that holds only after the release or after a run on production ("after rollout", "after the cleanup runs on prod, a query finds none") acts in `released` (or `merged`), not `active`.
- [ ] Every factual claim in the ticket that the requirements depend on was checked against the code, the data, or the live system, with what it showed in the details, or is recorded there as unchecked with the reason.
- [ ] No requirement rests on a claim found false.
- [ ] No question was asked that the code, the data, or the history could answer.
- [ ] No two requirements conflict, and none conflicts with an inherited obligation.
- [ ] No requirement restates an ancestor's obligation, and none duplicates a sibling's cross-cutting one. Where the same concern appears on several siblings, it is on their common ancestor instead.

Nothing else. "The user said to start design" is not a condition: the runner takes the task on as soon as the task is defined.
