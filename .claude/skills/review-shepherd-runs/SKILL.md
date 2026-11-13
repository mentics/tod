---
name: review-shepherd-runs
description: Review past runs of the shepherd-nodes skill (its Claude Code transcripts). Analyze what the shepherd agent itself did (its mistakes, self-corrections, wasted steps, unverified claims, idle time, repeated errors) as well as where the user corrected it, and improve .claude/skills/shepherd-nodes/SKILL.md so the next run is more accurate, efficient and robust. Use when asked to review, learn from, audit or improve shepherd-nodes, or to look at how past shepherd sessions went.
user_invocable: true
---

## The goal

`shepherd-nodes` gets better from how it actually ran. A review is a deep look
at the shepherd agent's own behavior, not only at the moments the user stepped
in. Most of what goes wrong in a run, the user never sees: the agent guesses
instead of checking, retries the same broken command, waits idle, ships a fix
it never verified, or notices its own mistake an hour later. Each of those is
knowledge the next run should start with.

So for every run, ask three questions:
- **Accuracy:** where was the agent wrong, and what would it have needed to
  know or check to be right the first time?
- **Efficiency:** where did it spend time, tool calls, context or subagent
  rounds that a better instruction, fact or habit would have saved?
- **Robustness:** where did it stall, break or depend on luck (a compaction,
  a misfired click, a deploy that happened to work)?

The output is a change to `.claude/skills/shepherd-nodes/SKILL.md` and an
updated ledger (`reviews.md`, next to this file). Problems in tod itself (the
product) are not skill changes: they belong in `doc/improvement-log.md`.

## 1. Find the runs

```bash
python3 -I .claude/skills/review-shepherd-runs/scripts/find_runs.py shepherd-nodes --ledger .claude/skills/review-shepherd-runs/reviews.md
```

It lists every transcript under `~/.claude/projects/` that invoked the skill,
with its size, time span, arguments, and whether the ledger has it. Review:
- every run marked `NOT REVIEWED`;
- every run with newer activity, from its "reviewed through" time on
  (`--since` below).

Also read "Lessons from runs" at the top of `reviews.md`: what a shepherd
wrote down mid-run. Each one is a finding with its evidence already found.

If the user named sessions (for example by title), match them with the
`list_sessions` tool's titles and times, and review those. Skip a run that is
still going: its lessons aren't in yet.

## 2. Profile and digest each run

Start with the profile; it shows where to look:

```bash
python3 -I .claude/skills/review-shepherd-runs/scripts/digest.py <transcript> --stats [--since <ISO>]
```

It prints wall time, compactions, subagents started, tool calls by tool,
errors grouped by kind, commands run more than twice, the longest gaps between
records, and the number of self-correction lines. Then the full digest:

```bash
python3 -I .claude/skills/review-shepherd-runs/scripts/digest.py <transcript> --tools [--since <ISO>] > <scratchpad>/<id>.txt
```

In transcript order: what the user typed (`## USER`), the agent's prose
(`-- A`), prose where it admits a mistake, corrects itself, or says it skipped,
guessed or didn't verify something (`>> SELF`), questions to the user
(`## ASK`), compactions (`## SUMMARY`), subagent hand-backs (`## AGENT`),
failed or refused tool calls (`!! ERROR`), and with `--tools` every tool call.
The `>> SELF` match is a wide net: read each one, many are honest status, not
mistakes.

The tool results themselves (command output, screenshots) are only in the raw
transcript. When a moment matters, read those records directly (find them by
timestamp) rather than trusting the agent's summary of them.

A digest under ~150 KB you can read yourself. For a larger one, or several
runs, give each to its own subagent (Sonnet, medium effort): the brief is step
3 of this file, the paths to the profile, the digest, the raw transcript and
the current `shepherd-nodes/SKILL.md`, and the ask for findings with evidence
(timestamp and a short quote). Read the user's messages and the `>> SELF` lines
yourself either way.

## 3. Find the gaps

