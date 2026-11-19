# Shepherd reviews

## Lessons from runs

The manager adds here what it, or a shepherd in its reply, learned mid-run
that the skills should have told them (nobody edits a shepherd skill during a
run). Older entries came from the single-session `shepherd-nodes` skill. Each review folds these into
the skill and clears the ones it applied.

- (2026-11-18, cli-7) `tod-cli` is not on PATH; the right one is the main checkout's `target/dev-fast/tod-cli` (matches the deployed daemon), not `target/debug/` (stale). Shepherd briefs should name it.
- (2026-11-18, cli-7) `tod-cli decisions list` takes `--node <slug>`, not a positional.
- (2026-11-18, cli-7) There is no read-only ledger lookup of one node's id/fingerprint; `next` leases as a side effect. Add a `show <node>` command.
- (2026-11-18, cli-7) Read-only sqlite on tod.db: quote `"file:<root>/tod.db?mode=ro"`; node ids are blobs, match with `lower(hex(node_id))`.
- (2026-11-18, cli-7) A shepherd retired after one pass when all its nodes were parked/rechecked, without lingering: by design it should linger `linger_minutes` and absorb due nodes. Check whether retiring early was right.
- (2026-11-18, manager) A new manager found the previous run's shepherds signalled `retire`, silent ~7 min, still leasing 20 nodes; it released them by hand. The skill should say how to take over a dead manager's run.
- (2026-11-18, ui-2, cli-7) Both retired right after one pass instead of lingering `linger_minutes`; check the retire condition.
- (2026-11-18, ui-2) The user's shell is zsh: `P="python3 -I ..."; $P` doesn't word-split; use a function. Say so in the shared reference.
- (2026-11-18, ui-2) Task tree text search shows a node up to 3 times, so `listitem:ENG-xxxx` is ambiguous; and nodes under the folded "Cloud ENG tasks" group aren't in the tree until searched.
- (2026-11-18, ui-2) A stopped node that also has a pending request: Resume first is right (the request still waits); the skill should say so.
- (2026-11-18, manager/ui-2) When the user narrows scope mid-run, a shepherd may act on a dropped node before the message lands (ui-2 resumed ENG-3096). Shepherds should `check` scope right before each app action; the manager should message shepherds before `scope remove`.
- (2026-11-18, cli-8) Don't `find /` for tod-cli (times out); name `target/dev-fast/tod-cli` in the brief.
- (2026-11-18, cli-8) `survey.py --save` without a file argument crashes; `tod-cli --node` wants a hyphenated UUID, not a slug (and tod.db `nodes.id` is a blob).
- (2026-11-18, cli-8) `decisions list` without `--all` hides withdrawn requests, so re-asks (gap 48) are invisible; shepherds should use `--all` when a request looks new.
- (2026-11-18, cli-8) `L next --n K` leases nodes without listing them where the shepherd looks; check `L owners`.
- (2026-11-18, cli-5) A wrapper that pipes `L check` through grep loses its exit code, so `check ... && act` acted after a refusal. Never pipe `check`.
- (2026-11-18, cli-5) After a verdict the lease is gone; `ack`/`check` fail `lease_not_held` until `next` re-claims.
- (2026-11-18, cli-5) Start/resume several local nodes one at a time (gap 50, worktree lock) until gap 50 is fixed.
- (2026-11-18, cli-5) A node "not started" while its PR waits on a bot/ready event is not parked: `runner start` it. Earlier verdicts got this wrong.
- (2026-11-18, cli-5) Use `tod-cli journey show <node> --since 6h` to see the user's own recent actions before resuming a paused/stopped node.
- (2026-11-18, all) 5 of 5 shepherds retired after one pass without lingering (cli-5, -6, -7, -8, ui-2).
- (2026-11-18, cli-9) Fingerprint churn: the "saving the branch to GitHub" run-state text flips a working node's fingerprint repeatedly (cli-9: 16 visits, 14 empty). Exclude it from the fingerprint like `waiting_outside` text.
- (2026-11-18, cli-9) Run `L gap list --text` before `gap report` to reuse a signature; cli-9 filed duplicate gap 55 (of 50/51).
- (2026-11-18, cli-9) `journey show` fails ("no journey recorded") when journeys are off on the root, so the "check journey before resuming" note is useless here; say how to see the user's recent actions otherwise.
- (2026-11-18, manager) The manager's Edit tool is denied in the main checkout, so it cannot append here there; it wrote these in its own worktree's copy (branch js/shepherd-manager-devcontainer-node-1c89a5). The skill should say where lessons go.
- (2026-11-18, cli-10) `survey.py` truncates the run reason at 90 chars, and there is no `tod-cli runner status` verb to read the full failure text. Add one.
- (2026-11-18, cli-10) `ledger wait` keeps returning `due_available` for parked pr_draft nodes, because the daemon flips the run between waiting and "running on this machine" and that changes the fingerprint. Ignore the flip, or recheck less often for webhook-backed waits. cli-10 had 17 of 21 visits empty.
- (2026-11-18, cli-10) Before parking a pr_draft node, recheck its PR checks: one can still be pending, with an empty conclusion, right after a push.
- (2026-11-18, cli-10) The ledger gives no hint that a user's pause was deliberate. cli-10 resumed ENG-2157, which the user had paused.
- (2026-11-18, manager) cli-10 answered ENG-3311's recovery request while the manager had told the user to hold it for the gap 57 fixer. Shepherds should not answer a request on a node whose gap is being fixed, or the manager must `L hold` it.
- (2026-11-18, manager) The manager wait kept exiting `unclaimed_due` for parked pr_draft nodes (ENG-3413, ENG-3419, ENG-2927) whose fingerprint flips with the daemon's waiting/running toggle. For the session the manager raised min_batch to 6 and the queue delay to 30 min instead of dispatching shepherds to churn. Fix the fingerprint.

