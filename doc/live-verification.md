# Live lifecycle verification

The procedure an agent follows is the `live-verify` skill
(`.claude/skills/live-verify/SKILL.md`, run it with `/live-verify <the bug>`);
this page is the setup and the facts.
Since the first run: the app reinstalls idle node sandboxes whose install
differs from the build at launch (`cloud_sync::refresh`), and the
orchestrator's gate checks a waiting pull request every 5 minutes and wakes
the node on feedback.

A persistent data root, `~/.local/share/tod-live-verify`, for walking a real
Linear ticket through the whole lifecycle in a Golden Sandbox fork, the way an
engineer on the team would, and fixing what breaks.

## Set up (once)

1. `scripts/seed-live-verify-root.sh` (run by you, not an agent: it copies
   credentials). It copies `tod.yml`, the Linear files and `credentials/`
   from `~/.config/tod`, keeps the Blaxel account and `golden` from
   `sandboxes.toml`, and drops `fleet_storage_root` and the shared worktrees
   root. A `tod.yml` that names the real root's `fleet_storage_root` makes the
   test app open the real database and daemon.
2. `credentials_file_only: true` in `tod.yml` (both roots): never use the OS
   keyring.
   The script also gives the root its own orchestrator user
   (`owner = "<you>-live-verify"` in its `sandboxes.toml`): the orchestrator
   keeps one database per user, and two roots with the same user seed over
   each other. An existing root with the real root's owner must be changed
   by hand the same way (then it seeds a fresh copy on the next launch).
3. `tod-sandbox --data-root <root> golden refresh cofactr/workspace` makes the
   workspace golden (a fork of the account's `wsx-golden-*`, which stays; its
   `/workspace` origin is Cofactr/workspace). Do not make a `cofactr/flagship` one.
4. Copy `target/sandbox/*` (relay, orchestrator, Linux `tod-cli`, ...) from a
   checkout that has built them; `tod-sandbox --data-root <root> doctor` must
   be clean.

## Launch

```sh
cargo build -p tod -p tod-cli -p tod-sandbox-cli
./target/debug/tod --data-root ~/.local/share/tod-live-verify --no-focus \
  --width 1800 --height 1000 --agent-socket-port 47611
```

Drive it over the control socket (README.md). Use `shot2 <png>`, not `shot`:
`shot` can be stale while the window is covered; `shot2` forces a draw.
`scroll <x> <y> <dx> <dy>` scrolls (positive dy = down). Wait ~1 s after a
click before the next one (dialogs animate).

## The tree

- `Linear tickets`: a generator node (source Linear, query
  `team IS ANY OF (ENG) AND stateType IS ANY OF (unstarted, backlog)`, cap
  250). Settings: `filter_query` and `result_cap` (`tod-cli` accepts any key,
  so use these exactly).
- `ENG work`: the destination. Files capability picks the location
  `Golden workspace` (a cloud sandbox, fork of `golden`, directory
  `/workspace`).
- The generator's Quick accept: destination slug `eng-work`, accept
  capabilities Spec, Lifecycle, Agent, Files, Ticket.
- Accept a ticket (search it, click Accept): it is copied under `ENG work`
  and inherits the location.

## Two locations, two repositories

The root has two Files locations (Locations dialog; the seed script adds them):

| Location | Repository | Use it for |
|---|---|---|
| `Golden workspace` | `Cofactr/workspace` (flagship, kb, ... are submodules inside it); a fork of `golden`, directory `/workspace`; its golden is `cofactr/workspace` (`tod-sandbox golden refresh cofactr/workspace`) | Checks on real data and the real golden flow: Linear tickets, `ENG work`, Start/Migrate/Reset environment on what the real roots do. **Nothing may be pushed from it** (see below). |
| `Build test` | private `joel-cofactr/build-test`; cloud sandbox "From an image", directory `/workspace/build-test` | Anything that pushes a branch or opens a PR, and anything that needs flexibility (any repository content). |

