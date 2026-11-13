# Improvement log

What went wrong while driving real nodes through the lifecycle, how often, and
what was done about it. One entry per kind of problem; bump its count and add
the node when it happens again. Newest kinds first. The point is to see which
problems recur and fix those at the root (app, prompts, context), not one node
at a time.

Fields: **Seen** (count, nodes, dates) · **Symptom** (what the user saw) ·
**Cause** · **Fix** (commit, or "open") · **Measure** (what statistic would
have shown it sooner).

## Statistics worth collecting

Ideas, not yet built. Each would come from data tod already has (conversations,
turns, findings, run_state, node_timeline, journey records):

- Per node and phase: agent sessions, turns, tool calls, tokens, wall time.
- Review loop: rounds per stay, findings per round, how many rounds came back
  clean, how many findings were reopened (fixer said fixed, review found it
  again), rejected/declined ratio.
- PR loop (pr_draft): bot review rounds, bot findings per round, threads
  answered vs. code changed, CI failures per push.
- Requests to the user: how many, how long they waited, how many were
  answered "carry on" or with an option the agent offered (a sign the
  question was not needed), how many were about tod itself (broken setup)
  rather than the work.
- Stops (`run_state` stopped): count by `reason_kind`, and how many were
  resumed without any change by the user (a sign the stop was wrong).
- Sandbox churn: replacements per node, time spent setting up, losses.

---

## 2026-11-13: sixty agent turns on a pull request that needed none (ENG-3095)

- **Seen**: 1, ENG-3095 (`pr_draft` for 4 days, flagship#6659), 2026-11-09 to 2026-11-11. The run stopped on the supervisor budget request "Used 60 of 60 sessions (1.6 of 16 hours)".
- **Symptom**: a node whose pull request was only waiting on a review bot and a busy base branch used up its whole session budget. The PR conversation holds ~60 agent turns, ten minutes apart or less ("merge the base in", "greptile has not reviewed the current head: if it has to be asked, ask it", "nothing to push"), plus 24 review and 7 fix sessions of tod's own review of those merges.
- **Cause**: (1) a bot that had not reviewed was *work* for the agent (fixed since in e6a0d47e, 2026-11-12: it is a wait and tod asks); each agent turn that posted an ask changed the PR, so the "two turns changed nothing" guard never fired. (2) The base branch of that repository moves every few minutes, so "behind its base" was work again after every push; each merge cost a PR turn, a review round and a push turn, with no bound, because merging the base in is deliberately not a counted round. (3) The "unchanged" counter lived in a local variable of `babysit`, so every resume or wake started it again.
- **Fix**: a round whose only work is merging a moved base in is started at most once per `PR_HOUSEKEEPING_GAP` (30 min; the run waits and polls in between, saying so) and at most `PR_HOUSEKEEPING_LIMIT` (8) in a row before stopping with `PrStuck` ("the base branch moved N times in a row..."); the counters (`loops.pr_housekeeping_*`, `loops.pr_unchanged`) are saved and reset when the PR is clear or waits for a person. All sessions in `pr_draft`/`pr_review` (entry turn, babysit turn, change-reviewed turns, skill groups, own review, fix) start in the one `babysit` loop, which now makes the decision before any of them.
- **Follow-up (verifier)**: the gap is measured from the last base merge whatever the count, and the count resets only when the PR is handed to a person, not on a clean read tod's own merge and review caused (ENG-3095's merge, own-review fix, clean read, base moves again had bypassed both). A failed GitHub read after a turn no longer resets the unchanged count: it surfaces through `github_look_failed`.
- **Tests**: `the_gap_between_base_merges_survives_a_clear_read`, `a_base_that_keeps_moving_is_waited_on_not_merged_in_every_poll`, `a_base_that_never_stops_moving_ends_in_a_precise_stop`.
- **Measure**: agent turns per node per stay in `pr_draft` (target: a handful); budget requests whose sessions are mostly `pr` turns that changed nothing (should be 0).

## 2026-11-13: six drafts stopped on a "Greptile did not review" request (ENG-2927, ENG-2923, ENG-2157, ENG-2057, ENG-3093, ENG-2228)

