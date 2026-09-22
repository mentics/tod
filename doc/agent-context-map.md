# Agent context map

What every agent-facing surface in tod actually needs in its prompt, and the
building blocks that let each one ask for exactly that.

Sections 1–2 describe the state this restructuring started from; §3–6 are the
design; §7 records what is built and what is left. Surfaces 1, 9, and 10 have
since been removed (the conversation view, surface 11, replaced them); they
stay in §1–2 as history and are marked removed elsewhere.

## 1. The surfaces

There are **eleven** places the app assembles a prompt for an agent. They arrive
through **two independent channels** that know nothing about each other:

- **M — media context** (`crates/tod/media/context/*.md` via
  `tod_core::media::load_static_context`)
- **P — process bundle** (`assets/process/agents/**` via
  `tod_core::process_bundle::launch`)

| # | Surface | Launched by | Channel | Stance | Mutates? | Static content today |
|---|---------|-------------|---------|--------|----------|----------------------|
| 1 | Obligations chat *(removed)* | chat icon / Ctrl+J in obligations panel | M | interactive | yes | `app`, `tod_cli`, `interactive`, `obligations` |
| 2 | Visual-design chat | design panel chat | M | interactive | yes (1 cmd) | `app`, `interactive`, `design/visual-design` |
| 3 | Implementation session | Active-phase **Implement** button | M | autonomous | yes | `app`, `tod_cli`, `active/implement` |
| 4 | Gate check | **Run gate check** in lifecycle panel | M + P | one-shot | no | state role doc + `app`, `gate_check` |
| 5 | On-entry turn | automatic, on lifecycle change | M + P | autonomous | yes | state role doc + `app`, `tod_cli`, `on_entry` |
| 6 | Fleet autonomous run | agent launched into a worktree | P | autonomous | yes | state role doc only |
| 7 | Interview question-maker | `interview::driver` | P | agent-to-agent | yes | role + phase + interview base |
| 8 | Interview answer-processor | `interview::driver` | P | agent-to-agent | yes | role + phase + interview base |
| 9 | Drafting drafter *(removed)* | `drafting::driver` | P | agent-to-agent | yes | drafter doc + drafting base |
| 10 | Drafting capture *(removed)* | `drafting::driver` | P | agent-to-agent | yes | capture doc + drafting base |
| 11 | Conversation | Ctrl+J anywhere, `proposed`/`design` nodes, app nav (`conversation::driver`) | M | interactive | yes | `surface/conversation` (see §4) |

Each also gets a **dynamic block**: surfaces 1–3 from
`agent_context::render_dynamic` / `render_implement_dynamic`, 4–5 from
`gate::context`, 6 from `build_fleet_agent_prompt`, 7–8 from
`interview::context::{snapshot, delta}`, 9–10 from `drafting::context::snapshot`
(both removed), and 11 from the `Focus` block plus
`conversation::context::{delta, resume_snapshot}`.

## 2. What is wrong today

### 2.1 `app.md` is not universal, and its framing is wrong for most surfaces

It opens with *"You are talking to a user inside tod"*. That is literally true
for **2 of 10** surfaces (1, 2). Surfaces 3–6 are background work with no human
in the loop; 7–10 are one agent talking to another agent's snapshot.

And it only actually reaches surfaces 1–5 at all — the four process-bundle-only
surfaces (6–10) never load it. So the "universal" doc is neither universal in
reach nor correct where it does reach.

### 2.2 The domain model is delivered wholesale or not at all

`app.md`'s "What tod stores" explains outline nodes, obligations, capabilities,
and lifecycle states as one indivisible lump. A gate check needs lifecycle;
it does not need to be told what the `agent` capability is. The visual-design
chat needs obligations; it does not need the lifecycle state list. Meanwhile
the interview agents, which work in obligations all day, get none of it.

### 2.3 `tod_cli.md` is all-or-nothing

141 lines covering four nouns (`node`, `obligations`, `drafting`, `plan`).
Actual need:

| Surface | node | obligations | plan | drafting | visual-design |
|---|---|---|---|---|---|
| Obligations chat | ✓ | ✓ | ✓ | | |
| Visual-design chat | | | | | ✓ |
| Implementation | | ✓ | ✓ | | |
| Gate check | | ✓ (read) | ✓ (read) | | |
| On-entry (`planning`) | | ✓ | ✓ | | |
| Interview / drafting | | ✓ | | ✓ | |

Nobody needs all four. The `drafting` noun ships to every surface that loads
`tod_cli` and is used by none of them.

### 2.4 There is a live self-contradiction in the obligations chat

