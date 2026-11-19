# Prompt cache: what was measured, what changed, what remains

Work item 16 of `find-it-first.md` (section 13, "Cost"). Cached input costs
about a tenth of fresh input, and a prefix is cached only while it is
byte-identical, on the same model, inside the cache's lifetime.

## 1. Block order (done)

Every recipe in `tod_core::context_recipes` now orders its dynamic blocks
from what is the same for every session on a node to what changes each round
(`blocks_run_from_stable_to_volatile` pins it; the ranking function is
exhaustive, so a new block must be placed):

| Rank | Blocks |
|---|---|
| 0 | Databases, WorkingDirectory, DataRoot |
| 1 | Node, NodeProcessFields, Focus, Environment, Workspace |
| 2 | SelectedObligation, NodeObligations, AncestorContext |
| 3 | ConfiguredSkills |
| 4 | Plan |
| 5 | PullRequests, ObligationVerdicts, ReviewFindings, WorkHistory, IncomingChanges, VisualDesign |

Changes: the review recipe used to put the PR and the verdicts before the
skills; the implementation, fix and phase recipes put the PR first and the
skills last; fix put its findings before the plan. All now end with the
per-round blocks. Skills are placed before the plan (the plan changes as
steps are worked; the configured skills do not). The surrounding order was
already right: static fragments (stance, domain, cli, surface), then the role
doc, then the blocks, then the tail (the phase status and usual actions, so
the per-round status is last). The working directory is per node, not per
session. The only per-conversation value (the visual-design draft path holds a
conversation id) is in the VisualDesign block, the last rank. No timestamp or
session id is rendered anywhere in an opening.

Not yet ordered: the opening's `cli/` nouns for a situational recipe are
chosen by the focus and its lifecycle state, so two sessions on one node in
different states share a shorter prefix.

## 2. Token usage (recorded now)

What existed: the ACP agent's `session/prompt` response `usage`
(`inputTokens`, `outputTokens`, `cachedReadTokens`, `cachedWriteTokens`) was
already read by `tod_agent::usage::AcpUsage` into `TokenUsage`, shown live and
read from Claude's session log for the conversation header
(`session_info`). Nothing was stored: `tod-cli stats` said "tokens are not
stored anywhere". Checked in `claude-agent-acp` 0.109.0: it does return that
usage on every prompt response, so Claude over ACP reports all four counts.

What was added: schema v101, table `conversation_usage` (one row per
finished turn, failed turns too: conversation id, time, phase, model,
requests, input, cache read, cache write, output).
`ConversationDriver::record_usage` takes the growth of the session's running
total across the turn (the whole total if it went down, a session that
started over) and enqueues `FleetMutation::RecordTurnUsage`. The protocol and
node come from the conversation row; the phase is `launch::phase_for`.
Queries: `tod_store::conversation_usage::{node_totals, phase_totals,
conversation_totals, node_turns}`, each with `cache_read_share`. Shown by
`tod-cli stats --section cost` (operator tier), which now has tokens and the
cached share per node and phase. Turns before v101 show "no tokens recorded".

## 3. What Claude Code puts ahead of our opening

Documented (code.claude.com/docs/en/prompt-caching, env-vars):

- Order: system prompt (tools, core instructions), then project context
  (CLAUDE.md, auto memory), then the conversation. The conversation opens
  with the working directory, platform, shell and OS version.
- "Sequential sessions share the prefix only when the git status snapshot
  taken at startup matches." The snapshot (branch, status, recent commits) is
  taken at startup, so it changes after every commit.
- The date line is not documented anywhere as a cache factor, and no setting
  pins it.
- The main conversation's cache lasts 5 minutes on API-key billing and 1 hour
  on a subscription; `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` extends it. The cache
  is per model and "effectively scoped to one machine and directory".

Measured from 396 sessions in `~/.claude/projects` (first request of each;
`usage` per request, counted once per message id):

- Median first request: 54.6k tokens read from cache, 38.5k written; 57%
  read. The read part is the same on every session on this machine, whatever
  the directory: the system prompt and tools (peaks at ~55k and ~46k, two
  tool sets). Everything after it is written afresh: CLAUDE.md, the context
  block, the first message.
- Consecutive sessions in one directory, same model: opened within 5 minutes
  of the previous one's last request (n=14) median read 55.0k, written 29.8k;
  5 to 60 minutes (n=43) read 54.9k, written 31.0k; over an hour (n=74) read
  46.0k, written 39.9k. So even a quick second session read nothing beyond the
  system prompt: the project context and first message are not shared across
  sessions. These sessions had different first messages, so this does not
  separate the git snapshot from the message itself.
- tod-launched sessions: only three phase-agent logs with the tod opening
  survive locally (cwd is not a git repo, so there is no snapshot), and none
  has request usage recorded, so there is no tod-specific cache-read figure
  yet. The Claude logs record an `environment` attachment (working directory,
  `isGitRepo`, platform, shell, OS) for each session; no date or git status
  is stored there. The new `conversation_usage` rows will give the real per
  session share once tod runs.

Reading: a new session on a node reads the system prompt from the cache and
rewrites its opening. Two things limit more: the cache is a prefix cache
whose entries exist only at earlier requests' breakpoints (the end of the
previous session's first message), and tod sends the opening and the user's
message as one turn, so a second session's prefix differs at the first
message. Unverified (needs a raw-request capture, `OTEL_LOG_RAW_API_BODIES`,
or a controlled pair of sessions): whether an opening alone, sent as its own
turn, would be read by the next session.

### Applied

`tod_agent::acp_host::cache_env` adds `CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS=1`
to every Claude launch (local, dev container, sandbox), unless the caller
sets it. It is the only documented switch for the git status snapshot, and it
also removes Claude Code's built-in commit and PR instructions. tod's agent
context carries the commit and push rules (`workspace/codebase`, the PR
surface), so this was judged acceptable; revisit if agents start committing
badly. Tested in `acp_host` (`claude_is_launched_without_the_git_status_snapshot_and_cursor_is_not`).
Not verified: that `claude-agent-acp` passes the variable through to the
Claude Code process (it inherits the environment; check with a raw request
capture).

## 4. What remains

- Measure the per-session cache-read share from `conversation_usage` after a
  few real runs and fill the "before" and "after" numbers here; the "before"
  for the block order cannot be recovered, since nothing was recorded.
- Capture a raw request (`OTEL_LOG_RAW_API_BODIES=file:<dir>`) to see where
  the date and the CLAUDE.md block sit and where breakpoints fall.
- Auto memory's path in the system prompt is per checkout; `autoMemoryDirectory`
  (or `CLAUDE_CODE_DISABLE_AUTO_MEMORY`) would stabilize it. Not applied: it
  changes what agents remember.
- `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` for runs that pause between rounds
  (needs Claude Code 2.1.242 or later). Not applied.
- Item 12's shared reviewer opening depends on the same prefix behavior: test
  that the second reviewer really reads the first's opening before relying on it.
