# Shepherd reviews

## Lessons from runs

The manager adds here what it, or a shepherd in its reply, learned mid-run
that the skills should have told them (nobody edits a shepherd skill during a
run). Older entries came from the single-session `shepherd-nodes` skill. Each review folds these into
the skill and clears the ones it applied.

(none yet)

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
