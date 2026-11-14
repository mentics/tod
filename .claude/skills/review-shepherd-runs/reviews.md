# shepherd-nodes reviews

## Lessons from runs

The shepherd adds here what it learned mid-run that the skill should have told
it (it doesn't edit its own skill during a run). Each review folds these into
the skill and clears the ones it applied.

(none yet)

## Transcripts reviewed

- `fec2b2b0-45ba-43a3-bacb-1d4613e56139` reviewed through 2026-11-12T23:12:20Z by v1 — Shepherd nodes for Eng tasks (1st run)
- `027bd068-2c83-43fb-b125-7ee5d6a22dff` reviewed through 2026-11-13T02:04:42Z by v1 — Shepherd nodes for Eng tasks (2nd run)

## 2026-11-13: the two "Eng tasks" runs

Scope: every Lifecycle node under "Eng tasks" (`linear-tasks`, 46 nodes) on the
live root. About 20 fixes merged and deployed across both runs.

**Applied 2026-11-13** to `shepherd-nodes/SKILL.md`, merged over 36eefdc7
(every phase through `done`) and the other session's uncommitted edit (no
"never start a node", no company names): A–F and 1–9 in the skill, B and G
partly as tooling (`shepherd-nodes/scripts/survey.py`, a one-command sandbox
probe). Finding 2 was already done by 36eefdc7. Still open: whether the
shepherd may edit its own skill (F: it now writes lessons to this file
instead), and the skill is ~325 lines, over the ~300 target.

Profiles (`digest.py --stats`):
- Run 1: 4 h 17 min, 2 compactions, 17 subagents, 140 Bash calls, 8 errors
  (3 zsh `= not found`, 2 classifier refusals), idle gaps of 76 and 45 min.
- Run 2: 1 h of work, then 14.7 h idle with nodes still open; 17 subagents,
  no errors, no compactions.

### Skill: the agent's own behavior

A. **It stops after one pass.** Run 1 ended its turn at 19:23 and 20:09 with
   nodes still stuck and nothing scheduled, and sat idle 45 and 76 min until the
   user came back. Run 2 ended at 02:04 with nodes open (several waiting on
   bots and CI) and sat 14.7 h. The skill says "repeat until every node is
   done", but gives no way to come back. Add: never end a turn while a node in
   scope is not done or waiting on the user; schedule the next pass
   (`ScheduleWakeup` / a `Monitor` on the database) sized to what it waits on.
B. **It inferred state from the UI instead of checking.** Run 1 read three
   `proposed` nodes as "not started" from their red triangles and empty run
   state; ENG-2057 had a `no_progress` stop from Nov 10 (self-corrected at
   20:09). It also read the task panel's state chip as truth while the plan
   said otherwise (23:12). Add to the survey: decide every node's state from
   `node_lifecycle`, `run_state` and `decisions` in the database, never from a
   glyph or screenshot.
C. **It relayed fixers' test results without re-running them** ("I haven't
   re-run them" at 21:32, 22:21, 22:26, 01:15, 01:39) and deployed rebased
   commits nobody retested; one batch stopped 11 nodes. The verifier rule
   exists but was dropped under time pressure for the last commits of each
   batch. Make the verifier pass a precondition of `build.sh`, and have the
   verifier run the scoped tests on the rebased commit.