- **Seen**: 1 kind, 6 nodes in `pr_draft`, 2026-11-13.
- **Symptom**: nodes stopped on a pending request about the review bot: the PR agent raised it itself after pinging the bot 2-5 times (and in ENG-2228 after `gh pr comment` hit GitHub's transient "Something went wrong while executing your query", asking the user to "Fix GitHub"); tod's own `bot_silence` decision for ENG-3093 said "after 0 asks (last asked never)". ENG-2927's only commit after Greptile's review merged main in, so Greptile (`triggerOnUpdates: false`, drafts) rightly did not run again.
- **Cause**: (1) the "head only merges the base in, so the review still covers it" comparison existed only in `pr_readiness::live`; the gate's `pr-approved.review-current` and `pr-pr_review.*` criteria, `pr_actions` and the babysitter's feedback check called `Assessment::of`, which never compares, so they judged the review stale. (2) A bot with no `ask` in `tod.yml` was never asked by tod (the live root has none) and, with nothing to ask with, was called silent by a time rule with `asks = 0`. (3) The PR role docs said not to ping the bot but nothing told the agent not to raise a request about it or about a transient GitHub error.
- **Fix**: `pr_readiness::assess` (feed + pr + snapshot + settings) is the one place a review is judged current; `live`, the gate, `pr_actions` and `github_wait` use it (the gate reads the feed override too). `pr_readiness::ask_text` gives the built-in bot's own ask when settings have none, and it is what asks, the agent's pings (matched case-insensitively, trailing punctuation fine), waived asks and the patience rule use, so tod asks (via `post_issue_comment`, which retries a transient failure) before it can call a bot silent. `surface/pr.md`, `pr_draft.md` and `pr_review.md` forbid a request about a silent bot or a transient GitHub error. Tests: `pr_readiness::tests::a_review_is_current_when_the_head_only_merges_the_base_in_for_every_reader`, `a_bot_with_no_configured_ask_is_asked_by_default_and_not_called_silent_unasked`. Open: an agent request about a bot is still not refused or converted by tod (prompt only); already-raised requests need answering by hand.
- **Measure**: pending requests on a node in `pr_draft`/`pr_review` whose subject is a bot or a GitHub error (should be 0); `bot_silence` decisions with `asks = 0` (should be 0).

---

## 2026-11-12: a person's "send the task back" answer moved nothing (ENG-2068, ENG-2287)

- **Seen**: 1 kind, 2 nodes (ENG-2068 in `review`, ENG-2287 in `verifying`), 2026-11-12.
- **Symptom**: a request offered "Send the task back to active to rebuild the change"; the person picked it; the node stayed in `review` with its steps verified, the agent was only told the answer as text and asked again. A person then moved both nodes by hand with the task panel's state chip.
- **Cause**: an option was only text. Answering recorded the answer and told the asking agent; nothing moved the node, and the agent has no way to. The lost-work rule (deb5524e, 3ffd4575, 1d6bfb92) closed the one case where the checkout proves the code is gone (and the verify/review prompts stopped offering "send back" there), but any other answer that means "move this node back" still depended on an agent that cannot.
- **Fix**: an option can carry the move (`tod-cli decisions ask --moves-to <N>:<STATE>`, `decisions.option_moves`, schema v94; only a state earlier than the node's current one and before `pr_review`). Answering it calls one function, `tod_core::lifecycle::send_back` (via `send_back::answer_decision`, the only way `AgentRuns::record_decision_answer` records an answer): it validates, moves the node as the user's own move (same as the state chip, `set_lifecycle_by_user`, which also clears what the states redone certified), reopens the implementation steps and verdicts when the target is `active` or earlier, then records the answer; a move that cannot be made fails the answer with the reason and records nothing. It moves through `lifecycle::operator::user_move` (the chip's and `tod-cli lifecycle set`'s one path), refuses `pr_review` and later (`check_operator_target`), and fails if the write did not land (`set_lifecycle_by_user` no longer drops a flush error) or the node is not in the target afterwards. A corrupt `option_moves` value is a read error, not "no moves". The move is made once, when the request is first answered: a changed answer that picks a moving option again is refused with an error pointing to the state chip (a changed answer to any other option still records), and a manual move (chip or `tod-cli lifecycle set`, the shared `SetLifecycleByUser` mutation) withdraws the node's pending moving requests. Each moving option also records the state the node was in when asked (`OptionMove.asked_in`); answering after the node has gone on (the gate advanced it, a person moved it) is refused with an error naming from and to and pointing to the state chip, whatever moved it. Rows from before this was recorded have no such state and are not refused. Repeating a still-pending answer is safe: a node already in the target is not moved again and the reopen runs again. Still not atomic: the move, the reopen and the answer are three writes; a failure between them leaves the node moved and the request pending until the answer is given again, and the error says so. The asking agent is told the app has moved the node. `cli/decisions.md` and the verify and review surfaces tell agents to offer a send-back only as such an option. Tests: `lifecycle::send_back::tests`, `tod_store::decisions` (validation), `tod_cli` doc sync.
- **Measure**: pending requests whose option text says "send back"/"rework" with no move (should be 0); requests re-asked on the same node within one stay after an answer that was a send-back.

## 2026-11-12: agent asked the user to install a missing tool

- **Seen**: 1 node (update-db-monitors-to-filter-out-use…), 2026-11-12.
- **Symptom**: a `broken` request: "Install terraform in the sandbox, then pick Fixed, try again", after `which terraform tofu` found nothing, for a plan step that runs `terraform plan`.
- **Cause**: the autonomous stance listed "a tool is missing" among the faults not to work around and to file as `broken`, so the agent read installing the tool as a forbidden workaround. The sandbox is root-capable with internet access, so the install was one command.
- **Fix**: `stance/autonomous-session.md` now has "A tool you lack, you install" (release binary, apt, pip, npm, cargo; the pinned version; ask only when the install itself fails, naming each command and error), "a tool is missing" is removed from the faults list there and in `domain/lifecycle.md`, and "Wording a request" says never to ask the user to install a program. Test `autonomous_stance_has_agent_install_missing_tools`. No tool list added to `bootstrap.sh`: the repository has no generic per-repository extra-tools place, and tools stay the agent's to install.
- **Measure**: `broken` decisions whose text matches "not installed" / "not found" / "install" (should be zero); count of such decisions answered with "Fixed, try again" and no other change.

---
## 2026-11-12: a review bot went silent, and each node handled it its own way

- **Seen**: 7 nodes, 2026-11-11 17:47Z on (bot silent on flagship#6640, #6641, #6661, #6679, #6680,
  kb#2866, kb#2885): four `pr_draft` nodes raised their own `broken` request, two stopped `pr_stuck`,
  one asked the user to "Fix GitHub so a comment can be posted".
- **Symptom**: the PR agent re-posted the bot's ask 3-5 times per PR (comments piling up, minutes
  apart); each request worded the problem differently and none offered to proceed without the bot;
  `pr_stuck: two turns in a row changed nothing on the pull request`; a `gh pr comment` that failed
  twice 10 s apart with GraphQL "Something went wrong while executing your query" reached the user.
- **Cause**: (1) an overdue bot review was `Work::BotReviewMissing`, so every `minutes` the agent
  was sent a turn that could only ping; (2) such a turn changes nothing on the PR, so two of them
  tripped the stuck counter, though no turn could have done anything; (3) nothing bounded or
  deduplicated asks, and no rule for a bot that never comes (only the agent's own `broken`
  request, differently each time); tod's own ask comment, posted as the user, would also have
  counted as a person's feedback; (4) that GraphQL text was not a transient error, and neither a
  GraphQL call nor a comment post was retried in `tod_store::github`.
- **Fix**: an overdue bot is only waited for; tod posts the project's configured `ask` text
  itself, at most `max_asks` per head and `ask_every_minutes` apart, counted from the comments;
  a silent bot raises one pending `bot_silence` request per node with fixed wording listing the
  asks and last activity, whose "Proceed without {bot}" answer removes the bot from the node's
  settings for the babysitter and the gate's criteria; `Work::agent_can_fix` keeps unfixable work
  out of the stuck counter; an `ask` comment is never feedback; GitHub's GraphQL hiccup is
  transient and GraphQL calls and comment posts are retried (2/6/15 s) before an error is
  returned. Spec: pr-readiness.md, "A bot that stays silent". Needs `ask:` set for the bot in
  `tod.yml` (`pr_readiness.bots[]`); without it tod asks nobody and the request comes after
  `rerun_after_minutes + max_asks * ask_every_minutes`. Agent-side `gh` failures inside an
  agent's own turn remain the agent's to retry; tod no longer needs the agent to post.
- **Measure**: agent turns on a PR per bot-silent head (target 0); ask comments per head
  (target <= `max_asks`); `bot_silence` requests per node (target <= 1) and how many were answered
  "Proceed without"; `pr_stuck` stops whose work was only a bot; transient GitHub errors that
  reached a request (target 0).
- **Follow-up gap (found in independent review)**: two readers still used the unwaived settings, so
  a waived bot was honored by the gate and babysitter but not by the PR controls
  (`pr_actions::load`: the buttons kept saying "Waiting for {bot}" and blocked Open while the gate
  passed) or by `conversation/pr.rs` `live_prs` (the progress fingerprint after PR agent turns);
  and a PR with no head commit time never counted as overdue, so a bot without an `ask` could never
  be told from a slow one and the request never came. Fixed: `pr_readiness::settings_at` is now
  private, every reader goes through `settings_for_node` (`pr_actions::load`/`update` take the fleet
  and node, and `live_prs` the protocol env); a missing head time while a bot has not reviewed is an
  error (`Assessment::head_time_missing`) from `live_answered` and `pr_actions::load`, so the
  babysitter shows it as a failed look at GitHub and the controls show it on the row.
  Regression tests: pr_actions, conversation::pr, pr_readiness.
- **Second follow-up (found in review)**: `live_prs` still collapsed every failure to `None` with
  `.ok()?` (a settings DB error, or `head_time_missing` from `live_answered`), and the driver read
  `progress` with `.ok().flatten()`, so the babysitter's error became a fingerprint of PR ids and
  the stuck/no-progress check ran on nothing. Fixed: `live_prs` returns `Result` (`Ok(None)` only
  with no GitHub credentials at all), `PrProtocol::progress` propagates it, and the conversation
  driver records it as an error turn and fails the turn (before and after it) instead of dropping
  it. Regression test: `conversation::pr::tests::a_failed_look_is_an_error_not_a_quiet_fallback`.

## 2026-11-12: lost work left a node in review, asking the user in a loop

- **Seen**: 2 nodes (ENG-2068, ENG-2287), in `review`; ENG-2068 asked twice.
- **Symptom**: the review agent raised a `broken` decision ("restore this task's kb code to the review checkout") offering "Send the task back to active to rebuild the change". Answering it moved nothing: the node stayed in `review`, its plan steps and verdicts stayed `verified`, and the agent asked again.
- **Cause**: the implementation commits had been lost with a sandbox before they were pushed, so the checkout held plain main while the node's steps claimed the work done. Nothing in tod looked at the checkout against the steps, an answered decision is only sent to the agent, and "send it back" was an option the agent made up with no behaviour behind it.
- **Fix**: `tod_core::lost_work` (`doc/lifecycle/lost-work.md`): in `verifying`/`review`/`pr`, a node with implemented or verified implementation steps whose checkout and submodules all have no commit beyond their base and nothing uncommitted is reopened by the autopilot, with no question: back to `active`, steps and verdicts open again, its `broken` questions withdrawn, a note on its activity. Commits on the node's branch (local or `origin/<branch>`) count wherever the checkout sits, a node branch on origin at a commit other than the base is not lost, and any git or Files failure is an error, never "not lost" (hardened after two independent reviews: only `broken` questions about the missing work are withdrawn and the rest named in the note, a merged node whose branch was deleted on origin is not lost, the Files directory must be a checkout of its own, and no branch is guessed from the checked-out one). Checked once per state per run and whenever a `broken` question is pending; a second finding in a run stops it (`NeedsHuman::LostWork`). The review and verify prompts no longer let an agent offer "send back" as an option; it asks one `broken` decision and tod resolves it from the checkout.
- **Measure**: count of `lost_work` steps and `note` timeline entries titled "Lost work: implementation reopened"; runs stopped with `lost_work`; `broken` decisions in `review`/`verification` that stayed pending while the node's branch had no commits (should be zero).

## 2026-11-12: work lost with a sandbox

### A node's work committed in a submodule is never pushed, so a reclaimed sandbox takes it

- **Seen**: 1 node confirmed (ENG-2068, 5631A571), 2026-11-10 to 2026-11-12; any cloud node whose work is in a submodule (`kb`, `flagship`, `ui`, `infra` of `cofactr/workspace`) is exposed. Verification ran on work that no longer existed, and review had to ask the user ("Restore this task's kb code to the review checkout...").
- **Symptom**: the review session found /workspace/kb on the task branch equal to `origin/main`, the test file the verdicts cited nowhere, no branch for the task in the kb repository, and `workspace`'s task branch holding only a submodule pointer at kb's `main` commit.
- **Cause**: implementation ran 2026-11-10 06:08-06:23 in the node's first sandbox. The supervisor's `git::push_branch` pushed only the workspace repository's branch; the commits were in the kb submodule, which was never pushed. Checkpoints (`checkpoint::save_repo`) keep only uncommitted work, so a committed, clean submodule had no checkpoint either. Blaxel reclaimed idle sandboxes (the app forgot 32 at 2026-11-10 14:42, and 8+ again at 2026-11-12 18:24), the node's sandbox was replaced from the golden, and nothing restored the work; its replacement note said only "made a new one". The app's own pushes (`provision::push_branches`, used by remove/refresh and the local runner) already covered submodules; only the supervisor's did not.
- **Fix**: `tod_supervisor::git::push_branch` pushes the node's own branch (only that) in every submodule (a nested one before its parent) that has commits of its own beyond what the parent pinned and no remote has, then the parent's; a boundary push failure stops the run instead of warning; failures (a detached HEAD or default branch with such commits, a rejected push) come back together naming each repository, never skipped. It now runs at turn boundaries as well as steps, so an implement or fix turn's commits are pushed when the turn ends. The replacement note now says unpushed work is gone and to check the branch. Tests in `tod-supervisor` `git::tests`. Not done: checkpointing commits that are clean and unpushed during a long turn (a checkpoint holds only uncommitted work); the work of ENG-2068 itself is not recoverable (see the node's report).
- **Measure**: count of replaced sandboxes (`cloud: replaced node ... sandbox`) whose node had plan steps implemented; submodule commits found on no remote branch when a sandbox is lost (not yet measured, nothing records it).

## 2026-11-12: bot review of a merge-only head

### The PR agent asks a review bot four times for a review it will never give, then stops

- **Seen**: 1 node confirmed (ENG-3193, Cofactr/flagship#6663), the same shape suspected on ENG-2927 (#6640) and nodes waiting on `bot-review:greptile`, 2026-11-12. About 5 hours lost, then a request to the user ("Check why Greptile is not answering review requests").
- **Symptom**: Greptile last reviewed 6a3e22b9; the head, 0d555102, differs only by merges of main. The agent asked "@greptileai please review." 4 times with no reply and gave up.
- **Cause**: `bot_standing` called a review current only when it named the head or was written after it. Greptile does not review a head whose own change is unchanged, so merging the base in (which the babysitter itself does when behind) made the review stale for good: `Work::BotReviewMissing` every `rerun_after_minutes`, and the `pr-approved.review-current` criterion failing.
- **Fix**: `Assessment::of_with` asks `PrFeed::same_change` when a bot's summary names another commit; `Github::same_change` compares per-file added/removed lines of `base...reviewed` and `base...head`; the head's files must be a subset of the reviewed ones (the first version required equal sets and missed #6663, where main absorbed `.quality/baseline.json`). Covered means current, with the reviewed commit's score; a comparison that cannot be made is an error, not a fallback. Regression tests in `pr_readiness` (reviewed 6a3e22b9, head merge-only) and `change_equal_tests` (the #6663 shape). See `doc/lifecycle/pr-readiness.md`.
- **Measure**: count of `BotReviewMissing` sends per head whose change equals the last reviewed commit's; and PRs where a bot's review is older than the head and the head's own diff is unchanged.

## 2026-11-12: slow replacement

### Replacing a sandbox spends ten minutes copying session logs

- **Seen**: 1 node (ENG-2925, F05EF005), 3 session logs, about 10 minutes, 2026-11-12. The node did nothing meanwhile.
- **Symptom**: "sending session log n of 3" for minutes.
- **Cause**: `session_log::sh_write` sent a log in 48 KB base64 chunks, each its own `tod-sandbox exec` (a relay connection plus a `sh` per chunk): about 21 calls per MB, and the bytes uncompressed. A 50 MB log is about 1,000 calls.
- **Fix**: `SandboxRemote::write` makes the project directory with one command, then uploads the whole file gzipped through Blaxel's filesystem API (`upload_large_compressed`, 4 MB parts, unpacked and renamed in place), the path the supervisor binary already takes. JSON-lines logs compress about 10 to 1. That is one `mkdir` command, two free-space checks, one PUT per 4 MB of the gzip, and one unpack command per log (about 4 calls plus 1 per 4 MB of gzip, so ~0.25 per raw MB at 10 to 1, against ~21), and about a tenth of the bytes. Failures name the log. Total bytes and seconds are logged and shown when done. Dev containers keep the chunked path (local `docker exec`, cheap). Nothing runs differently inside the sandbox (no relay or supervisor change).
- **Measure**: seconds and bytes per replacement's session-log copy (now logged as "restored session logs").

## 2026-11-11: overnight stops

### PR babysitter counts housekeeping as rounds ("PR stuck")

- **Seen**: 3 nodes (ENG-2228, ENG-2927, ENG-3055), on the night of 2026-11-10/11. CI was green on all of them, and Greptile scored them 4/5 to 5/5.
- **Symptom**: the node stopped with `pr_stuck` after hitting `max_rounds` (4).
- **Cause**: rounds were every PR agent turn since the run started. Merging main, resolving a conflict, and asking a bot to review a new head all used up the cap.
- **Fix**: b23d8034. Only a turn on feedback is a round (`Work::is_feedback`: threads, failing checks, low score, changes requested, review comments). The count resets when the PR is clear or waiting for a person.
- **Measure**: PR turns per stay, split by the kind of work.

### Review of a PR change counts clean reviews ("not converging")

- **Seen**: 4 nodes (ENG-3067 10 rounds, ENG-3095 10, ENG-3193 10, ENG-2923 7). ENG-3193's last review was clean.
- **Cause**: `pr_change_reviewed` counted every review since the change was made (`reviews_since(..).len()`). The fix of 2026-11-10 covered only the own-review path.
- **Fix**: b23d8034. Both paths now share `unconverged_rounds_since`, which counts only consecutive unclean reviews.
- **Measure**: a stop whose standing list is empty, or whose last review was clean.

### Another build refreshes the shared golden, which blocks Start

- **Seen**: 2026-11-11. All 7 stopped nodes are blocked.
- **Symptom**: "The golden sandbox for cofactr/workspace was refreshed by a newer tod (install cc0ea2cf…); this tod is older".
- **Cause**: another session's build, which is not on main, refreshed the one golden every root shares. Main's build then refuses to fork from it.
- **Fix**: open. The install stamp says which is newer, not which build is on main.
- **Measure**: golden refreshes by install stamp; Start refusals for this reason.

### Waiting for the user to mark ready misses a new conflict

- **Seen**: 1 node (ENG-3093, 2026-11-11). It waited on "flagship#6641 to be marked ready" while GitHub showed the PR as `CONFLICTING`.
- **Cause**: found 2026-11-12. Each scheduled look did re-read the PR, but a conflict on a draft was invisible: GitHub reports a draft's `mergeable_state` as `draft` whatever the branch is, and `Assessment::conflicted` read only `mergeable_state == dirty`, so `Work::Conflict` never fired while the draft waited to be marked ready. (Behind was caught: it comes from the compare count.) Whether ENG-3093 was also stopped cannot be proved now, its sandbox being pruned.
- **Fix**: `conflicted()` also reads `mergeable == false` (which only ever means a conflict); `mergeable` null stays "GitHub still computing", left to the run's own poll, never clean and never a conflict. Test: `autopilot::tests::a_draft_waiting_to_be_marked_ready_gets_work_when_it_falls_behind_or_conflicts`.
- **Measure**: nodes waiting for a person whose PR is not mergeable.

## 2026-11-10 — first pass over the 12 `pr_draft` nodes

Starting state (live root): 12 nodes in `pr_draft`; 8 stopped
`review_not_converging`, 1 stopped `pr_stuck`, 2 waiting on a request, 1
waiting for the PR to be marked ready. Every node's cloud sandbox was gone
from Blaxel.

### basedpyright check killed for memory in node sandboxes

- **Seen**: 7 nodes (ENG-2680, ENG-3055, ENG-2927, ENG-2923, ENG-3067,
  ENG-3193, ENG-3095), 2026-11-10/11, five of them as requests to the user; any node whose
  pr_draft or review checks include flagship's basedpyright.
- **Symptom**: the check fails with no output (exit 247 or nothing after
  `uv sync`); the agent files "give the sandbox more memory".
- **Cause**: a fork has its golden's 8 GB, and its root filesystem is in RAM
  (about 2.7 GB for the checkout, venvs and node_modules). The workspace's
  Blaxel boot starts the dev servers (UI vite about 1.5 GB, two flagship
  runservers and the KB API, about 1.4 GB). basedpyright reaches 2.5–3.2 GB
  RSS, and the check gives it an 8 GB heap cap, so the kernel kills it before
  V8 collects. The checks themselves run one at a time.
- **Fix**: the check's heap is 2 GB (`--max-old-space-size=2048`) in the
  pr_draft and review checks of "All tasks" (2026-11-11, by the user's
  choice; a check setting, not a commit). Measured in ENG-2680's sandbox with
  the dev servers running: 108 s, peak RSS 2.2 GB, 0 errors and the baseline
  unchanged. The five OOM requests were answered "fixed, try again". A bigger
  golden was not needed: Blaxel takes any memory size in MB (CPU follows at
  1 per 2 GB, about $0.041 per GB-hour active), but a fork cannot choose its
  own, so it would mean changing the golden.
- **Seen again**: ENG-2157 (2026-11-11), this time the ui `tsc` check, which runs with
  `--max-old-space-size=6144`. It was killed at 3.3 GB RSS, with about 3.4 GB free
  next to the dev servers. It is the same kind of problem and is not fixed yet.
  Measured: at 2048 MB tsc runs out of V8 heap after 46 s, and at 3072 MB the kernel kills it
  (available memory fell to 21 MB). So a smaller heap cannot fix it. It needs memory freed
  (the dev servers stopped while it runs) or a smaller tsc footprint.
- **Fix (tod, 2026-11-12)**: a check killed for memory (exit 137/SIGKILL, "Killed", V8 heap OOM,
  `MemoryError`) is now *broken*, not failed: it is not sent to the Fix agent, the run stops with
  a request to the user (lower the heap, scope or remove the check), and the output starts
  "Killed for memory (...)" with the machine's total and free MB when `/proc/meminfo` exists
  (`checks::run_command`; tests `a_sigkilled_check_is_killed_for_memory_not_a_finding`,
  `a_v8_heap_exhaustion_is_killed_for_memory`). Commit: see `git log -S"KILLED_FOR_MEMORY"`.
- **Fix (tod, 2026-11-12, second part)**: in a sandbox, the first kill for memory no longer
  stops the run: tod frees memory itself and runs the check once more
  (`checks::run_command_with`, `free_sandbox_memory`). It stops the dev servers found by program and
  arguments (`python manage.py runserver`, `python -m uvicorn`, node vite, not vitest; by PID, never
  `pkill`; reported stopped only when gone, a failed kill is an error and the check is not rerun) and drops the page
  cache, then retries. The servers stay stopped (no restart, so the next check has the memory
  too). Only if the retry is also killed does the node get the broken request, whose text now
  says it was killed twice and what was stopped, with free MB before and after; a failure to
  free is stated in it instead of a retry. Tests: `a_sandbox_check_killed_for_memory_frees_memory_and_uses_the_second_run`,
  `a_sandbox_check_killed_twice_is_broken_and_says_what_was_freed`,
  `a_kill_outside_a_sandbox_never_frees_memory_or_retries`. Match logic is tested against sample command lines (`only_the_workspaces_dev_servers_match`),
  including the real flagship and KB command lines and false positives (grep, tail, vitest).
- **Measure**: check failures with no output, or exit 137/247, per check; and, in the broken
  request text, "killed for memory twice" (the retry did not help) versus checks that pass
  after a freeing (the output of a passing retry is not annotated: look for the sandbox's
  dev servers gone from `ps`).

### Agent-made git worktrees hide the work

- **Seen**: 1 node (ENG-2927).
- **Symptom**: after Reset environment the checkout stayed on a detached HEAD;
  the PR head was in `/workspace/.worktrees/{flagship,kb}-eng-2927`.
- **Cause**: the workspace's CLAUDE.md tells agents to work in a worktree.
  tod reviews, checks and pushes the checkout it set up, and the worktree held
  the branch, so the checkout could not be put back on it.
- **Fix**: `workspace/codebase.md` (c3db91b2): work in the checkout you were
  started in; never make a worktree or switch branches.
- **Measure**: a check after each turn that the checkout is on the node's
  branch and `git worktree list` has one entry.

### Sandbox setup fails on concurrent forks; the kept sandbox cannot be recovered

- **Seen**: 3 nodes (ENG-3067, ENG-3193, ENG-2228), 2026-11-11 00:2x UTC.
- **Symptom**: "sandbox health check 'manifest' failed: ... agents=false,
  expected node ..."; Adopt existing sandbox then refused the kept sandbox.
- **Cause**: `fleet::sandbox::connect` ran `Sandboxes::ensure` on a half-made
  node sandbox and installed a plain manifest. A fork whose proxy update wiped
  tod's files was not reinstalled. A failed sandbox was kept for inspection
  with nothing in the app able to replace it.
- **Fix**: 240ab386 (merged 74292b77): `ensure` refuses node sandboxes that
  are not ready; `make_sandbox` reinstalls tod's files into a fork that lost
  them; Start deletes a kept failed sandbox of the same node and makes a fresh
  one (`NameDecision::ReplaceFailed`). Seen working live for ENG-3067 and
  ENG-3193.
- **Wording**: every health failure now says what is wrong, the likely cause
  and the next step in plain words, then a `Technical detail:` line with the
  check name and fields (`HealthFailure` Display, a3b0a37e; next steps made
  true per new/existing sandbox in the follow-up commit). No automatic retry
  added.
- **Measure**: setup failures per fork, by health check.

### Small UI problems seen

- Stale "the cloud record names sandbox … not adopted" notes stay on the task
  panel after Start has made a new sandbox.
- The task panel's upper scroll area is short: the runner buttons (Defer,
  Watch, Resume) are below its fold on a 1000 px window when a node has a long
  failure text.
- Stop texts stored before the rewording keep the old wording.
- `tod-sandbox exec ""` (a node with no sandbox) answers "not authorized
  (401)" instead of naming the missing sandbox.
- The task panel's layout shifts when "Reading GitHub…" clears, so a click
  aimed at a request can land on another control (here it opened Findings).
- Enter on a request that carries evidence (a finding link) opens the
  finding instead of the free-text answer field.
- "journey: prune failed: tod-agentd exited at start" in the log: the prune
  starts a second daemon while one is already running.

### Review "not converging" with nothing standing

- **Seen**: 8 nodes (ENG-2927 12 rounds, ENG-2157 10, ENG-2228 8, ENG-3095 8,
  ENG-3067 7, ENG-3193 7, ENG-2680 7, ENG-3093 5). ENG-2680 recorded one
  finding in total (fixed); ENG-2927 three (all fixed).
- **Symptom**: node stopped, "the code review ran N rounds of review and fix
  and the last review still found problems", with no finding listed.
- **Cause**: `Autopilot::own_review` stopped once
  `pr_draft_review::reviews_this_stay` reached 5. That counts every review in
  the stay, clean ones included, and a review is owed on every new PR head
  (merge of main, CI fix, answering a bot), so ordinary progress used up the
  rounds. The message claimed problems the stop list did not have.
- **Fix**: only consecutive unclean reviews count
  (`pr_draft_review::unconverged_rounds`); never stop with nothing standing.
- **Measure**: rounds per stay vs. findings per round; a stop whose standing
  list is empty.

### Replacement sandbox puts submodules on a stale branch

- **Seen**: 1 node (ENG-2923, 2026-11-10 16:09); latent for every replaced
  sandbox.
- **Symptom**: the reviewer asked the user to "fix the working directory": the
  flagship branch was at origin/main, the PR's commits missing.
- **Cause**: `prepare_sandbox_checkout` fetched only the superproject; the
  submodules (flagship, kb, ui) never learned `origin/<branch>`, so the branch
  was created at the pinned commit.
- **Fix**: each submodule fetches `origin/<branch>` before branching; a
  local branch behind it is fast-forwarded, a diverged one is an error
  (`worktree::sync_branch_with_origin`).
- **Measure**: a check after setup that each submodule's branch contains
  `origin/<branch>`.

### Node sandboxes vanish

- **Seen**: 11 sandboxes 2026-11-10 21:01–23:01 UTC; 32 earlier the same
  day (14:42 "forgot 32 sandboxes").
- **Symptom**: every waiting node's sandbox gone; each is rebuilt on next
  start (minutes each), losing anything not pushed.
- **Cause**: Cofactr's `Blaxel Prune` workflow (Cofactr/workspace,
  every 30 min) deletes any sandbox asleep over `SANDBOX_IDLE_TTL` (1h)
  unless labelled `keep-when-idle=true`; tod labelled node sandboxes
  `false` on purpose.
- **Fix** (2026-11-10): node sandboxes carry `keep-when-idle=true`; tod
  deletes them at `done` (`sandbox_retire`).
- **Reverted** (2026-11-11): only goldens and the orchestrator are kept;
  node sandboxes are `false` again, left to the prune and made again on the
  next start. Sandboxes made between the two keep `true` (relabeling resets
  a sandbox's container) until `sandbox_retire` deletes them.
- **Measure**: replacements per node per day.

### Agents in a sandbox cannot query the database

- **Seen**: 5+ requests to the user (remediate-stale-primary ×2,
  editing-a-linked-org-part, nightly-auto-dedup, retract-manufacturer), all
  `db-query: aws CLI not found`.
- **Cause**: the workspace repo tells agents to use its `db-query`, which
  needs the aws CLI and an SSO session; a sandbox has neither. tod's brokered
  `tod-cli db` was not in the phase agents' or evaluator's context.
- **Fix**: a `db-query` shim in every node sandbox routes `prod-platform` /
  `prod-kb` through `tod-cli db run` (read only, same limits); `cli/db` is in
  the phase and evaluate recipes.
- **Measure**: requests whose text contains a tool error, by error.

### Requests to the user are hard to read

- **Seen**: most requests (user's report).
- **Symptom**: dense run-on text, bare ids, unclear what to do, long options.
- **Fix**: "Wording a request" in `stance/autonomous-session.md` (imperative
  first line, background after, ids explained, short options with
  consequences, recommended first); app stop texts reworded.
- **Measure**: request length; answers that pick the first option.

### Bot review keeps finding new things (PR stuck)

- **Seen**: 1 node (ENG-3055, 4 rounds).
- **Symptom**: stopped `pr_stuck`; Greptile posted a new P1 on kb#2859 after
  the last push.
- **Cause**: real feedback; each fix round draws new bot findings.
- **Fix**: none needed in the app yet; rerun.
- **Measure**: bot findings per round; whether they shrink.

### Pruned sandbox stops a waiting node

- **Seen**: several live nodes (ENG-3093, ENG-2925).
- **Symptom**: a node waiting on GitHub (CI, a bot, a person) showed stopped,
  "its sandbox was deleted; Start makes a new one", until someone clicked Resume.
- **Cause**: the Blaxel Prune workflow deletes node sandboxes asleep over an
  hour (they are never kept awake on purpose). The orchestrator marks the node
  lost when a wake finds the sandbox gone and the app or daemon replaced it,
  but with no regard for whether the node was meant to run (a paused node got
  one too), and a replacement that failed was only logged and retried every
  ten minutes forever while the node read "a new one is made at the next sync".
- **Fix**: `cloud_sync::lost` replaces the sandbox lazily (only when a wake or
  feedback finds it gone) for a node whose run row is not `Stopped`/`Done`;
  a user-deleted sandbox is still left for Start. Three failed tries, ten
  minutes apart, record the node failed with the real error
  (`next_actor` shows that error instead of the loss). The status note says
  "pruned or deleted outside the app; made a new one". Commit: see `git log`
  for "Pruned sandbox".
- **Measure**: nodes showing "sandbox is gone/deleted" for longer than one sync;
  replacements per node; nodes failed by `could not be made after 3 tries`.

- **Follow-up (pruned sandbox, second pass)**: the first fix did not help the
  live nodes because `cloud-sync.json`'s `deleted_by_user` flag was set on 13
  non-done nodes, which kept them out of replacement. Cause: the flag was set by
  every `delete_with`, including the daemon's retirement of a done node's
  sandbox (`sandbox_retire`), and outlived the node's next run. Fix: only the
  user's explicit Delete sandbox (menu, delete anyway, stop and delete) sets
  it, bound to the sandbox it removed (`deleted_sandboxes`); retirement leaves
  none and a done node is never replaced. `lost::heal_deleted_flags` (every
  sync, in the app and the daemon) clears a flag whose row now names another
  sandbox, and a legacy flag (no sandbox name) on a node whose run row is
  starting/working/waiting. The replacement tries are persisted
  (`replace_tries`), kept until a sandbox is made so a failed record write is
  recorded again without another try, and the conversation driver's resend
  after a launch finds the sandbox gone shares the same bound
  (`allow_remake`).

- **Follow-up (pruned sandbox, third pass)**: after the flags healed, ten
  replacements went quiet for minutes after "forked and set up" and the lost
  mark stayed. The silence is the step after the fork: a replaced node's agent
  session logs (58 for one node) are copied in chunk by chunk through the relay,
  which logged nothing, so a slow copy looked like a hang. Fix: that copy says
  "sending session log n of N (KB)"; a replacement is done only when the lost
  mark is verified cleared in the database (`verify_recorded`, else an error),
  and says so ("replaced ... with ..."); a Start also resets the persisted
  replacement tries; the driver's remake counts a try only once it begins one
  (`can_remake` / `note_remake`); stale flags heal before the sync, which may
  fail; and the one-call submodule branching handles a submodule that already
  has the branch locally (every golden fork), moving it to origin's when
  behind, instead of declining on every fork. A5898BE2's row (sandbox
  `node-bom-upload-...`, lost since October, no Files location) is a legacy
  record no replacement is attempted for.

### A request stays pending after the fix it asked for (and on a finished node)

- **Seen**: 3 nodes at once (ENG-3152, ENG-2924, ENG-3358; `db-query: aws CLI not found`
  requests filed 2026-11-06/10, before the 2026-11-10 db-query shim), plus ENG-3043
  (`done` with its run state still "waiting on you"), 2026-11-12.
- **Symptom**: the node shows "working: repairing its cloud sandbox" and a pending request
  that no longer applies; the options say "AWS CLI installed and SSO logged in".
- **Cause**: pending decisions are never re-examined when a fix lands, and a node reaching
  `done` does not clear its run state's request.
- **Fix**: open. Answered by hand ("Fixed, try again"); after the restart ENG-3358 went on
  to query prod KB through the shim and asked a real product question.
- **Measure**: pending decisions older than the deploy that fixed their cause; `done`
  nodes with a pending decision.

### Open minor gaps in the memory-freeing script (bc49872f)

- An unreadable `/proc/<pid>/cmdline` is skipped silently; a process that exits between the
  scan and the kill reads as "could not stop"; an unreaped zombie reads as still running;
  `python -u -m uvicorn` or `node --max-old-space-size 4096 vite` do not match.

### `tod-cli db`: "owning machine not connected" while the machine was on

- **Seen**: ENG node 5631A571 (clean-up-dangling-manufacturer-stateme-2), verify agent in its
  sandbox, 2026-11-10 ~06:26Z (decision filed 1794291962980): "prod-kb was unreachable ...
  (tod-cli db: owning machine not connected)", then asked the user. Also node 8f80f1e0 on
  2026-11-06 (`... which is not connected` mid-`psql`).
- **Symptom**: the agent stops and asks the user about access plumbing although the app and
  `tod-agentd` were running.
- **Cause**: not a stale pin: the node's `owner_client` (`app-1597fe1579e4`) is the live data
  root's client id (`cloud-sync.json`). The orchestrator keeps `seen` (when each machine last
  polled) in memory only, and refused a request **at once** when the owner had not polled in
  the last 46 s. That window opens whenever the poll is away: an orchestrator redeploy or wake
  (`seen` is empty), the daemon's poll timing out (`orchestrator broker: timeout: global`,
  logged every ~16 min in `daemon/agentd.log`, e.g. 2026-11-10T05:44:42Z, just before 25
  sandboxes were replaced at 05:47) and retrying 2 s + 4 s, or a 502 outage. A request landing
  there got the error, and `tod-cli db` neither waited nor retried, and its docs told the agent
  to hand the user a request.