Surface 1 loads `interactive.md` **and** `obligations.md` in the same prompt:

> `interactive.md`: "Confirm with the user before creating, editing, or
> deleting anything."
>
> `obligations.md`: "create them directly without confirmation … make the
> changes immediately upon request without asking for any confirmation."

Both are in the bundle right now. The surface-specific doc is carrying
behavioral policy that belongs to the stance layer, and it directly negates it.

### 2.5 `tod-cli` syntax is documented in two places

`CLAUDE.md` says `media/context/app.md` is "the one canonical place `tod-cli`
command syntax is documented for agents". That is stale twice over: the
reference moved to `tod_cli.md`, and **twelve** files under `assets/process/`
also describe `tod-cli` usage for the surfaces that never load the media
reference at all.

### 2.6 The docs describe a layering model that no longer exists

`CLAUDE.md`'s "Agent chat context" still describes the ancestor-chain model
(`obligations` → `app.md` then `obligations.md`). That was replaced by an
explicit fragment list.

## 3. The building blocks

Four static categories plus composable dynamic blocks. Ordering in the assembled
prompt is always: **stance → domain → cli → role/task → dynamic**.

### 3.1 Stance — exactly one per surface

Who you are and how you are expected to behave. Mutually exclusive; this is the
layer that owns "ask first" vs "just do it" and "be brief" vs "be thorough".
Nothing else may contain behavioral policy.

| Block | Used by | Says |
|---|---|---|
| `stance/interactive-chat.md` | 2, 11 | Side panel chat with a human. Keep replies short. The selection's text is inlined — don't shell out for what you were given. |
| `stance/autonomous-session.md` | 3, 5, 6 | Work autonomously; questions go unanswered until the turn ends. Act directly, don't ask permission, be as verbose as the work needs. Report what you did unless the surface says otherwise. |
| `stance/one-shot.md` | 4 | Single turn, no follow-up. If you can't decide, say so in the structured reply rather than asking a question. |
| `stance/agent-to-agent.md` | 7, 8 | Your counterpart is an agent, your input is a snapshot, your output is parsed. No conversational filler. |

Confirmation policy moves out of the surface docs entirely. Where a surface
must act without confirming, that is a *scoped exception* stated once in its
`surface/` doc and phrased as an exception to the stance, not a flat
contradiction of it. `surface/conversation.md` is the live example (surface 1's
`surface/obligations.md` was the first, and went with that surface).

### 3.2 Domain — pick what the surface touches

| Block | Content | Needed by |
|---|---|---|
| `domain/outline.md` | Nodes, hierarchy, slugs, ids | all |
| `domain/obligations.md` | Requirements vs constraints, inheritance, ordering | 2–8, 11 |
| `domain/lifecycle.md` | The state list and what advancing means | 3, 4, 5, 6, 11 |
| `domain/capabilities.md` | Per-node optional behaviours, the `agent` capability | 4 (gate criteria reference them) |
| `domain/plan.md` | Plan steps, dependencies, `--satisfies` links | 3, 4, 5, 7, 8, 11 |

`app.md` is deleted; its content is redistributed here. No surface is told what
tod stores in general — only about the concepts it will handle.

### 3.3 CLI — intro plus per-noun fragments

