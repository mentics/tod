---
name: review-shepherd-runs
description: Review past runs of the shepherd-manager skill (the manager's Claude Code transcript and its cli-shepherd and ui-shepherd subagents' transcripts, plus older shepherd-nodes runs). Analyze what the manager and each shepherd did (mistakes, self-corrections, wasted steps, unverified claims, idle time, churn, repeated errors) as well as where the user corrected them, and improve the shepherd skills and agents so the next run is more accurate, efficient and robust. Use when asked to review, learn from, audit or improve the shepherds, or to look at how past shepherd sessions went.
user_invocable: true
version: 2
reviews_ok_from: 1
---

## The goal

The shepherds get better from how they actually ran. A run is one
`shepherd-manager` session and the shepherd agents it started: several
`cli-shepherd`s, at most one `ui-shepherd` at a time, and the fixers and
verifiers. A review is a deep look at their own behavior, not only at the moments the user stepped
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

The output is a change to the shepherd files and an updated review ledger
(`reviews.md`, next to this file). The shepherd files are:
- `.claude/skills/shepherd-manager/SKILL.md` and its `config.toml`;
- `.claude/skills/shepherd-cli/SKILL.md`, `.claude/skills/shepherd-ui/SKILL.md`;
- `.claude/skills/shepherd-common/` (shared rules, triage, gap reports,
  reference, the shepherd ledger's docs and scripts);
- `.claude/agents/shepherd/cli-shepherd.md`, `ui-shepherd.md`.

Any change to a shepherd skill, a shared file or an agent retires every
running shepherd when it is deployed, so batch changes into one review. Problems in tod itself (the
product) are not skill changes: they belong in `doc/improvement-log.md`.

## 1. Find the runs

```bash
python3 -I .claude/skills/review-shepherd-runs/scripts/find_runs.py shepherd-manager --ledger .claude/skills/review-shepherd-runs/reviews.md
```

It lists every manager transcript under `~/.claude/projects/` (the session
that invoked the skill), with its size, time span, arguments, whether the
ledger has it, and under it every subagent transcript that session started,
by agent type. Pass `shepherd-nodes` instead for runs of the old
single-session skill. Review:
- every run marked `NOT REVIEWED`;
- every run with newer activity, from its "reviewed through" time on
  (`--since` below).

Also read "Lessons from runs" at the top of `reviews.md`: what a shepherd
wrote down mid-run. Each one is a finding with its evidence already found.

If the user named sessions (for example by title), match them with the
`list_sessions` tool's titles and times, and review those. Skip a run that is
still going: its lessons aren't in yet.

## 2. Profile and digest each run

Profile the manager's transcript and each shepherd's (`cli-shepherd`,
`ui-shepherd`). Fixers' and verifiers' transcripts are worth reading only
where a fix went back and forth. Start with the profile; it shows where to
look:

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

Then read the shepherd ledger's churn counts (read-only; the ledger is
`<root>/shepherd/ledger.db`, `<root>` the `data_root` in `install.toml`):

```bash
python3 -I .claude/skills/shepherd-common/scripts/ledger.py <root> owners
```

Per shepherd (owners are kept after they retire): heartbeat age, nodes held,
node visits, empty visits (a visit where it did nothing), signal and
acknowledgement. Why each retired, and the gaps it reported, are in its final
reply in the manager's transcript and in `ledger.py <root> gap list`.

The tool results themselves (command output, screenshots) are only in the raw
transcript. When a moment matters, read those records directly (find them by
timestamp) rather than trusting the agent's summary of them.

A digest under ~150 KB you can read yourself. For a larger one, or several
runs, give each to its own subagent (Sonnet, medium effort): the brief is step
3 of this file, the paths to the profile, the digest, the raw transcript and
the shepherd files that run used, and the ask for findings with evidence
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

### Running in parallel

11. **Churn.** Shepherds that retired after a visit or two, many empty
    visits, nodes dispatched with no fingerprint change or recheck due, a
    node visited again and again for the same reason. Is it the settings
    (`linger_minutes`, `min_batch`, recheck times), a verdict chosen wrong
    (`in_motion` with too short a recheck), or a rule in the dispatch?