- **Fix**: the broker holds a request for an owner that is not connected for 60 s
  (`NOT_CONNECTED_GRACE`) before failing, so the daemon's next poll takes it; the failure is
  `retryable`, names the cause and says it is tod's to repair; `tod-cli db` asks again 3 more
  times (10 s apart), then fails with an error that says to report a failure, not ask the user;
  `cli/db.md` no longer tells the agent to file a request for this. Needs the orchestrator
  redeployed (the grace) and `tod-cli` rebuilt/installed into sandboxes (the retry).
- **Measure**: `db` errors containing "has not been connected" per week (target 0); requests
  that waited in the broker for a reconnect.

### Verification asks the user about obligations that act in the wrong phase

- **Seen**: 2 nodes in `verifying`, 2026-11-12 (digikey-productdetails-404s-50-of-look-2:
  requirements 75042c52 `released`, 4b52118d `design`; clean-up-dangling-manufacturer-stateme-2:
  33abe1af, eef15342, b8247541 default `active`).
- **Symptom**: the verify agent asked "How should the measurement be handled?" for obligations
  that are about production after the rollout / after the cleanup asset runs on prod.
- **Cause**: (a) the proposed/design/planning Done-when checklists and the evaluator never asked
  whether each obligation acts where it can be shown true, and design.md let a requirement act in
  `design` once refined; (b) verify.md told the agent to exercise every listed obligation, including
  ones acting in `merged`/`released`/`design` (the loop never owed them), and the app refused every
  agent obligation edit past `planning` with "ask the user", so a phase mistake could only become
  a question.