Never use `/workspace/flagship` as a location directory: its origin is
`Cofactr/flagship`, so the golden is keyed `cofactr/flagship` and Migrate would
push branches there. The old `Golden flagship` location and the
`cofactr/flagship` golden were retired for that reason (the location kept its id
and became `Golden workspace`, so nodes that used it follow).

`Build test` is an image location, so its repository has to come from somewhere:
until the Locations dialog accepts an HTTPS repository URL for it (gap 14 of
`runner-menu/live-check-2026-11.md`), set the node's `node_fields.repo` to
`https://github.com/joel-cofactr/build-test` in the root's `tod.db` (as pass 7
did). A golden cannot be made for build-test (a fork never clones).

## Loop

Walk the accepted node one lifecycle step at a time. On any failure: write a
short repro, hand it to a sub-agent in its own worktree to fix with a
regression test, merge, rebuild, relaunch, and rerun from the failing step.

## Gotchas found on the first run

- **Stale daemons answer for you.** `tod-agentd` hosts the database broker and
  outlives its app. Every data root whose `sandboxes.toml` declares a
  `[[database]]` runs one, and the orchestrator offers a request to each. A
  daemon from an older build (your real root's, another worktree's) answers a
  node it does not own with a final error. Before testing `tod-cli db`, list
  them (`ps aux | grep tod-agentd`) and stop the old ones.
- **Sandbox binaries.** `tod-sandbox orchestrator` takes the newest
  `sandbox/<name>` above the executable; never leave a copy in
  `target/debug/sandbox`. Pass `--bin target/sandbox/tod-orchestrator` to be sure.
- **Testing `tod-cli db` by hand.** A plain shell lacks `TOD_ORCHESTRATOR_CLI_URL`
  (`<orchestrator>/port/8090/cli`), `TOD_USER` and `TOD_NODE`; set all three.
- The node's `Needs access` plan step needs **Retry** once access works.
- **A fresh worktree has no `target/sandbox`.** The first "Run with a new session"
  then fails naming `target/debug/sandbox/tod-relay`; build the binaries
  (`scripts/build-sandbox-binaries.sh`) or copy `target/sandbox/*` from a checkout
  built from the same commit (never into `target/debug/sandbox`).
- **A node sandbox made for another orchestrator user** (the root's `owner`
  changed after its `cloud_nodes` row was written) gets `TOD_USER` of the old
  user: its supervisor seeds from that user's database, does not find the node
  and exits 1, and the app waits 150 s. The app now refuses it, naming Delete
  Sandbox. A shared orchestrator can also answer a poke with Blaxel 502s for
  many minutes (egress proxy); redeploying the orchestrator cleared it.
- **The orchestrator sits behind Blaxel's edge.** Every request to its URL
  needs `Authorization: Bearer <Blaxel key>`, `/health` included (a bare
  `curl` gets 401). The app's version-contract probe forgot it once and every
  sync failed with "orchestrator health: 401"; `tod-sandbox exec
  tod-orchestrator` is the way to look inside.
- **Blaxel masks env values** in its listing (`TOD_USER` reads `****`). That
  is not a user: `SandboxInfo::env` treats it as absent.
- **The orchestrator's log** is in the sandbox, `/var/log/sandbox-api/tod-orchestrator-<id>.stderr.log`
  (newest by `ls -t`, or `grep -l trusting`). It says how many CA certificates
  it trusts at its first HTTPS call. `UnknownIssuer` on every poke was seen
  once on a fresh deploy and not again after a second one; the cause is not
  known (Python in the sandbox verified the proxy chain with the same bundle).
- **A node with a live heartbeat is not reinstalled at app launch.** After a
  deploy use the task panel's runner menu, Reset environment, to
  put the new supervisor on it; check with `tod-sandbox exec <sandbox> --
  python3 -c ...` on `/var/lib/tod-supervisor/<node>/tod.db` (`user_version`).