| Block | Content |
|---|---|
| `cli/intro.md` | Where the binary is, `--data-root`, "every mutation goes through `tod-cli`, never raw SQL or files" |
| `cli/node.md` | `node` noun |
| `cli/obligations.md` | `obligations` noun |
| `cli/plan.md` | `plan` noun |
| `cli/capabilities.md` | `capabilities` noun (enable, disable, and configure a node's capabilities) |
| `cli/changeset.md` | `changeset` noun (a conversation's change set and flags) |
| `cli/visual-design.md` | `visual-design save` (currently inlined in `design/visual-design.md`) |

There is one fragment per noun the binary dispatches — as built, that is all
nine: `node`, `obligations`, `plan`, `visual-design`, `content`, `questions`,
`memory`, `interview`, `changeset`. (`drafting` was one until drafting was
removed.)

A surface that mutates nothing (4) loads none of these. Surface 2 loads
`cli/intro` + `cli/visual-design` only. This also gives the process-bundle
surfaces (6–8) something to load instead of re-explaining `tod-cli` in twelve
role docs.

### 3.4 Surface / role — what this specific job is

Unchanged in spirit, minus the behavioral policy that moves to stance:
`surface/implement.md`, `surface/gate-check.md`,
`surface/on-entry.md`, `surface/visual-design.md`, `surface/conversation.md`
(`surface/obligations.md` was removed with the obligations chat). For surfaces 4–8 the
process-bundle role doc plays this part and stays where it is.

### 3.5 Dynamic blocks — composed, not branched

`render_dynamic` currently hard-codes `if request.surface == "obligations"`.
Replace with a list of block renderers the recipe names:

`DataRoot`, `Node`, `SelectedObligation`, `NodeObligations`,
`AncestorObligations`, `Plan`, `GateCriteria`, `Workspace`.

Each surface lists the blocks it wants, in order. No renderer needs to know
which surface it is serving.

## 4. Per-surface recipes

```
1 Obligations chat — removed; Ctrl+J in the obligations panel now opens
  the conversation view (surface 11)

2 Visual-design chat
  stance/interactive-chat, domain/outline, domain/obligations,
  cli/intro, cli/visual-design, surface/visual-design
  dyn: DataRoot, Node, AncestorObligations, SelectedObligation

3 Implementation session
  stance/autonomous-session, domain/outline, domain/obligations, domain/plan,
  domain/lifecycle, cli/intro, cli/obligations, cli/plan, cli/tests,
  cli/secrets, surface/implement
  dyn: DataRoot, Node, Plan, NodeObligations, AncestorObligations, Workspace

4 Gate check
  stance/one-shot, domain/outline, domain/obligations, domain/lifecycle,
  domain/capabilities, domain/plan, surface/gate-check   [+ state role doc]
  dyn: DataRoot, Node, GateCriteria, NodeObligations, Plan

5 On-entry
  stance/autonomous-session, domain/outline, domain/obligations,
  domain/lifecycle, domain/plan, cli/intro, cli/obligations, cli/plan,
  surface/on-entry                                       [+ state role doc]
  dyn: DataRoot, Node, NodeObligations, Plan

6 Fleet autonomous run
  stance/autonomous-session, domain/outline, domain/obligations,
  domain/lifecycle, cli/intro, cli/node, cli/obligations, cli/plan
                                                         [+ state role doc]
  dyn: DataRoot, Node, Workspace

7,8 Interview agents
  stance/agent-to-agent, domain/outline, domain/obligations, domain/plan,
  cli/intro, cli/obligations, cli/content, cli/plan, cli/questions,
  cli/memory, cli/interview                     [+ role + phase + interview base]
  dyn: interview snapshot / delta (unchanged)

9,10 Drafting agents — removed with drafting (schema v37); the
  conversation view (surface 11) replaced them

11 Conversation
  stance/interactive-chat, domain/outline, domain/obligations, domain/plan,
  domain/lifecycle, domain/capabilities, cli/intro, cli/node,
  cli/changeset, surface/conversation
  + situational (by focus and lifecycle state): cli/obligations, cli/content,
    cli/plan; every other noun through `tod-cli help`
  dyn: DataRoot, Focus
  (+ per turn: conversation::context::delta; on rotation:
   conversation::context::resume_snapshot)
```

The conversation surface is an exception to the interactive stance: it acts
without confirming, because every change is recorded in its change set and can
be reversed there. Its reply rule (say nothing about what changed) is also
stated in `surface/conversation.md`.

## 5. One assembler

Today media-channel surfaces go through `build_first_message` /
`build_implement_message` / `build_gate_check_message` / `build_on_entry_message`,
and process-channel surfaces through `interview_session_prefix` /
`drafting_session_prefix` / `build_fleet_agent_prompt`. Seven builders, two
vocabularies.

Collapse to one:

```rust
pub struct ContextRecipe<'a> {
    /// Static media fragments, in order.
    pub layers: &'a [&'a str],
    /// Process-bundle role doc, already read (keeps tod-core's media module
    /// free of ProcessManifest, as gate::context does today).
    pub role_doc: Option<&'a str>,
    /// Dynamic blocks to render, in order.
    pub blocks: &'a [DynamicBlock],
}
```

Each surface becomes one `const` recipe next to the code that launches it.
Adding a surface means writing a recipe, not a builder.

## 6. Guardrails

Structural mistakes should fail a test, not a prompt dump review:

1. **Every fragment referenced by a recipe exists.** A test walks all recipes
   and asserts each `.md` resolves — today a typo silently drops a fragment
   (`load_static_context` skips missing keys by design).
2. **Every fragment is referenced by at least one recipe.** Catches orphans
   left behind by a split.
3. **Exactly one `stance/` fragment per recipe.** Directly prevents §2.4.
4. **Snapshot test per surface** over the static half of the prompt, so a change
   to a shared fragment shows which surfaces it reaches.
5. **No `tod-cli` command syntax outside `cli/`.** A grep test over
   `media/context/` and `assets/process/` for `tod-cli ` in a fenced block,
   with an explicit allowlist.

## 7. Status

All four steps are done.

1. **Split the files.** `stance/`, `domain/`, `cli/`, `surface/` created;
   `app.md`, `tod_cli.md`, `interactive.md` and the old surface docs
   redistributed and deleted. Resolves §2.1-2.4.
2. **Guardrail tests.** All five in §6, plus three more that fell out of the
   registry: own-obligations must be followed by ancestor context, a recipe
   loading `cli/` must render the data root, and the interview prefix must
   carry its media fragments ahead of its role docs.
3. **Dynamic blocks composed.** `crate::dynamic` owns the renderers; a surface
   names the blocks it wants. The `surface == "obligations"` branch is gone —
   the panel's "no obligation selected" remark is now a `fallback` the recipe
   supplies, so the renderer no longer knows who it serves.
4. **One assembler.** `context_recipes::build_message` replaced the separate
   builders in `agent_context` and `gate::context`, and
   `process_bundle::launch` now composes media fragments onto the three
   process-bundle surfaces via `with_static_context`. Every live surface is
   registered in `ALL_RECIPES`: seven recipes today (conversation,
   visual-design chat, implement, gate check, on-entry, fleet autonomous, and
   one interview recipe for both interview roles).

### Gaps closed since

- **The fleet run's workspace is modelled.** `DynamicBlock::Workspace` renders
  repo, branch, cwd and notes, and `NodeSelection` gained `slug`, so
  `build_fleet_agent_prompt` no longer hand-rolls a Task block. Only its
  closing Instruction is still surface-specific.
- **The `cli/` fragments are pinned to the binary.** They had already drifted:
  `obligations add` requires `--phase` and the fragment omitted it, `node list`
  takes `--list`, `visual-design` has `show` and `clear`. Four of the
  nouns (`content`, `questions`, `memory`, `interview`) had no fragment at all,
  so the interview agents could only work from the copy in their role doc.
  Every noun now has one, checked against each noun's own `USAGE` string by
  `tod-cli`'s `doc_sync` tests: every verb documented, no verb invented.
- **The process docs no longer carry command tables.** `interview/base.md`
  points at the `cli/` fragments its recipe loads, keeping
  only what is genuinely theirs (how ids appear in their snapshots). A test
  enforces this over all of `assets/process/`.

- **The obligations chat is gone.** The conversation view (surface 11)
  replaced it: Ctrl+J from the obligations panel opens the conversation about
  the selected obligation, and `OBLIGATIONS_CHAT` and `surface/obligations.md`
  were deleted. The `SelectedObligation { fallback }` block stays; the
  visual-design chat uses it.
- **Drafting is gone.** Surfaces 9 and 10, `DRAFTING_AGENT`, `cli/drafting.md`,
  and `assets/process/agents/drafting/` were removed with the drafting view
  (schema v37). Proposed and design nodes open the conversation view.

### Decisions

**The interview snapshot is not a block.** The interview surfaces take their
static fragments from their recipe like every other surface, but their dynamic
half stays as the `snapshot` function in `interview::context`. Blocks exist so
that surfaces needing the same thing share one renderer, and moving the
snapshot into blocks would share nothing:

- Most of the snapshot is used by the interview only (questions, memory,
  content). Each would become a block with one user.
- The sections that do overlap with existing blocks are filtered by phase and
  by role. Blocks are deliberately unaware of which surface they serve, and
  teaching them either filter would bring back the per-surface branching they
  replaced.
- The snapshot queries the database directly, and `DynamicContext`
  deliberately carries no connection.

The conversation surface is the opposite case: its opening state is an
ordinary block (`Focus`), which `conversation::context` loads into a
`FocusSelection` and `dynamic` renders.

The per-turn `delta` is a separate concern: it reports what changed since a
session's change-log watermark (for a conversation: the user's own edits and
reversals, and items changed elsewhere, since its previous turn), not the
current state, so it has no block counterpart either way. Line formats stay consistent across snapshot, delta,
and blocks because all three use `node_context`'s line renderers
(`obligation_line`, `plan_step_line`). The interview snapshot's header and
obligations-by-kind writers also live there
(`write_snapshot_header`, `write_obligations_by_kind`). A node inherits one
thing of each Spec ancestor besides its constraints — the ancestor's summary —
and `render_inherited_context` is the only place that renders it.

**Role docs may name a `tod-cli` noun in prose.** The prose says *which* noun
applies, and the `cli/` fragment says how to call it. Command tables are what
is banned.