- **Fix**: the three role docs, `domain/obligations.md` and the evaluator state the rule and reject
  a mistake; a verification conversation may move an obligation of its node to `merged`/`released`
  itself in `verifying` (`settled::verify_may_retag`, only that, only later); that move does not
  count as a plan change (`lifecycle_validity`); the Verify loop requires a plan step of that phase
  for a requirement moved there (`untraced_for_later_phase`); verify.md and verifying.md give the
  rule; autonomous-session.md's real bad example names it a bug in the app. No keyword check was
  added: wording does not decide the phase. Hardened after review: the retag is limited to the
  conversation's own node, needs a `--reason` kept in the change set, is refused for an obligation
  with a `failed` verdict or a `failed` carrying step, and the trace check covers constraints and
  needs a step of that phase or later. A `failed` verdict anywhere in the history blocks the
  retag, and the reason needs at least 15 non-space characters. Known limit: a failed plan step
  flipped to `verified` by the agent, then the obligation moved, is not caught (adversarial path,
  out of scope).
- **Measure**: `decisions` from a `verification` protocol whose evidence is an obligation acting in
  `merged`/`released`/`design` or about production; obligations moved by a verification conversation
  with no later-phase step; nodes sent back from `verifying` for an acts-in change.

## 2026-11-12: open gaps in the supervisor's submodule push (721f1161)

