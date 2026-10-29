# Live lifecycle verification

The procedure an agent follows is the `live-verify` skill (text in
`doc/live-verify-skill.md`, to be installed as
`.claude/skills/live-verify/SKILL.md`); this page is the setup and the facts.
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
3. Copy `target/sandbox/*` (relay, orchestrator, Linux `tod-cli`, ...) from a
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
  `Golden flagship` (a cloud sandbox, fork of `golden`, directory
  `/workspace/flagship`).
- The generator's Quick accept: destination slug `eng-work`, accept
  capabilities Spec, Lifecycle, Agent, Files, Ticket.
- Accept a ticket (search it, click Accept): it is copied under `ENG work`
  and inherits the location.

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