D. **It held verification to batch it** ("I'll start it once the obligation
   fixer lands, so both can be verified together", 21:32), so each fix waited on
   the slowest. Start each verifier as soon as its commit lands.
E. **It accepted actions that did nothing.** An answer "Send back to active"
   left the node in `review` and the agent asked again (22:33); "Answered
   elsewhere" clicks didn't take (23:12). After every GUI or CLI action that
   should change a node, read the database to confirm it did.
F. **A self-improvement edit was blocked.** At 21:27 the Edit tool refused
   `.claude/skills/shepherd-nodes/SKILL.md` (permission settings) and the
   commit was denied, so the agent's own lesson was lost. Either allow the
   shepherd to edit its skill, or have it write lessons to a file this review
   skill reads (and say so in the skill).
G. **Repeated command shapes.** The same `tod-sandbox exec` probe was rebuilt
   4 times with small changes; zsh `[ a == b ]` failed 3 times. Give the skill
   a ready query/probe script for a node's sandbox health, and the zsh fact.

### Skill: the user's interventions

1. **"Never start a node" left proposed nodes untouched.** Run 1 skipped three
   `proposed` nodes, one of them (ENG-2057) stalled since Nov 10 with a
   `no_progress` stop, and quoted SKILL.md:46 when the user asked why
   (2026-11-12T20:08). The rule is already removed in the uncommitted working
   tree; line 31 still says "list the *started* nodes". Commit the removal, fix
   line 31, and say outright: starting an unstarted node in scope is the
   shepherd's job.
2. **Merged and released were out of scope.** Run 2's arguments overrode it:
   "the skill says only up to pr_draft, but I want you to also look at released
   and merged" (2026-11-13T01:05). Extend the loop to `merged` and `released`
   (waits on a release, prod steps that need the user's access, product calls).
3. **Requests were relayed instead of triaged.** Run 1 passed two verify-phase
   requests to the user: a post-rollout measurement (an obligation in the wrong
   phase) and "prod-kb unreachable (owning machine not connected)" while the app
   was running. The user called both bugs the agent should have caught
   (2026-11-12T21:25); the agent agreed it "treated them as agent claims and
   relayed them". The triage rule exists ("Requests to the user") but sits after
   the loop. Move it into loop step 2 as a check that must pass before any
   request reaches the user, with the patterns: an obligation that can only hold
   after rollout or a prod run → wrong `acts_in`; "unreachable / not connected /
   not found / missing tool" while tod is running → tod's bug, fix it.
4. **Asked permission to send a node back.** "Why are you asking me for
   permission here? … There's no problem with sending things back"
   (2026-11-12T22:32). Add to Hard rules: moving a node back to an earlier state,
   rerunning a phase, resetting an environment are the shepherd's calls; only
   `pr_review`, marking ready and choosing a reviewer are the user's.
5. **A deploy stopped 11 nodes.** Run 2 deployed a batch of fixes, then found
   two regressions (shallow-clone `merge-base`, a narrowed submodule refspec)
   (2026-11-13T01:50); the last four commits shipped without a verifier. Add to
   the deploy steps: survey right before and a few minutes after each deploy, and
   treat any node newly stopped as a regression to fix before anything else;
   don't deploy a commit that has no verifier pass without saying so.
6. **Fixes found path by path.** The sandbox-delete guard took three rounds
   (4 paths, then 4 more, then EXPIRING/orphans); the lost-work and submodule
   fixes took 2–3 rounds each. Fixer briefs should require listing every code
   path that does the same thing (all callers of the delete, every push site)
   and putting the guard at the lowest common point.
7. **Context ran out twice** in run 1 (49 MB transcript). Keep a state file in
   the scratchpad (scope table, fixers and verifiers in flight, commits not yet
   deployed, requests answered) and update it at each step, so a compaction
   loses nothing.
8. **GUI clicks misfired** answering requests and changing state (clicks
   opening the Obligations panel, a stray key filtering the tree). Part of it was
   the user using the app at the same time (2026-11-12T22:34). Add: after each
   GUI action confirm it in the database; when clicks keep missing, ask whether
   the user is using the app before debugging.
9. **Reference facts:** zsh errors `= not found` on `[ a == b ]` (use `=` or
   `[[ ]]`; hit 3 times); the auto-mode classifier refuses prod reads from this
   session (don't retry; it is the node's agent's job through `tod-cli db`);
   duplicate subagent hand-backs need no reply.

### tod

- There is no `tod-cli` command to set a node's lifecycle state; the only way
  is the task panel's state chip, which the shepherd struggled to click
  (2026-11-12T22:34). **Fixed** 2026-11-13: `tod-cli lifecycle set`
  (75ddec36, 81cc6fe5); 1 review round.
- Answering a request with "Send the task back to active" only told the agent;
  nothing moved the node (2026-11-12T22:34). **Fixed** 2026-11-13: options
  carry a move (`decisions ask --moves-to`), answering performs it once
  (4113f3fe..309bd43b). Took 3 review rounds (hidden flush error, re-answer
  moving again, stale question after Check now): finding 6 again, the brief
  didn't ask for every way the node's state or the decision can change.

### Worked well (keep)

- Haiku verifiers given the symptom, not the fixer's conclusion, caught real
  holes in most first-round fixes.
- Answering stale requests "Fixed, try again" after deploying the fix.