- **Seen**: found by an independent review of the fix for ENG-2068's lost kb work; none hit a live node yet.
- **Open**: (1) in a parent with no origin, a submodule commit the parent already records counts as the base, so it is not reported (only matters without a remote); (2) `let outcome = run?;` returns before the run-end push check, so a push failure is dropped when the run itself failed; (3) a parent with an origin but no `origin/main` ref may report a healthy detached submodule as unpushed (not reproduced); (4) the boundary-push test also passes on the old code, so it guards the wiring only loosely.
- **Measure**: count of runs ending with a push failure named in their error.

## 2026-11-12: supervisor push failed on a submodule sitting at an upstream commit

- **Seen**: nodes `clean-up-dangling-manufacturer-stateme-2` and `data-fix-repoint-org-parts-on-deprecat-2` stopped `failed`: "submodule infra: it has commits of the node's own no remote has, but nothing was pushed (detached HEAD or no origin)".
- **Symptom**: a submodule detached at an upstream commit of its default branch (c8623f6) was reported as holding the node's own work.
- **Cause**: `has_unpushed` (`tod-supervisor/src/git.rs`) counted commits on no remote-tracking ref, but the submodule's `origin/main` was never refreshed (tod fetches without recursing), so an upstream commit looked like the node's. Not yet fixed: `tod_core::lost_work::state_of` compares against the same kind of possibly stale refs without fetching; it was left because that check inspects checkouts and says to fetch origin.
- **Fix**: `has_unpushed` fetches `origin` (no submodule recursion) before deciding; a failed fetch is an error naming the repository, no fallback. Own commits on a detached HEAD still fail. Tests: `an_upstream_commit_behind_a_stale_remote_ref_is_not_the_nodes_own`, `an_own_commit_on_a_detached_head_still_fails_after_the_refresh`, `an_unreachable_origin_is_an_error_naming_the_repository`.
- **Measure**: runs ending with "no remote has, but nothing was pushed" for a submodule whose HEAD is an ancestor of its origin's default branch (should be zero); runs failing with "refresh the remote refs".