Go through the run with each lens below. A finding names what happened, the
evidence, what it cost (time, rounds, a wrong answer, the user's attention),
and what the agent would have needed in advance to avoid it.

### The agent's own behavior

1. **Mistakes it noticed itself.** "I should have…", "correction to my earlier
   answer", "that was a guess", "my click didn't take". Trace each back to
   where it started: what did it believe, and what check or fact would have
   caught it then? The fix is usually a check the skill makes mandatory at
   that step, or a fact in its Reference.
2. **Mistakes it never noticed.** Compare its claims with the tool output that
   came before them: a state read from a screenshot glyph instead of the
   database, a "fixed" before the node got past the point, a cause stated
   firmer than the evidence. Also look for actions that did nothing (an answer
   that didn't move the node, a click on the wrong row) and the agent moving on.
3. **Inferences instead of checks.** Every "I read X as…", "I assumed", "it
   looks like" that drove an action. Which ones were wrong? Which query or
   command would have settled it, and is that in the skill?
4. **Unverified claims and skipped steps.** Fixer test results relayed
   without re-running, commits deployed with no verifier pass, rebases not
   retested, "I haven't checked yet" that was never followed up. Note which
   later caused harm (a regression, a re-fix).
5. **Rework and churn.** A fix sent back to its fixer several times; the same
   kind of hole found one code path at a time; a deploy that had to be redone;
   the same node revisited for the same reason. Ask what the brief, the
   verification or the deploy steps should have required up front.
6. **Wasted motion.** Commands repeated with small edits (quoting, paths, a
   flag that doesn't exist on macOS), the same lookup done many times,
   screenshots taken to read what a query would give, long reads of logs a
   subagent could have digested, acknowledgements of duplicate notifications.
   Each repeated error is a fact for the skill; each repeated lookup is a
   query or script worth giving it.
7. **Idle time and stalls.** The longest gaps in the profile. Was the agent
   waiting on something real (a build, a subagent), or had it ended its turn
   with nodes still not done and nothing scheduled to bring it back? Did it
   poll tightly, or not at all?
8. **Context health.** Each `## SUMMARY` is a compaction. What was lost or
   redone after it? What filled the context (screenshots, large outputs) that
   could have stayed out?
9. **Permissions and refusals.** Classifier refusals, denied edits, blocked
   commands. Did the agent retry what can't work? Should the skill say how to
   do it instead, or that it isn't this session's job?
10. **What worked.** A move the agent improvised that went well (a query, a
    script, a way of briefing a fixer). Codify it so the next run starts there.

### The user's interventions

11. **Corrections and questions** ("why didn't you…", "why are you asking
    me…", "that's a bug"). Find the line of the skill the agent followed or
    lacked. When the agent quoted a skill rule as its reason, that rule is the
    finding.
12. **Arguments that override the skill** ("the skill says only up to
    pr_draft, but…"). The default was wrong for this user.
13. **Questions the agent asked that it could have answered**, and requests it
    relayed that the user then called bugs. Either a permission is missing
    from the skill, or a rule sits where the agent doesn't reach it in time.

For each finding decide where it belongs:
- **Skill**: what the shepherd agent should do, check or know. Edit `SKILL.md`.
- **Tooling**: a script or query the shepherd keeps reconstructing by hand.
  Add it under `.claude/skills/shepherd-nodes/` and point to it from the skill.
- **tod**: what the product should do (a missing `tod-cli` command, a status
  that misled the agent). Add or bump an entry in `doc/improvement-log.md`.
- **Neither**: a one-off, or already fixed. Check
  `git log -p -- .claude/skills/shepherd-nodes/` and the working-tree diff
  first, so you don't propose what's already there.

Rank by cost: a wrong answer the user acted on, or hours idle, matters more
than a retried shell command.

## 4. Change the skill

- Change the rule that caused the behavior; don't add a second rule beside it.
  Remove a rule that did harm.
- Put a rule where the agent will be when it matters. A check that must happen
  before relaying a request goes in the step where requests are read, not in a
  section after it.
- Prefer a check over a warning: "confirm the state in `run_state` before
  acting" beats "be careful about stale UI".
- Keep the skill's style: short plain sentences, its existing sections, no
  story of what happened. The *why* goes in the ledger, not the skill.
- Keep it general. "A verify-phase obligation that can only hold after rollout
  is a wrong `acts_in`" is a rule; "ENG-2287's obligation" is not.
- Don't let it grow without bound: if the skill passes ~300 lines, tighten
  something as you add.

Show the user the diff with one line per change saying which finding it
answers. Commit it to main when they agree (tod has no PRs).

## 5. Update the ledger

In `reviews.md`, add one line per transcript reviewed, in this exact form
(`find_runs.py` reads it):

```
- `<transcript uuid>` reviewed through <last timestamp reviewed> — <session title>
```

Then one section for this review: the date, the run profiles (wall time,
compactions, errors, idle), the findings (each with evidence, cost, and where
it went: skill, tooling, tod, or neither), and what was applied. Findings you
didn't apply stay listed as open, so the next review starts from them, and a
finding that comes back in a later run gets its count bumped.
