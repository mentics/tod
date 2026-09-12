# Interview

An interview turns a conversation with the user into the obligations — refined further in the design phase, then turned into a plan — of one **node**. Two agents cooperate through the database while the user answers questions in the app at their own pace.

| Actor | Does |
|--|--|
| **Question maker** | Keeps a short queue of the most valuable open questions. |
| **User** (in the app) | Answers, defers, or sends a question back to be improved. Accepting a proposal applies it immediately. |
| **Answer processor** | Works out what each answer means: settles obligations, repairs whatever the answer made wrong, and keeps memory current. |

Neither agent talks to the user. Everything flows through **questions**, **obligations**, and **memory**.

## What you are given

Your context is given once and then only added to.

- **Your first turn** carries these docs and a **snapshot** of the node: purpose from the root down, this node's obligations by section, inherited obligations by source node, content, the memory your role sees, open and deferred questions, and one line per answered question.
- **Every later turn** carries only **what changed** since your previous turn, then the instruction. Your own changes are left out — you made them. Anything new to you arrives in full once; after that, only its changed fields.

Your context is therefore current. **Don't re-read with `tod-cli` what you were given.** If something you change was modified by someone else in the meantime, `tod-cli` refuses the write and prints the current version — decide again with that.

A session can be replaced by a fresh one between turns. Anything worth keeping beyond this turn must be in obligations, interview memory, or a question — never only in the conversation or in a memory feature of your own.

Questions and memory belong to the **node**, not to one session. They survive app restarts and carry over when the user returns to this node, including in later phases.

## Obligations form a hierarchy

**Requirements** say what must be true for the work to be done. **Constraints** bound how it may be done — platforms, compatibility, data rules, boundaries with other work.

**Across the tree.** A node inherits every obligation of its ancestors. Put an obligation on the highest node where it is true for everything beneath; by default that is the interview node. Never restate an inherited obligation on this node. When this node genuinely needs to differ from an inherited one, that is a conflict for the user to settle: propose rewording the ancestor's obligation to carve out the exception, not a contradicting copy here.

**Within a node.** Obligations are grouped by kind, then by optional **section**. Use existing section names exactly. Start a new section only when several related obligations would share it; small lists stay unsectioned. Regrouping an existing list is not interview work.

## Writing obligations

Whoever drafts obligation text — as a proposal or a direct write — holds it to this standard:

- **Checkable.** Someone could look at the finished work and say pass or fail. If that takes more than a sentence or two, it is probably several obligations.
- **Grounded.** Only what the user said, accepted, or clearly implied. No invented scope.
- **Positive.** State what must hold. Don't record that something *doesn't* happen unless something would otherwise suggest it does.
- **Right phase.** Requirements say *what*, not *how*. Vendors, tools, and implementation belong in design or planning — park them (see Memory).
- **No meta-obligations** such as "follows the parent's requirements" — inheritance already does that.

## Plan steps

In the `planning` phase, the deliverable is a set of **plan steps** (`tod-cli plan`), not a text document. Each step is a discrete unit of work with a `body` and a `status` (`pending` → `ready` → `in_progress` → `implemented` → `verified`, or `blocked`).

- Steps form a dependency **graph**, not a single order: link one to what must land first with `--depends-on` (on `add`) or `plan depend` afterward. Leave independent steps unlinked — that's what lets them be worked in parallel, in separate agents or worktrees. `plan ready --node <NODE>` lists the steps eligible to start now (status `ready`, or `pending` with every dependency `implemented`/`verified`): the set dispatchable at once.
- Link each step to the obligation(s) it satisfies with `--satisfies` (on `add`) or `plan satisfy`/`plan unsatisfy` afterward. One obligation can be satisfied by several steps — a requirement that touches several places in the codebase becomes several linked steps. This is the traceability mechanism: a requirement is traceable once it maps through a `--satisfies` link to a step, and that step reaches `verified`.
- `implemented` and `verified` are separate milestones on the same step: `implemented` unblocks dependents, `verified` is the step's final completion state.

Obligations can no longer be tagged phase `planning` — they stay a requirements/design artifact. When planning uncovers a missing or wrong requirement, that's still an obligation edit (`tod-cli obligations`), just never tagged phase `planning`.

## Memory

Interview memory — the notes you read and write with `tod-cli memory` — is how the interview keeps what isn't an obligation, and how the two agents pass context to each other. It is the only memory here: don't use any memory feature of your own agent platform (memory files, saved notes). Nothing written there reaches the app, the other agent, or a fresh session. Keep every note short and specific: one fact or request per note. Update or close an existing note rather than adding a near-duplicate.

| Kind | Written by | Read by | Holds | Closed when |
|--|--|--|--|--|
| `context` | either | both | Facts that shape future questions or how to read answers: who the users are, priorities, vocabulary, defaults the user is happy with | It stops being true |
| `handoff` | answer processor | question maker | A follow-up the question maker should ask, and why | The question maker acts on it |
| `parked` | either | both | Detail volunteered for a later phase, tagged with that phase | Promoted or discarded in that phase |
| `plan` | question maker | question maker | What is settled, what remains, and what to ask next — one per phase, each new plan replaces the last | Replaced |

Questions also carry an `intent` the user never sees: the question maker's note to the answer processor on how to read the answers.

## tod-cli

Every command takes `--data-root <DATA_ROOT>` (from your snapshot). Ids: obligations by the 8-character prefix shown to you; questions `q-<n>` and memory notes `m-<n>`, numbered per node. Writes print one line. Use `list` / `show` only when you truly lack something.

```
obligations list      --node <NODE> [--inherited]
obligations add       --node <NODE> --kind requirement|constraint --body <TEXT> [--section <NAME>]
obligations update    <ID> [--body <TEXT>] [--section <NAME>]
obligations delete    <ID>

content get           --node <NODE> --type goal
content set           --node <NODE> --type goal --body <TEXT> [--append]

plan list             --node <NODE>
plan show             <ID>
plan add              --node <NODE> --body <TEXT> [--after <ID>] [--before] [--depends-on <ID>] [--satisfies <OBLIGATION>]
plan update           <ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|blocked]
plan delete           <ID>
plan depend           <ID> --on <ID>
plan undepend         <ID> --on <ID>
plan satisfy          <ID> --obligation <OBLIGATION>
plan unsatisfy        <ID> --obligation <OBLIGATION>
plan ready            --node <NODE>

questions list        --node <NODE> [--status open|answered|deferred|withdrawn]
questions show        --node <NODE> <Q>
questions add         --node <NODE>                              # question YAML on stdin; prints its id
questions withdraw    --node <NODE> <Q> --reason <TEXT>
questions processed   --node <NODE> <Q> --summary <TEXT>

memory list           --node <NODE> [--kind context|handoff|parked|plan] [--status open|done]
memory add            --node <NODE> --kind context|handoff|parked|plan --body <TEXT> [--phase requirements|design|planning] [--question <Q>]
memory update         --node <NODE> <M> [--body <TEXT>] [--status done]

interview exhausted   --session <SESSION> --reason <TEXT>
```

Never open the database directly.

## Principles

1. **Obligations belong to the user.** Change one only when an answer decides it or the user confirms the change.
2. **Make answering cheap.** A good question takes one keystroke and a moment's thought. The agents do the drafting, categorizing, and checking.
3. **Nothing volunteered is lost.** Detail that doesn't fit yet is parked, not dropped and not forced in.
4. **Plain language in anything the user reads.** Name the phase in plain words (requirements, design, planning). Never mention agents, queues, memory, sessions, or ids. Refer to an existing obligation by a short label taken from its text — never by its position or id.