12. **Idle slots.** A shepherd blocked in `wait` while the queue held due
    nodes it should have claimed; fewer shepherds running than
    `max_parallel` with work waiting; a deploy drain that took long because
    one shepherd never reached an action boundary.
13. **Gaps.** The same cause reported under two signatures (the signature
    rules in `gap-report.md` failed), a gap the manager fixed twice, a
    summary a fixer had to re-investigate, a shepherd that fixed or deployed
    anything itself.
14. **Conflicts.** Lost leases, `check` replies other than `ok`, the UI
    shepherd's `user-conflict` and `no-target` replies: did the shepherd
    re-decide, or carry on as if it had acted?

### The user's interventions

15. **Corrections and questions** ("why didn't you…", "why are you asking
    me…", "that's a bug"). Find the line of the skill the agent followed or
    lacked. When the agent quoted a skill rule as its reason, that rule is the
    finding.
16. **Arguments that override the skill** ("the skill says only up to
    pr_draft, but…"). The default was wrong for this user.
17. **Questions the agent asked that it could have answered**, and requests it
    relayed that the user then called bugs. Either a permission is missing
    from the skill, or a rule sits where the agent doesn't reach it in time.

For each finding decide where it belongs:
- **Skill**: what the manager or a shepherd should do, check or know. Edit
  the file that agent reads: the manager's, one shepherd kind's, or a shared
  file when both kinds need it. Never put GUI matter in the CLI shepherd's
  context, or the other way round.
- **Config**: a default in `shepherd-manager/config.toml` that was wrong for
  the load (churn and idle slots usually land here).
- **Tooling**: a script or query a shepherd keeps reconstructing by hand.
  Add it under `.claude/skills/shepherd-common/scripts/` and point to it from
  the skill that uses it.
- **tod**: what the product should do (a missing `tod-cli` command, a status
  that misled the agent). Add or bump an entry in `doc/improvement-log.md`.
- **Neither**: a one-off, or already fixed. Check
  `git log -p -- .claude/skills/shepherd-* .claude/agents/shepherd/` and the working-tree diff
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
- Don't let them grow without bound: if a skill passes ~300 lines, tighten
  something as you add. Every line of a shepherd skill is paid once per
  shepherd started.

Show the user the diff with one line per change saying which finding it
answers. Commit it to main when they agree (tod has no PRs).

## 5. Update the ledger

In `reviews.md`, record each transcript reviewed with the script, never by
hand; it writes the line in the form `find_runs.py` reads, with the current
version:

```bash
python3 -I .claude/skills/review-shepherd-runs/scripts/find_runs.py shepherd-manager --ledger .claude/skills/review-shepherd-runs/reviews.md --mark <manager transcript path> "<session title>"
```

Mark the manager's transcript only; its shepherds' transcripts count as part
of it. Then one section for this review: the date, the run profiles (wall
time, compactions, errors, idle, and for the shepherds: started, visits,
empty visits), the findings (each with evidence, cost, and where
it went: skill, tooling, tod, or neither), and what was applied. Findings you
didn't apply stay listed as open, so the next review starts from them, and a
finding that comes back in a later run gets its count bumped.

## Versions

`version` in the frontmatter says which release of this skill reviewed a run;
the ledger records it on every line. Bump it on any change to this skill or
its scripts that could find something new, and add a line below.

`reviews_ok_from` is the oldest version whose reviews still count. A minor
change bumps `version` and leaves it alone, so runs reviewed by earlier
versions stay done. A major change (a new lens, a new source of evidence) also
raises `reviews_ok_from`: runs reviewed by an older version then show as
`STALE` in `find_runs.py` and get a full review, from the start of the
transcript. Tell the user how many runs that is before starting, and let them
choose which to redo.

- v1: lenses on the agent's own behavior and on the user's interventions,
  tooling and tod gaps, the ledger.
- v2: runs are a `shepherd-manager` session and its shepherd subagents;
  `find_runs.py` lists the subagent transcripts; the shepherd ledger's churn
  counts; the "Running in parallel" lenses. Old `shepherd-nodes` runs had no
  shepherd subagents, so their v1 reviews still count (`reviews_ok_from`
  stays 1).
