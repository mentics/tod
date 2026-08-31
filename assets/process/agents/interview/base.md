An interview gathers blocking information in small steps. Durable outcomes are returned for persistence (with human approval where required). Exact Q&A is recorded in a **transcript** — not summaries.

Prefer **decisions with `proposed_text`** (Accept after optional edit) over a gather step plus a later wording rubber-stamp.

## Two sessions

| Session | `queue/` |
|--|--|
| **Question maker** | Creates question files |
| **Answer processor** | Deletes or modifies question files when answers are handled |

Dedicated pooled sessions — no orchestrator, no nesting on every answer.

## Question queue

One markdown file per open question: `{scratchpad}/queue/q-NNN.md`. UI lists and submits answers in any order, including parallel.

## Transcript

| Actor | Transcript |
|--|--|
| Question maker | Creates on bootstrap (session header only) |
| UI | Appends exact Q&A **before** answer-processor invoke |
| Answer processor | Read only |

Append format:

```markdown
## {question-id}

{question body from queue file}

**Answer:** {user answer — include MC option when selected}

**Default stated:** {optional}
```

Question maker actions (defer, reconsider, more-options):

```markdown
## {question-id} (action: {action})

{optional user notes}
```

## Session files

| Path | Purpose |
|--|--|
| `{scratchpad}/interview-config.md` | Session manifest |
| `{scratchpad}/queue/` | Open questions |
| `{scratchpad}/scope/` | Obligations + node context export |
| Entity `…/scratchpad/to-process.md` | Phase overflow (entity-level, shared across sessions on the node) |
| Transcript | App-managed per session |

Never share a session scratchpad across concurrent interviews.

## Phase overflow

Answers often mix current-phase content with later-phase detail. Only the **current interview phase** slice belongs in outcomes for this session.

Park wrong-phase detail in entity `to-process.md`. Answer processor appends; question maker reads before asking; later phases consume. Do not lose volunteered detail; do not force it into the wrong artifact.

## Obligation inheritance

Before asking or writing at this node, read **inherited obligations**. If an applicable obligation already exists, do not ask and do not duplicate at this node. Target writes use question `layer`.

## Principles

1. **Propose, do not own** — human-owned obligations; Accept / write-from-decision is permission to return that item now; no second “OK to add already-accepted items?” ask
2. **Defaults explicit** — state assumed defaults in the transcript
3. **One session, one scratchpad**
4. **Waivers explicit** — silence or hurry is not waiver
5. **No process commentary** in user-visible replies (agents, queue, lifecycle jargon)
6. **Numbered lists for referral** in question `context` when the user may refer to items by number — not for MC options (those use `options:`) or external obligation indices (use short labels)

## Completion

Interview complete when `queue/` is empty, question maker status is `complete`, and answer processing has drained.
