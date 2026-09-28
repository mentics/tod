# Phase agents and deterministic gates

Status: proposed design. Replaces the gate-check agent and the on-entry turn.

## The rule

**A gate check never runs an agent.** It is a deterministic check the app runs
itself: instant, repeatable, and the same wherever it runs. Anything that needs
judgement ("are these requirements meaningful and non-conflicting?", "is this
design buildable?") is done beforehand by the **phase agent** for the state the
node is in. When the agent judges its phase done, it **certifies** the phase.
The certificate records a digest of what was judged. The gate recomputes the
digest and passes only while nothing it covers has changed since.

**The runner has no built-in stopping points.** Once started, it takes a node
through the whole lifecycle with no user intervention whenever it can. It stops
only for something that only the user can supply, such as a missing intent, a
decision that belongs to them, or an access grant. Required human PR review is
the one exception: it naturally holds `pr → approved` until the PR is
mergeable. Anything an agent can fix confidently, an agent fixes. A stop that
makes the user ask "why did you stop to ask me that?" is a bug.

This design does two things:

- **Agents can talk to the user.** The phase agent is an ordinary
  conversation, so it can ask a question (multiple choice when the answers can
  be listed, free text when they can't) and wait for the answer. A one-shot
  gate agent could only report `needs_human` with a summary, and the task panel
  had no way to answer that summary.
- **Each role is clear.** The phase agent makes the phase true. The gate
  confirms that it is still true.

## Terms

| Term | Meaning |
|---|---|
| **Phase agent** | The agent for one lifecycle state. It runs while the node is in that state and the gate does not pass. It replaces both the on-entry turn and the gate-check agent. Implement, verify, review, fix and pr are the phase agents of `active`, `verifying`, `review` and `pr`, and keep their current protocols. |
| **Certificate** | A row saying "phase *S* of node *N* was judged complete, over inputs with digest *D*, by session *X*". Only the app computes *D*; the agent just asks for the certificate. |
| **Evaluator** | A fresh agent session, separate from the phase agent, that judges the phase and certifies it or sends it back. Used when the setting below is on. |
| **Gate** | The app's deterministic check for leaving a state. It combines the derived criteria we already have with, for some states, "a current certificate exists". |

## The loop

For a node in state *S*, the autopilot's `next_step` becomes:

1. **Gate passes** → advance (app only, no agent). Then loop in the next state.
2. **Unanswered decisions** → stop, `NeedsHuman::Decision` (same as today).
3. **Otherwise** → run *S*'s phase agent. It resumes its conversation if one is
   open for this stay in *S*, otherwise starts a new one.

The phase agent's turn ends in one of three ways:

- **It asked the user something** (`tod-cli decisions ask`, options optional).
  The runner stops and the question appears in the task panel. The answer goes
  back to the same conversation as a turn, and the runner continues.
- **It judged the phase done:**
  - *Independent evaluation off:* it certifies (`tod-cli phase certify`) and
    the loop re-runs the gate.
  - *Independent evaluation on:* it asks for evaluation
    (`tod-cli phase ready`), and the app starts an evaluator (below).
- **Neither:** it changed nothing and asked nothing. That is `NoProgress`, as
  today, with its last reply shown as the reason.

There is no separate on-entry turn. What each `## On entry` section did
(regenerate the summary, draft the plan, read the release evidence) becomes the
first thing that state's phase agent does. `proposed` has a phase agent too, so
a new node with nothing on it gets one whose first move is to ask what the node
is for.

### Independent evaluation

Setting: **Require independent evaluation for lifecycle transitions**
(`lifecycle.independent_evaluation`, default **on**). It is in the Settings
view under a new "Lifecycle" section and passed through `ConversationConfig`,
so both the local runner and the cloud supervisor use it.

- **On:** the phase agent cannot certify its own phase; `tod-cli phase certify`
  refuses a phase-agent actor. `tod-cli phase ready` marks the phase ready, and
  the app starts an **Evaluate** conversation. That is a new session with its
  own protocol and recipe. It gets the phase's "Done when" checklist and the
  node's current data, not the phase agent's transcript.

  The evaluator stays **pure**: it cannot edit the node. `tod-cli` refuses
  every outline, obligation, plan and content mutation from an Evaluate actor.
  It ends in one of three ways:
  - **Certify** (`tod-cli phase certify --note …`). The certificate records
    the evaluator's conversation, and the loop re-runs the gate.
  - **Send back for an agent fix** (`tod-cli phase reject --fix "…"`, once
    per problem). Use this for anything an agent can fix confidently: an
    obvious mistake, a duplicate, a vague obligation whose meaning is clear
    from context. The lifecycle processor resumes the phase agent with the
    list of fixes as its turn. The phase agent makes them and calls
    `phase ready` again, which starts a fresh evaluator. **The user is not
    involved.**
  - **Ask the user** (`tod-cli decisions ask`). Only for what no agent can
    settle: intent, priorities, access. The answer goes to the phase agent,
    which is the one that can act on it, and the loop continues from there.

  **Loop guard.** A rejection whose digest equals the digest of an earlier
  rejection in the same stay means the phase agent changed nothing in
  between. The runner then stops with `NeedsHuman::EvaluationStuck` and shows
  both sides. Rejections that lead to real changes are not capped, other than
  by the runner's overall budget.
- **Off:** one phase-agent session does the work, evaluates it against the same
  checklist, fixes whatever it finds, and certifies. No evaluator runs.

Implement, verify and review already run in separate sessions. That is the
same independence, and they are not changed.

## Certificates

Table `phase_certifications`, keyed on `node_id` and `state`, so it holds only
the latest certificate:

| Column | |
|---|---|
| `node_id`, `state` | primary key |
| `digest` | SHA-256 over the canonical JSON of the state's inputs (below) |
| `conversation_id` | who certified: phase agent, evaluator, or null for the user |
| `evaluator` | `self` \| `independent` \| `user` |
| `note` | the certifier's one-line rationale, shown in the lifecycle panel |
| `certified_at` | |

A certificate is **current** when both of these hold:

- `certified_at` is not earlier than `node_lifecycle.updated_at`, so a
  certificate from an earlier stay in the state does not count;
- the digest recomputed now equals `digest`.

The digest is computed in `tod-store`, over rows sorted by id. `sha2` is
already a dependency there. The same function runs at certify time and at gate
time, and nobody else computes it. Ids are included in the digest, so a row
that is deleted and then recreated with the same text still invalidates it.
Nothing in the digest changes when a row is reordered or when its status
changes, unless the state lists that field.

Because the digest is recomputed from the rows, a change that is later reversed
stops counting, just like `lifecycle_baseline`. Any writer invalidates a
certificate: the user in the UI, a conversation agent, or an incoming change.
There is no separate trigger, and `trg_buildable_reset` can go.

The node goes back to its phase agent when a certificate stops being current.
The gate then fails, so the loop's step 3 runs the phase agent. The agent is
told what changed since it certified; the certificate keeps a JSON snapshot
alongside the digest so the app can diff it. It re-judges, and may be done in
one turn.

The user can certify from the lifecycle panel ("Mark phase done"). That replaces
Waive for the certificate criterion. Force advance stays as it is.

## Per-transition design

"Certified" means the transition's gate includes the derived criterion
`<from>.phase-certified`: a current certificate exists for *from*.

| Transition | Gate (all deterministic) | Certificate digest covers | What the phase agent / evaluator judges |
|---|---|---|---|
| proposed → design | ≥1 own `requirement` obligation; **certified** | own obligations (id, kind, body) | Is there a real task here? The obligations define something concrete to do, don't conflict with each other or with inherited obligations, and don't duplicate ancestors or siblings (today's dedupe rule). This is the only question. No "the user said start design" rule: that is removed. When the node has nothing to go on, the phase agent asks the user what it is for, in free text. |
| design → planning | **certified** | own obligations; design content; attached mockups (ids, `sha256`) | Replaces `buildable` and `constraints-satisfied`: would a competent implementer build it correctly from this? |
| planning → ready | `requirements-traceable` (derived, kept); **certified** | own obligations; plan steps (id, body, obligation links) | Replaces the 11 agent criteria, which become the evaluator's checklist in `planning.md`. |
| ready → active | `action-config-configured` (derived) | — | No agent (unchanged). |
| active → verifying | `plan-steps-implemented` (derived) | — | Unchanged; implement is the phase agent. |
| verifying → review | `obligations-verified`, `plan-steps-verified` (derived) | — | Unchanged; verify is the phase agent. The 9 agent criteria are retired: what they asked is either already in the verify protocol's instructions or is now covered by the verdicts. |
| review → pr | `review-done`, `findings-answered` (derived) | — | Unchanged. |
| pr → approved | `mergeable` (derived, GitHub) | — | Unchanged; pr is the phase agent. |
| approved → merged | `pr-merged` (derived, GitHub) | — | Unchanged. |
| merged → released | **certified** | nothing (empty digest) | Release evidence recorded. The certificate note carries the evidence. |
| released → learn | **certified** | nothing | Post-release smoke recorded with evidence. |
| learn → done | `learn-recorded` (new derived: a `tod-cli learn` record exists for this stay) | — | The learn phase agent writes the retrospective; the record is the check mark, so no certificate is needed. |

With an empty digest, a certificate is just a check mark for this stay. That
covers the states where nothing on the node can drift after the agent judges
it.

`lifecycle_baseline` (taken on entering `ready`) now has the same job as the
planning certificate's snapshot. As a follow-up, `lifecycle_validity` should
read the certificate instead and the baseline table should be dropped. That
follow-up is not needed for this change.

## What goes away

- `ProtocolKind::GateCheck` and `ProtocolKind::OnEntry` as runnable protocols.
  The enum variants stay so that old conversations still load and display;
  `protocol_for` gives them `RetiredProtocol`, which refuses further turns.
- The `GATE_CHECK`, `LEARN_GATE_CHECK` and `ON_ENTRY` recipes;
  `surface/gate-check.md`, `surface/on-entry.md` and `surface/learn.md` (the
  `learn` role doc's "Done when" covers the retrospective);
  `mock_gate_check_reply`. `gate/context.rs` stays as the phase message's
  builder. `gate/response.rs` stays too: old gate-check transcripts are still
  read with it, and `GateAction` names the actions a criterion row offers.
- Agent-judged `gate_criteria` rows. They are deactivated by the seed, like the
  retired design criteria.
- The `Gate` attention kind, and with it the task panel's gate card. A gate
  can no longer need a human; only a phase agent can, and it does that
  through a decision. `NeedsHuman::GateNotPassed` is kept only so a run saved
  before this change still loads; nothing raises it. `NeedsHuman::FailingCriteria` stays for derived criteria that no
  agent can fix, such as missing Files setup or a PR that isn't mergeable.
- `base.md`'s on-entry, gate-check and response-envelope sections, and every
  state doc's "Forward gate rules" and "Exit" sections. Each is replaced by a
  "Done when" checklist, which is what the phase agent and the evaluator both
  judge against.

## What is added

- **Free-text decisions.** `tod-cli decisions ask` without `--option`. The
  card shows only the text field, focused when it is the top request, and has
  its own reason label ("Needs your input"). The mock gets
  `ask <text>` → a free-text decision.
- **`tod-cli phase`**: `status` (gate criteria, certificate, whether it is
  current), `ready`, `certify --note`, `reject --reason`. Documented in
  `cli/phase.md` and pinned by `doc_sync`.
- **Protocols:** `Phase` (one impl, parameterized by state, for proposed,
  design, planning, merged, released and learn) and `Evaluate`. Each has a
  recipe with `stance/autonomous-session`, the state's domain layers,
  `cli/{intro, obligations, plan, decisions, phase}`, and
  `surface/phase.md` / `surface/evaluate.md`.
- **`lifecycle_next`:** `next_step` returns `Advance`, `Phase` or `Evaluate`
  in place of `GateCheck`. The autopilot's `gate()` becomes "evaluate the
  derived criteria; if all clear, advance".
- The **`phase_certifications` table**, with a `journey_changes` trigger. The
  certify / reject / ready journey events come from `Actor::Agent`, or from
  `Actor::User` for "Mark phase done".
- **UI.**
  - The lifecycle panel's "Run gate check" becomes an instant recheck.
  - The panel shows the certificate: who certified it, its note, and whether
    it is current. When it is stale, the panel says what changed.
  - The conversation view's lifecycle button beside Send becomes the phase
    step for states that have a phase agent.
  - The task panel's status label arrows: `x →` means evaluating, and the
    phase agent shows as the state's name.

## Decisions

1. **The `proposed` phase agent first runs when the user starts the runner.**
   Accepting a ticket into a node does not start it.
2. **There are no user-direction gates.** Every "the user directs X" rule and
   every "human look-over" rule is removed from the process docs, for example
   in `proposed.md` and `planning.md`. The only human hold is required PR
   review.
3. **The evaluator does not edit when independent evaluation is on.** It
   sends agent-fixable problems back to the phase agent (`phase reject
   --fix`), so they are fixed without the user. When the setting is off, the
   single session fixes what it finds itself.

## Stance for every lifecycle agent

`stance/autonomous-session` states this, and each state's "Done when"
checklist repeats it where it applies:

- Act; don't confirm. Everything is reversible.
- Fix what you can fix confidently. Stop for the user only when the missing
  piece is something only they can supply, and say which piece it is.
- Prefer a multiple-choice question when the answers can reasonably be listed.
  Use free text when they can't (for example, "what is this node for?").

## Work items, in order

0. ~~Fix: `VALID_LIFECYCLE_STATES` in `tod-store/src/settings.rs` was missing
   `pr`.~~ Done. `tod-core` now has a test that the two lists agree.
Items 1–7 are done.

1. Free-text decisions: the CLI, the card, and the mock directive. This is
   independent of the rest and fixes the dead end on its own once (4) lands.
2. The `phase_certifications` table, the digest functions per state, and
   `tod-cli phase` with docs and doc_sync. Also the `<state>.phase-certified`
   and `learn-recorded` derived criteria, the seed changes, and deactivating
   the agent-judged criteria.
3. The setting: the `TodSettings` field, the Settings view's Lifecycle section,
   and threading it through `ConversationConfig` and the supervisor.
4. The `Phase` and `Evaluate` protocols, recipes and surfaces; the new
   `next_step` and autopilot loop; mock support.
5. UI: the lifecycle panel (certificate, recheck, Mark phase done), the
   conversation view's lifecycle step, and the task panel labels. Remove the
   `Gate` attention kind.
6. Process docs: `base.md` and each state's "Done when";
   `domain/lifecycle.md`; `doc/agent-context-map.md`; `doc/ui/task-panel.md`;
   `doc/ui/unified-view.md`; `doc/journeys/spec.md`.
7. Delete the gate-check and on-entry protocols, recipes and surfaces, and
   their tests.