- (2026-11-18, cli-13) The full run reason is in tod.db `run_state.reason_text`, read-only. Name it in the shared reference until survey/tod-cli can print it whole.
- (2026-11-18, cli-13) zsh expands `==` in `echo == $p` ("= not found"); quote such strings.
- (2026-11-18, cli-13) A shepherd holding only parked nodes can't linger: the fingerprint churn wakes `wait` at once. cli-13 retired after 5 visits, 3 of them empty.
- (2026-11-18, cli-14) A bare ticket often matches 2-3 nodes, so `L verdict ENG-xxxx` errors; briefs should give slugs for ledger calls.
- (2026-11-18, cli-14) `wait` can hand back a node whose last verdict was another owner's; run `next` to lease it before the verdict.
- (2026-11-18, manager) Shepherds now retire within minutes (cli-12/13/14: 6-8 visits, half empty): the parked pr_draft churn means the linger is never quiet. Fixing the fingerprint (ignore "saving the branch to GitHub" / waiting<->working flips) is the top shepherd-tooling change.
- (2026-11-18, cli-15) `runner check-now` blocks until the check ends (up to 10 min): run it in the background or with a long timeout.
- (2026-11-18, cli-15) `L release` says `not_leased` for nodes that already have a verdict (the verdict released them); harmless.
- (2026-11-18, cli-15, cli-16) The user's zsh prints a harmless `.zshenv` error about `~/.vite-plus/env` on every command; say so in the reference.
- (2026-11-18, cli-15, cli-16) Both retired after one pass with every node parked or in motion, without lingering: the pattern from cli-5..14 continues.
- (2026-11-18, cli-18) When `wait` says `due_available` but the shepherd already holds `nodes_per_shepherd`, `next` can't claim; `wait` returns instantly forever. Say to retire (or re-wait) instead of looping.
- (2026-11-18, cli-19) Recording verdicts on every held node releases them, so the next `next --n 5` claims fresh due nodes; cli-19 then retired holding 3 it never visited (manager released them). Retire right after the last verdict, or release first.
- (2026-11-18, cli-19, cli-21) zsh loops break `L check --fingerprint` and `survey.py` slug args (`set -- $p`); `tail -n +2` drops the only output line (the .zshenv error is line 1). Use explicit args.
- (2026-11-18, cli-20) A `declare -A` loop tripped a false 'dangerous rm' safety check; plain `for` over slugs works.
- (2026-11-18, manager) After deploy 3, sync made all 29 nodes due (shepherd version change). 4 of 5 shepherds re-parked in under 1 min each with no action: a version bump should not re-queue nodes whose verdict waits on a user step (ready_for_review, a user request).
- (2026-11-18, cli-22) `gap report` takes a signature but `verdict --gap` takes the numeric id; say so.
- (2026-11-18, cli-22) `runner start` on a node whose run holds only a stale request wait acts as a Resume (gap 67).
- (2026-11-18, cli-17) A mistyped UUID for a `runner` verb resolves to some 'not started' node and is refused, not an error; derive UUIDs from `nodes.id` hex. `capabilities list` omits Files when the node only inherits it: that is how to tell an inheriting node from a pinned override.
- (2026-11-18, cli-23) A one-node brief plus `next --n 5` leased 4 extra nodes; the brief should say `--n 1`. `L release` with a ticket shared by several nodes fails ("matches 3 nodes"); use slugs.
- (2026-11-18, cli-23) No CLI way to confirm a node got its worktree and its agent is active (runner menu/survey show only run state); and the skill doesn't say which of resume / resume-fresh-budget / run-fresh to pick after a setup failure stopped the run.
- (2026-11-18, cli-24) `L gap report` has no dry-run: cli-24 filed junk gap 70 ('x') probing for a signature. Use `gap list --text` to match first.
- (2026-11-18, cli-25) A stale worktree shows as `node_files_locations.source_node_id` differing from the Files-owning node; `reset-environment` works while the run waits outside, so fix it before the babysitter needs files. Survey each node before acting on a brief's action list (nodes move on).
- (2026-11-18, cli-26) `survey.py`/`L check` refuse a ticket shared by several nodes; `next`'s compact view can omit slugs; `source_node_id` is a BLOB (compare with lower(hex())). Briefs should give slugs and hyphenated UUIDs.
- (2026-11-18, manager) cli-26 resumed ENG-3421 assuming gap 62's deploy fixed it; gap 62 only adds the per-check Setup field, which the user still has to fill. Briefs should say when a fix needs a user's configuration before it helps.
- (2026-11-18, cli-27) A request withdrawn after a deploy ("Request re-examined after a deploy") is invisible to `decisions list` without `--all`; tod.db `node_timeline` (node_id, kind, title, detail, at) is the quickest place to read why a node stopped. Name it in the reference.
- (2026-11-18, cli-27) Retired after one pass with every node verdicted, without lingering (pattern continues).
- (2026-11-18, manager) Deploy 8 pulled a one-line operator-cli.md change, which bumped the shepherd version and made all 29 nodes due (5 shepherds started for mostly no-op re-parks). Exclude reference docs like operator-cli.md from the version hash, or don't re-queue on a version change unless the shepherd skill/agent itself changed.
- (2026-11-18, cli-32) `L verdict … in_motion` needs `--recheck-minutes`; a zsh variable holding it expanded to nothing. zsh `set -- $p` loops fail again; use `while read` over a heredoc. `L wait due_available` lists nodes without leasing them.
- (2026-11-18, cli-31) shepherd-cli skill says `survey.py --save` but it needs a file argument (crashes otherwise); fix the skill text.
- (2026-11-18, cli-30) `next`/survey show a PR number without its repo; shepherds guess (kb#2888 is not flagship). Print owner/repo#n.
- (2026-11-18, cli-34) Returned a literal 'placeholder' report after 6 visits (all empty, all re-parked); the manager had to read the ledger to learn what it did. The agent definition should require a per-node summary in the hand-back.
- (2026-11-18, cli-37) `L verdict` accepts `--recheck-minutes` only after `--no-action`/`--acted`; the skill's usage line doesn't show the order.
- (2026-11-18, manager) After deploy 9, all 29 nodes came due on the version bump again; 2 of 5 shepherds re-parked everything in under 30 s with no action (same as deploys 3 and 8).
- (2026-11-18, cli-35, cli-36) Neither ran `L gap list` before finishing; 4 of 5 shepherds (cli-34..37) retired after one no-action pass. survey's pr_draft line again lacked the PR's repo (cli-35 probed repos for #2888).
- (2026-11-19, manager) `manager-wait --queue-delay-minutes 60` timed out 3 times (50 min each) with 2 nodes due 150-200 min and no shepherd running, never returning `unclaimed_due`; the delay seems measured per wait run, not from when the node came due. The manager dispatched by hand.
- (2026-11-19, cli-40) The skill should say: use `target/dev-fast/tod-cli` (built with the daemon); `target/debug/tod-cli` is stale and nothing is on PATH. Under zsh a command prefix held in a variable (`T="tod-cli --data-root …"`) fails as one word; use a shell function. The reference should give real column names for diagnosis (`conversations` and `node_timeline` have no `node_id`; `conversations.focus_node_id`), and there is no `tod-cli gate` noun to read which criterion failed.
- (2026-11-19, manager) After a deploy, a node whose run stopped on pending requests is not re-run by the daemon; a fix that should change what that run does (gap 78) needs the node made due (`L clear --node`) and a shepherd to Resume it.

- (2026-11-19, cli-41) Six pr_draft nodes showed "working" right after a deploy. That was the restarted daemon re-checking each node, and they went back to waiting by themselves. The shepherd blamed gap 78 for 0-ahead branches in workspace/kb, but `unlinked_branches` only looks in repositories where the node already has a PR. Shepherds should read the code path before filing a gap theory; the skill could say "a burst of working right after a deploy is the restart re-checking every node".
- Unobserved risk, not filed: `branches_without_pr` does not skip a branch that is 0 commits ahead of its base in a PR repository. The agent could not open a PR for one, so it would keep getting work it cannot do. File it if it ever shows up.

- (2026-11-19, cli-43) Ticket ids match several nodes (the -2/-3 copies from the move to the dev container), and `ledger.py verdict`/`next` then refuse them. Briefs should give slug or UUID, and shepherds should use the UUID. cli-43 also said "the manager retired me early", which never happened: it retired on its own after one round.

- (2026-11-19, cli-42) Slugs can match several nodes too (the -2 copies), so briefs and `L gap report --node` should use UUIDs. `conversations` keys a node by `focus_id`, with times in milliseconds. `tod-cli decisions list` needs `--node <UUID>`.

- (2026-11-19, cli-45) The reference should give a run_state/waits recipe: ids are BLOBs (`lower(hex(node_id))=?`), and `waits` keeps cancelled rows (`state='pending'`). Shepherds wrote helper scripts to shared /private/tmp paths and overwrote each other's; each shepherd needs its own scratch dir. Foreground `ledger.py wait` is moved to the background after about 180 s, and foreground `sleep` is blocked, so the skill should say how to wait (an until-loop on the output file). Gap 79 was confirmed live after deploy 13: four nodes re-checked with `since` unchanged.

- (2026-11-19, cli-46) A bare `survey.py --save` crashes with an IndexError: it needs a value. The skill's `T` shorthand omits `TOD_OPERATOR=shepherd`, and without it `tod-cli runner` fails as "unknown noun". To confirm a quiet re-check: `waits.updated_at` moves forward while `run_state.since` stays put. `ledger.py wait` returns `due_available` for nodes other shepherds will claim, so it can wake a shepherd for nothing.

- (2026-11-19, cli-47) `tod-cli capabilities show` doesn't exist; use `capabilities list <node>` on each ancestor to find who sets a lifecycle config. Request text comes from `decisions show <id>`, findings from `review list --node`. A lifecycle-config skill list set on a node whose repository holds the skills does not resolve for a descendant whose Files are another repository (ENG-3354, shmem-cache).

- (cli-49) `ledger.py verdict` rejects `--no-action` followed by an empty or trailing `--recheck-minutes`; put `--recheck-minutes N` before `--no-action`.
- (cli-49) ENG-2157's fingerprint flapped between "working: saving the branch to GitHub" and "waiting for greptile review" while only waiting on a bot. That wakes `wait` as `held_changed` with no real work. Either the fingerprint should ignore that wording, or the push runs when it needn't (check against gap 79's fix).
- (cli-49) Shepherds judged pr_draft "parked" from the survey plus `gh pr view` (CI, mergeability) without checking open threads or the bot score; the skill should say which checks a parked-on-ready verdict needs.
- (manager) manager-wait timed out twice with 4–5 nodes due past `--queue-delay-minutes 60` without firing `unclaimed_due`; the manager dispatched by hand. The queue-delay clock may count from the last sync rather than from when the node became due.

- (cli-50) `node_fields.linked_prs` in tod.db lists each node's PR URLs; `node_files_locations` is a blob, so read container/branch from `tod-cli runner menu` instead.
- (cli-50) Query `waits` with `state='pending'`: unfiltered, ENG-2157 alone had over 1,000 expired/cancelled rows.
- (cli-50) The daemon's log is `<root>/daemon/agentd.log`; `log/tod.log` is only the app.
- (cli-50) The zsh shell-function rule (`L(){ …; }`, not `L="…"`) is in briefs but not in the skill text; put it in shepherd-common.

## Open findings

Findings a review did not apply, one line each with its count and evidence.
Each review starts from these, bumps a count that came back, and removes what
it applied.

(none: the 2026-11-13 review's last two open points were about the retired
`shepherd-nodes` skill)

## Transcripts reviewed

- `fec2b2b0-45ba-43a3-bacb-1d4613e56139` reviewed through 2026-11-12T23:12:20Z by v1 — Shepherd nodes for Eng tasks (1st run)
- `027bd068-2c83-43fb-b125-7ee5d6a22dff` reviewed through 2026-11-13T02:04:42Z by v1 — Shepherd nodes for Eng tasks (2nd run)

## Lessons from runs (2026-11-16, cli-4)
- `tod-cli` is not on PATH; use the built binary under target/debug/ of the main checkout.
- `tod-cli decisions list` needs `--node <UUID>`, and can fail with "another tod instance is using the fleet storage root" (held by tod-agentd); read the `decisions` table in tod.db read-only with sqlite3 instead.
- Never run `.schema` on tables with sync triggers (huge output).
- `survey.py` truncates question text; get the full request from the database.
- (cli-2) The tod-cli that opens the live root is the one beside the running app (target/dev-fast/tod-cli); the debug build fails on the daemon's tod.lock. Store `L`/`T` shorthands as functions or wrapper scripts, not variables (zsh).
- (cli-2) Request ids in survey.py output differ from `decisions list` ids; cite the latter.
- (cli-2/4) Shepherds retire after one visit when all nodes are parked; they should linger per linger_minutes.
- (cli-3) `ledger.py verdict` and `survey.py` reject a ticket id like ENG-2926 as ambiguous (matches several nodes); use the node UUID. The brief should name the tod-cli binary.
- (cli-5) A shepherd holding a full batch gets `due_available` from `L wait` at once and cannot linger; it should say so and retire, or the manager should hand out smaller batches when more due nodes exist.
- (cli-1) `survey.py --save` needs a value. `tod-cli` calls intermittently fail with "another tod instance is using the fleet storage root" (held by tod-agentd); possible gap, not yet reported. tod-sandbox is also not on PATH. The two shepherds disagreed on which tod-cli build to use (debug vs dev-fast): the skill should name one.
- (ui) `L next --n 1` leased 5 nodes to the UI shepherd; the brief should say to release extras (or the manager should check `next` honors --n). Ledger verbs reject bare ticket ids; use slug/UUID.
- (cli-9) `L wait` returns `due_available` at once whenever any unheld node is due, so a shepherd whose nodes are all in_motion spins and retires early. `runner start` on a proposed cloud node blocks 1-2 minutes: use a 600s Bash timeout.
- (cli-12) `tod-cli decisions list --node <uuid> --all` works on the live root. `gh pr view` prints a leading warning line before the JSON. Possible gap to watch: a withdrawn decision ("carry on without Kasm") ended in pr_stuck instead of asking again (ENG-2504).
- (cli-10) `L verdict` releases the lease, so after verdicts on all held nodes `wait` returns due_available at once; call `next` again. New nodes change fingerprint at each phase change: use 20-25 min rechecks for nodes that are only working. Take fingerprints from `next`, not survey.py.
- (cli-11) `tod-cli runner` verbs must run with cwd inside the repo worktree (else "bundled process documentation not found"; or set TOD_PROCESS_ROOT). survey.py run state can be stale: check `runner menu` before parking a pr_draft node. Possible gap to watch: outdated-but-unresolved review threads on pr_draft PRs (flagship 6659, 6661) and whether readiness counts them.
- (cli-15) With the fixed ledger, a shepherd whose nodes are all parked (no recheck time) still has room, so `wait` returns due_available while others are due. The skill should say: claim extra due nodes up to nodes_per_shepherd, or retire; "act only on what changed" does not tell it which.
- (cli-21) Don't use `set --` in a zsh verdict loop (it clobbers positional args); use a plain `for n in ...`. cli-21 never read the shepherd-common docs: briefs should say to.
- (cli-20) The skill's L/T shorthand reads like variables: it should show them as shell functions (L() { python3 -I ... "$@"; }).
- (cli-18) Re-run `ledger check`/`ack` fresh after `runner check-now` (the fingerprint changes). `gh pr view --json` has no reviewThreads: use `gh api graphql` from a script file. `runner check-now` on a cloud node can take over 2 minutes: long timeout.
- (run 2026-11-16) `survey.py --save` needs a FILE argument; `next --n` leases extra nodes once the first batch has verdicts (counts against the cap); a long foreground `wait` gets backgrounded after 180s; `.zshenv`/`gh` print a harmless `.vite-plus/env` error; survey run line (`starting`) and runner menu run line can disagree for a stale start (gap 36).
- (run 2026-11-16) survey.py takes node args only (no --text/--full); decisions table keyed by BLOB id: use hex(); read request text with tod-cli decisions list --node <uuid> --all
- (run 2026-11-16) runner menu 'running' can disagree with survey 'stopped failed' for a silent supervisor: use sync-now for last-heard age; stop-now on such a node revives relay and queued Stop lands, follow with resume.
- When the shared GitHub core quota is empty, `gh pr view --json`/`gh pr checks` fail but `gh api graphql` (own quota) still gives draft state, head, checks and unresolved threads in one call. (cli-28)
- `ledger.py wait` returns `due_available` immediately while any node is due, so it cannot idle; `check` returns `lease_not_held` after a verdict (call `next` first). During a deploy `tod-cli` is rebuilt before `tod-agentd`, so runner verbs fail with "older than this program": that is the pause. (cli-28)
- `tod-cli` is not on a shepherd's PATH: use the running app's `target/dev-fast/tod-cli`. `survey.py` and `L check` reject a ticket matching 2 nodes (ENG-3418): use the slug. (cli-29)