- **The live-verify root's `orchestrator-github.json`** lists the GitHub tokens
  the orchestrator may use for the root's user and which repositories each
  reads. If none covers the node's repository the gate logs "pull request not
  found (GitHub also answers 404 when tod's token cannot see the repository)"
  and wakes the sandbox instead; the supervisor then looks itself.
- **Updating a sandbox's proxy redeploys it from its image.** Whatever was
  installed into it before (tod's relay, manifest, supervisor, bundles) is gone;
  a plain `tod-sandbox fork` of a sandbox keeps them. So install after any proxy
  update (`golden refresh`, `fork_golden`), never before.
- **A second `tod` on the root** (another session's) overwrites
  `sandboxes.toml` with its stale copy and logs "Could not checkpoint" for your
  sandboxes; check `ps aux | grep 'debug/tod '` first, use your own port, and
  re-read `tod-sandbox golden` after anything you did to the golden.
- **The control socket's `shot2` lags one action** after a click that opens a
  menu or dialog: take it twice. A node's turn on a throwaway PR node lasts
  ~20 s; catching "running" menus needs a script that clicks without a tool
  round trip in between.
- **No `timeout` command on macOS**: `timeout 120 cargo ...` fails with
  "command not found", and a pipeline that greps its output hides that. Run
  cargo plainly, or check the exit code. macOS `sed -i` needs `-i ''`.

## Test pull requests and branches: only in `joel-cofactr/build-test`

Any pull request or branch a live check needs (a throwaway draft PR, a
`task/<slug>` branch, a review request) goes **only** in the private repository
`joel-cofactr/build-test`, so a node that will push or open a PR uses the
`Build test` location, never `Golden workspace`. Never create, push, mark ready, comment on, or touch
a PR or branch in any Cofactr organisation repository (`Cofactr/workspace`,
`Cofactr/flagship`, ...): that notifies code owners and is the user's to handle.
Close every PR you open there and delete the branches you make when you are done.

## Testing the webhook wake (`joel-cofactr/build-test`)

A throwaway private repo for GitHub tests; a webhook for the live-verify
orchestrator is registered on it (`gh api repos/joel-cofactr/build-test/hooks`,
events: pull requests, reviews, comments, checks, pushes, releases) and stays.
Its payload URL and where the secret is: `tod-sandbox --data-root <root>
webhooks`. A delivery's result (200, the keys it made, which nodes it routed to)
is in `gh api repos/joel-cofactr/build-test/hooks/<id>/deliveries[/<delivery>]`
(`.response.payload`); a redelivery is `POST .../deliveries/<id>/attempts`.

Recipe, as run 2026-11-03 (a wake 2 s after the merge, the wait's own timer
due 3 minutes later):

1. A branch named like the node's (`task/<slug>`) with one commit, and a PR.
2. `tod-cli node create --parent eng-work`, `capabilities enable <n> spec
   lifecycle agent files ticket`, `pr link joel-cofactr/build-test#<N> --node
   <uuid>` (needs the UUID), and set `node_lifecycle.state = 'pr_review'` in the
   root's `tod.db` (no CLI sets it). The Files location can stay
   `Golden workspace`: only the PR link matters for the wait.
3. Run it in the cloud (task panel). With the PR having failing or stuck checks
   the node ends `pr_stuck`; `gh workflow disable` the repo's two workflows and
   push a commit so the PR has none, then "Run with a new session": the node
   goes to `approved` and waits on `github:pr:<N>:merged`.
4. `gh pr merge <N> --squash`. The orchestrator logs `webhook PR #N merged for
   <user>'s node ...`, the wait becomes `satisfied` and the supervisor starts
   at once. Re-enable the workflows and delete the branch afterwards.

Gotchas from it: a node's recorded repository is its checkout's, not its PR's,
so routing keys on its linked PRs (a flagship node's PR is in kb); the gate
replaces every `github:pr:` wait of a node in review, so add test waits only in
states it does not own; `tod-sandbox fork` makes a sandbox with no proxy rules,
which `fork_golden` refuses, so a repo-specific base must come from a sandbox
the app made; macOS `tar` adds `._` files that break a git checkout (use
`COPYFILE_DISABLE=1`).