## 2026-11-12: lost kb work not reopened, and a delete that could take unpushed commits (ENG-2287)

- **Seen**: ENG-2287 in `verifying`: the verify agent asked the user "The DigiKey 404 fix is missing from the kb checkout: restore the branch work." Its sandbox was new (fresh clone, kb on `task/ENG-2287…` at `origin/main`), the branch was never pushed.
- **Symptom**: the lost-work rule (deb5524e, 3ffd4575, 1d6bfb92) was in the running supervisor and looked again at the `broken` question, but found nothing lost, so the user was asked.
- **Cause**: two separate things. (1) *Why the rule stayed silent*: a sandbox forked from a golden whose submodules sit on their default branch, ahead of the commits the checkout records, made `git status` in the checkout show ` M kb`, ` M flagship`, ... (counted as uncommitted work: `Dirty`), and each submodule was compared with its *pinned* commit only, so flagship counted 51 commits "beyond" the pin though all are on `origin/main`; a shallow submodule (infra) also showed its HEAD, the parent of its shallow `origin/main`, as one commit beyond it. Any of the three kept `is_lost` false. (2) *How the work was lost*: the sandbox was deleted outside this repo (Cofactr's hourly `Blaxel Prune` of idle sandboxes, deliberate; tod then logs "sandbox is gone; replacing it", 2026-11-12 18:24Z). The supervisor pushed only the parent's branch then; pushing every submodule's branch at step boundaries landed at 22:11Z (141f897e), after the prune. tod's own delete (runner menu **Delete sandbox**, **Delete sandbox without a checkpoint**, and the retire at `done`) checkpointed only *uncommitted* work: a clean tree with committed, unpushed commits was `Clean`, then deleted. The external prune cannot be guarded from here; this path could.
- **Fix**: `lost_work`: a checkout's status ignores submodule pointer changes (each submodule is inspected on its own); a submodule's own work is what neither its pinned commit nor its default branch has; a shallow boundary's hidden parents count as the base's. `cloud_sync::lost::delete_with` (the one deletion path: menu, stop-and-delete, retire) first calls `provision::ensure_work_pushed`: commits no remote has (HEAD or any local branch, not on any remote-tracking ref, in the checkout and every submodule) are pushed, and what is still unpushed refuses the delete with an error naming the repository, the count and the newest subjects. It runs also for the delete that skips the checkpoint; a repository with no origin, or any git failure, is an error. Reset environment deletes nothing.
- **Follow-up (every other delete)**: a review found paths that still bypassed it, and `remove_location` / the credential reinstall only pushed the *current* branch (a detached submodule or a non-current local branch lost its commits without error). All now go through one function, `provision::secure_sandbox_commits` (Files location removal, recreate-for-credentials (also when forced: commits are never given up, only uncommitted files, and a failure is an error, no longer a confirmation), the old-style-sandbox upgrade, the dead-sandbox removal, the menu delete via `ensure_work_pushed`, and orphan deletes in `delete_orphans_via`, which skips an orphan with work or that cannot be looked at, with an error naming it). It pushes, then refuses naming repository, count and newest subjects; it never invents a branch name, so commits on a detached HEAD or a branch that is not checked out are refused for the user to push. A sandbox Blaxel reports dead (failed, terminated, deleting, expired) cannot run git, so it is the one case that passes without inspection (its work went when it died); every other failure to look is an error. It takes no node lock (callers holding it, like `remove_location`, call it directly; `ensure_work_pushed`, the only locking wrapper, is called only from `delete_with`, which holds none, so no deadlock). Tests: `the_shared_guard_*` (provision), `an_orphan_holding_commits_no_remote_has_is_skipped_with_an_error_not_deleted`. Not covered: an end-to-end test of `BlaxelBackend` / the Blaxel-calling callers (they need a real sandbox); the guard they share is tested on local repositories.
- **Follow-up 2 (the rule is structural; five gaps)**: `Sandboxes::delete` now takes a `sandbox::Cleared`, which only `secure_sandbox_commits` / `ensure_work_pushed` can return, and `delete_cleared` is the only other node-sandbox delete, so a new caller cannot skip the guard (`no_node_sandbox_is_deleted_without_the_guard` also fails on a raw `bx.delete(` in `provision.rs`; the one exception, `Sandboxes::delete_just_made`, is a sandbox the same call forked or created that failed its own setup, used by create-failure cleanup and `discard_failed_sandbox`; goldens and `tod-sandbox delete` are not node sandboxes). Gaps closed: (1) `choose_sandbox`'s `ReplaceFailed` (a live, labelled, unrecorded sandbox) and `ReplaceDead` now run the guard (probing the sandbox for its checkouts) before deleting; (2) `refresh_sandbox_proxy` uses the erroring `location_dir`, so an empty-`repo` location is refused instead of skipping the dirty check and the guard; (3) EXPIRING is still running, so `is_past_inspection` (dead states minus EXPIRING) is the only pass-uninspected test and an expiring sandbox is inspected, or refused if it cannot be; (4) with no location, `find_sandbox_checkouts` returns every checkout (`/workspace/repo` and `/workspace`) and all are inspected, instead of the probe's single guess; (5) the guard passes a sandbox that vanished mid-look only when the error names *that* sandbox (`says_sandbox_gone`), not on any quoted "is TERMINATED". Tests: `an_expiring_sandbox_is_inspected_not_passed`, `a_live_unrecorded_sandbox_with_unpushed_commits_is_refused_not_replaced`, `a_sandbox_location_with_an_empty_repo_is_refused`, `every_checkout_of_a_sandbox_without_a_location_is_inspected`, `only_this_sandbox_being_gone_passes_the_guard`. Not covered: the `choose_sandbox` / `refresh_sandbox_proxy` wiring end to end (needs Blaxel); the type requirement and the source test guard it. `sandbox_is_gone` (unnamed) remains for the checkpoint and launch paths, which delete nothing.
- **Not fixed**: the external prune itself, and work lost before the supervisor pushed submodules (like this node's, which the corrected lost-work check now reopens). The "sandbox is gone; replacing it" path still only leaves an in-memory note; it could record a node-timeline note when the node had done steps and its branch was on no remote.
- **Measure**: nodes in `verifying`/`review`/`pr` with a pending `broken` request about missing work (should be 0: reopened instead); `Delete sandbox` failures naming "no remote has"; "sandbox is gone; replacing it" for nodes whose branch is on no remote.

## 2026-11-12: the new git checks stopped healthy nodes (shallow boundary absent; narrowed fetch refspec)

- **Seen**: ~11 live nodes stopped after the lost-work and unpushed-commit checks deployed. Ten (review/verifying) failed "git merge-base --is-ancestor 290b69ed… refs/remotes/origin/main in /workspace failed: Not a valid commit name"; `supplier-offers-show-when-async-refres-2` (merged) failed "submodule flagship: refresh the remote refs … couldn't find remote ref refs/heads/task/ENG-2680…".
- **Cause**: (A) `lost_work::hidden_parents` probed every commit named in `.git/shallow` with `merge-base --is-ancestor`, but a fork's shallow file can name a boundary whose object the repository does not hold; git then fails instead of answering 0/1. (B) `has_unpushed` ran a bare `git fetch origin`, which follows the submodule's configured `remote.origin.fetch`; that was narrowed to the node's branch, not yet on origin.
- **Fix**: (A) a boundary that is absent (`cat-file -e <sha>^{commit}` fails) cannot hide anything and is skipped; a failure of the probe on a present commit is still an error. (B) every fetch the checks make for "all remote refs" (`has_unpushed`, `ensure_commit`, provision's pinned-commit fetch) names `+refs/heads/*:refs/remotes/origin/*`; an unreachable origin is still an error. `lost_work` itself does not fetch; the other fetches (`worktree.rs`) already use explicit refspecs.
- **Decision**: a failure of the lost-work check still fails the run. It guards reopening and deleting work; skipping it on error would let a node proceed with its work unproven, which "errors are never hidden" forbids. The two causes are fixed instead, so only a real git failure stops a node, with its message.
- **Tests**: `a_shallow_boundary_absent_from_the_repository_hides_nothing` (tod-core), `a_fetch_refspec_naming_a_branch_missing_on_origin_is_not_an_error` (tod-supervisor).
- **Measure**: runs failing with "Not a valid commit name" or "couldn't find remote ref" (should be 0).

## 2026-11-13: review-bot credits spent on superseded heads; a silent bot always stopped the node

- **Seen**: Greptile credit limits; flagship#6640 went silent, kb#2881 answered an ask with "run out of review credits".
- **Symptom**: asks were spent on heads a later push replaced, and a bot that could not review stopped each node on a request to the user although tod's own review was clean.
- **Cause**: asks were due as soon as a head was overdue, before tod's own review of it; the only way out of a silent bot was the user's decision.
- **Fix**: with the review steps on, a bot is asked only once tod's own review of that head is clean (`Wait::OwnReview` first); the PR agent never asks and prompts name no bot. Per-bot `silent_after_minutes`, `fallback: own_review` and `exhausted_pattern` (all unset by default) waive the bot per head, visibly (timeline note and status line); a bot without `fallback` keeps the decision. Everything through `settings_for_node`.
- **Tests**: pr_readiness `a_bot_is_not_asked_before_tods_own_review_of_the_head_is_clean`, `a_head_a_later_push_superseded_is_never_asked`, `a_silent_bot_with_a_fallback_*`, `a_bot_without_a_fallback_still_raises_the_question`, `a_waiver_is_for_the_head_it_was_made_on`, `a_bot_that_says_it_is_out_of_credits_is_not_waited_for`; autopilot `a_bot_is_asked_only_after_tods_own_review_of_the_head_is_clean`, `a_silent_bot_with_a_fallback_is_given_up_on_visibly_without_asking_the_user`; tod-store `a_bot_fallback_parses_defaults_to_none_and_is_validated`.
- **Follow-up (verifier)**: in `pr_review` the own review is not rerun, so only the heads the gate's review covered count as cleared (a head pushed there is neither asked nor waived); invalid settings are an error at every reader (`settings_at`, the gate's `readiness_settings`), not "no bots"; `exhausted_pattern` is a compiled `ExhaustedPattern` (a bad or empty one fails the load); a missing head commit is an error. Tests: `a_head_pushed_in_pr_review_is_neither_asked_nor_waived`, `a_missing_head_commit_is_an_error_*`, `settings_that_fail_validation_are_an_error_not_no_bots`. `owed` and `cleared_shas` agree on heads and on `round_clean`; `cleared_shas` is stricter only while an open finding is carried by an open thread, which is the agent's work (a turn, not a wait). node_timeline is deliberately not in `journey_changes`, so no journey record.
- **Measure**: asks per PR (target 1 per final head); `bot_silence` requests for bots with a fallback (target 0); timeline notes "relying on tod's own review".

## 2026-11-12: no CLI way to move a node's lifecycle state (ENG-2068, ENG-2287)

- **Seen**: 1, ENG-2068 and ENG-2287, 2026-11-12. The shepherd had to move both nodes from `review` back to `active` after their work was lost, by clicking the task panel's state chip over the app's control socket; the clicks kept landing on the wrong rows.
- **Symptom**: a supervising agent had no reliable way to change a node's state; a wrong click moved a node to the wrong state.
- **Cause**: the chip (`TaskPanel::set_phase` -> `lifecycle::set_lifecycle_by_user`) was the only route; `tod-cli` has no lifecycle verb, on purpose, since nothing but the lifecycle process may set a state.
- **Fix**: `tod-cli lifecycle set <NODE> <STATE> --reason <TEXT>`, for the operator only. It sends the same store mutation as the chip (`tod_core::lifecycle::operator::user_move`, which `set_lifecycle_by_user` now uses too), so a move back clears certifications, reopens verified verdicts, cancels waits and retakes/clears the baseline, and shows on the node's activity as the user's, with the reason (`PhaseChange.reason`). It is refused in an agent session (`TOD_INTERVIEW_ACTOR`, `TOD_IMPLEMENT_NODE` or `TOD_IMPLEMENT_CONVERSATION` set), into `pr_review` or any later state, into the current state, and without `--reason`. Tests: `review_to_active_has_the_effects_of_the_chip`, `pr_review_is_refused_and_nothing_moves`, `an_agent_session_is_refused_and_nothing_moves`, `a_reason_is_required` (tod-cli), `operator::tests::*` (tod-core). Not done: the chip itself still allows `pr_review` and later (it is the user's own click); the CLI does not push the Linear ticket's state or record a `UserAction` journey event (the journey's change feed still records the transition).
- **Measure**: moves of a node's state made by clicking the chip over the control socket (should be 0 for shepherd runs); `tod-cli lifecycle set` refusals from an agent session (should be 0).

## 2026-11-13: four stopped nodes (ENG-2594, ENG-3067, ENG-2552, ENG-3129)

- **Seen**: 4 nodes stopped on the live root.
- **ENG-3067 (`pr_stuck`)**
  - *Symptom*: stopped after two turns that "changed nothing", with all checks green and no open thread.
  - *Cause*: a `missing migrations` run was cancelled by a newer run while its replacement (and other checks) were still running. `combine_checks` read any completed non-success run, `cancelled` included, as `failure`, and `failure` outranks `pending`, so the assessment said "a check is failing" (`Work::FailingChecks`, which the agent can fix). The agent could only say the run was cancelled and re-run it; twice.
  - *Fix*: a cancelled run is pending while anything else runs, and a failure only when nothing replaces it (tod-store `github::combine_checks`). Test `a_cancelled_run_is_pending_while_others_run_and_a_failure_only_when_nothing_replaces_it`.
  - *Measure*: `pr_stuck` stops whose last agent turns say "cancelled" or "re-ran the failed job" (target 0).
- **ENG-2594 (`no_progress` in `released`)**
  - *Symptom*: after the user answered the post-release decision with "wait for the KB deploy", the released agent's next turn ended "I'll resume when the background watcher reports", changing nothing; the run stopped. The deploy had succeeded by then.
  - *Cause*: nothing wakes a finished session; the prompt said to ask and end the turn but not that a chosen wait is done inside the turn.
  - *Fix*: `released.md` step 7 (wait by polling inside the turn, never end promising to resume; ask again if too long). Test `prompts_place_post_merge_steps_in_later_phases_and_wait_inside_the_turn`.
  - *Measure*: `no_progress` stops in `released`/`merged` whose last turn promises to resume (target 0).
- **ENG-2552, ENG-3129 (`request`: steps handed back)**
  - *Symptom*: steps blocked as `access`: QA spike, QA apply/demonstration (needs AWS/SSM and Terraform in QA), and PG 18 suite runs (the sandbox's db is PG 17, no docker, no apt network).
  - *Cause*: the QA apply and demonstration only exist after merge, and a PG 18 test run is what the pull request's CI does once the pin lands; both were put in `active` and blocked, and `implement.md` said nothing about moving or delegating them.
  - *Fix*: `implement.md` "Leaving work for the user": before `access`, move a post-merge step to `merged`/`released` with `plan update --phase`, and for CI-only evidence build what makes CI run it and let the PR's checks decide; `access` only for access nothing later gets, naming which in `--needs`. Same test as above. Not fixed in code: tod cannot grant AWS/QA access (a user call for the one genuinely pre-merge spike).
  - *Measure*: steps blocked `access` whose `needs` names QA/prod/apply/deploy or a server version (target 0).
## 2026-11-13: a temporary Blaxel/network failure while making a node's sandbox stopped the node (ENG-3038)

- **Seen**: 1, ENG-3038: run_state failed "starting in the cloud failed: tod-remediate-duplicate-org-parts-created-594afc was never made (Blaxel has no sandbox of that name) ... This looks temporary ... fork ... 502 Bad Gateway ... context deadline exceeded (FORK_FAILED)". It sat failed until a person clicked Resume, which worked.
- **Symptom**: tod itself classified the failure as temporary, yet stopped and asked a person.
- **Cause**: `provision::make_sandbox` (the one creator, reached from start, resume, migrate, reset environment, replacing a pruned sandbox and `cloud_sync` through `resolve_launch_cwd_with`) cleaned up after a temporary failure (`discard_failed_sandbox`) and returned the error, with no retry. Only the orchestrator feed retried 502s.
- **Fix**: `make_sandbox` retries (`retry_transient`, delays 20s, 60s, 120s: four attempts) when the attempt failed with `blaxel::error_is_transient` (the classification the feed retries use) and cleaned up (half-made sandbox deleted, location forgotten, intent cleared): `Attempt::retryable`. A kept sandbox, a failed delete or check, or an unknown cause is never retried, so nothing is double-created, and a name Blaxel has that the location does not record is still the "lost track" error on the next attempt. Each retry is said in the progress; the final error says how many attempts were made.
- **Tests**: `a_temporary_failure_that_cleaned_up_is_retried_then_stops_with_the_error`, `a_failure_that_is_not_retryable_is_never_retried` (the retry loop with a fake attempt; the Blaxel calls themselves need a real sandbox and are not covered).
- **Follow-up (verifier)**: ENG-3038's error was the `node::fork_golden` call (context "fork <name> from <golden>"), covered by the first fix. Now also retried: temporary failures before anything is made (`orchestrator_of`, `choose_sandbox`, `node_proxy_credentials`, `current_golden`, `bx.get(golden)`, `runtime_image`; `before_anything_is_made`), a bare "context deadline exceeded" (`text_is_transient`), and after a transient failure the existence check is repeated up to 3 times, 3s apart, before a sandbox is declared absent. Tests: `a_temporary_failure_before_anything_is_made_is_retryable`, `a_bare_deadline_error_is_transient`. A `make_sandbox`-level test needs a fake Blaxel client and sandbox host (no seam; too costly), so the loop and the classification are tested separately. `golden::retry_step` does not wrap the golden's own fork; a failure there now retries the whole attempt instead.
- **Measure**: nodes failed with "This looks temporary" from a sandbox creation (should be 0 unless Blaxel is down for several minutes); progress lines "making the sandbox failed temporarily".


## 2026-11-13: a done node still asked for an answer (ENG-3043)

- **Seen**: 1, ENG-3043, `done` for 4 hours; its run line still read "the agent asked you 1 question(s) and is waiting for your answer".
- **Symptom**: a finished node showed a pending request to the user.
- **Cause**: the supervisor's last `run_state` row (`waiting`/`request`) outlived the move to `done`, and nothing closed a node's pending decisions or settled its run state when it got there.
- **Fix**: `tod_store::done_close::close`, called from `NodeRepo::set_lifecycle_by` (the one place the gate's advance, the state chip, `tod-cli lifecycle set` and the supervisor mirror all pass through), withdraws the node's pending decisions and sets a stale run state to `done`. `done_close::sweep` does the same for rows an older build left or a late sync wrote; `tod-agentd` runs it each minute with the sandbox-retire pass (`InterviewCommand::CloseDoneNodes`). Decisions have no withdrawal-reason column (no migration); `withdrawn` plus the run state text "the node reached done; its open requests were withdrawn" is the record.
- **Tests**: `done_close::tests::*` (tod-store): reaching done, the chip path, a node not done keeps its requests, the sweep.
- **Measure**: pending decisions or non-`done` run states on `done` nodes (should be 0).

## 2026-11-13: the verifier asked the user to retag three prod-data requirements (ENG-2068)

- **Seen**: 1, ENG-2068 in `verifying`: "Decide whether to move three prod-data requirements to act in the merged phase."
- **Symptom**: the agent tried `obligations update --acts-in merged`, was refused, and asked the user instead.
- **Cause**: the retag refused any obligation with a `failed` verdict anywhere in its history. These three had been failed in the previous stay because the code was missing; the node then went back to implementation, the code now exists, and the stale verdicts blocked a correct move.
- **Fix**: only a `failed` verdict recorded in the current stay in `verifying` (or a `failed` plan step) blocks the retag (`settled::verify_may_retag`); one from an earlier stay does not. The verify surface and `cli/obligations.md` say so and tell the agent to move it and note why, never to ask.
- **Tests**: `a_failure_from_an_earlier_stay_does_not_block_the_move`; the existing `a_failed_obligation_is_not_moved_to_a_later_phase` still holds for this-stay failures.
- **Measure**: `missing_rule` requests from verification about an obligation's phase (target 0).

## 2026-11-13: an agent asked the user to install terraform (ENG-2072)

- **Seen**: 1, ENG-2072: "Install terraform in the sandbox, then pick Fixed, try again."
- **Symptom**: the user was asked to do an install the agent could do.
- **Cause**: asked before `stance/autonomous-session.md` carried "A tool you lack, you install" (5f7cb589); the section also did not cover a non-root sandbox user.
- **Fix**: the section now names `~/.local/bin` for a non-root user and says an "Install X, then try again" request is never the form: the agent installs X. The repository pins versions in `.terraform-version`/`.tool-versions`/CI config, which the agent reads; `assets/sandbox/bootstrap.sh` is left as is (no per-repository tool list: the golden sandbox, `tod-sandbox golden refresh`, is where a repository's tools are baked).
- **Measure**: `broken` requests whose text is a missing tool (target 0 unless the install itself failed).
