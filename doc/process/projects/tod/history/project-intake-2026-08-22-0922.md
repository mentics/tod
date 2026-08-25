# Project intake — tod — 2026-08-22

## Session

**Agent:** Process agent resumed project creation. Scaffold verified at `doc/process/projects/tod/` (`user.md`, `state.md`, `history/`, `doc/`). Lifecycle state `defining`, mode `interactive`. Beginning project intake interview.

**User request:** "Follow the skill /process to create a new project here called tod"

**Prior session:** `project-intake-2026-08-22-0720.md` — scaffold only; no user answers recorded.

---

## Q1 — Overview & purpose

**Question:** What is **tod** — what does the name mean, and what problem or capability should this project grouping organize work around?

**Answer:** The project is to build an agent fleet management application.

<!-- answer-anchor: Q1 -->

---

## Q2 — Goal & success criteria

**Question:** What is the **overarching goal** for work under this project, and what **measurable success criteria** will tell you the current aims are met?

**Answer:** The overall goal is to make it extremely efficient for a software engineer to work with many agents, on the order of a hundred simultaneously, while being able to examine work at both a high level and a low level and everywhere in between.

<!-- answer-anchor: Q2 -->

---

## Goal approval

**Question:** Approve this proposed `user.md` Goal wording?

```markdown
## Goal

Make it extremely efficient for a software engineer to work with many agents — on the order of a hundred simultaneously — with the ability to examine work at any granularity: high level, low level, and everywhere in between.
```

**Answer:** Yes, what you have there for the proposed goal is the goal exactly.

<!-- answer-anchor: goal-approval -->

---

## Resume — 2026-08-22 (orchestrator)

Session resumed. Config `queue` path updated from legacy `question-queue.md` to `queue/` directory. Empty `queue/` created; researcher to migrate/create per-question files. No new user answer this turn.

---

## q-004 — Overview approval

**Question:** Approve this proposed `user.md` Overview wording?

```markdown
## Overview

tod is an agent fleet management application: it helps a software engineer run and examine many agents as one operable system.
```

**(Presented earlier in chat as “q-002” / Overview approval; current queue id is q-004.)**

**Answer:** For question number two, that overview sounds accurate, but the problem is it's redundant with the goal. I don't see a reason to have both written separately. Either you need to take the two and separate them into overview and goal, or just have one section.

<!-- answer-anchor: q-004 -->

---

## q-005 — Overview / Goal structure

**Question:** How should Overview and Goal appear in `user.md` given they currently overlap?

**A)** Keep both sections — Overview = what tod *is* (product/category); Goal = the efficiency/scale outcome we already approved
**B)** Collapse to one section — keep the approved Goal text only; leave Overview empty / omit it for intake

**Answer:** B — Just keep the goal. That's satisfactory, I think.

<!-- answer-anchor: q-005 -->

---

## q-003 — Deployment / platform constraint

**Question:** What deployment / platform constraint should apply to all tod tasks for now?

**A)** Local-first on the engineer’s own machine (desktop or local web UI); no shared hosted service required for v1
**B)** Browser app usable from anywhere, with a hosted or remote backend assumed
**C)** No platform constraint yet — leave Constraints empty until a later decision
**D)** Something else — name the constraint

**Answer:** For question three, the deployment platform, this is going to be a locally run application, but it'll be an application that interacts with many, many different things.

**Interpretation (for processors):** Maps to local-first / Constraints. Do not invent extra constraints beyond local run; retain “interacts with many different things” verbatim for possible follow-up (integrations vs multi-host ambiguity).

<!-- answer-anchor: q-003 -->

---

## Resume — 2026-08-22 (orchestrator, /process no new answers)

Session resumed with no new user answer. Open queue unchanged (q-001, q-002, q-006, q-007). Researcher and answer processor both idle; spawn/resume requested fire-and-forget (orchestrator has no Task tool this turn — parent should background-spawn if not already running).

---

## q-001 — Measurable success criterion

**Question:** Which measurable success criterion should we lock in first for intake?

**A)** Scale: one engineer can supervise ~100 concurrent agents without needing a second person for routine monitoring
**B)** Drill-down: from fleet overview to one agent's low-level work detail in a single continuous navigation path
**C)** Situational awareness: answer "what is the fleet doing right now?" from the primary UI without opening external tools
**D)** Something else — name a different check

**Recommend:** A — it anchors the goal's "~100 simultaneously" scale in an observable operator outcome.
Which do you want?

**Answer:** Expanded beyond option A as written: Must manage 100+ concurrent agents, particularly with no degradation in performance. Should include soft and hard limits on resource usage (CPU, RAM, or similar).

<!-- answer-anchor: q-001 -->

---

## q-002 — Explicit non-goal (skipped)

**Question:** Which explicit non-goal should we record first for this grouping?

**A)** Not a multi-tenant / org-wide SaaS product in v1 — solo-engineer (or small personal) use only
**B)** Not an agent runtime or model-hosting platform — tod orchestrates/examines agents, it does not replace their engines
**C)** Not CI-only headless orchestration — interactive examination is in scope; pure pipeline bots without a human operator UI are out
**D)** Something else — name a different non-goal

**Recommend:** B — keeps the fleet-management boundary clear against building another agent framework.
Which do you want?

**Answer:** Skip this question. Explicit skip/waiver: do not lock a non-goal from q-002.

<!-- answer-anchor: q-002 -->

---

## q-006 — Diversity of local-app interactions

**Question:** When you say the local app “interacts with many, many different things,” what is the primary kind of diversity?

**A)** Many external integrations/APIs/services (issue trackers, clouds, tools, etc.)
**B)** Many hosts/machines — agents and resources spread across a fleet of machines
**C)** Many agent backends/runtimes/providers (different agent engines tod must talk to)
**D)** A mix of the above — name which mix matters for v1

**Recommend:** C — fleet management of ~100 agents most often implies heterogeneous agent backends first.
Which do you want?

**Answer:** Mix / specifics (not plain C): Integrating with local applications like a code editor. Need to run Claude terminals, possibly command lines. Need to run Claude VMs — micro VMs with blaxel.ai. Running Claude terminals or Claude SDK inside those.

<!-- answer-anchor: q-006 -->

---

## q-007 — Constraints wording (modify)

**Question:** Approve this proposed Constraints wording for local run?

```markdown
## Constraints

Runs as a local application on the engineer’s machine (desktop or local web UI); no shared hosted service is required for v1.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — leave Constraints empty for now

Pick A, B, or C?

**Answer:** B — Modify. Just that it runs as a local desktop application. Do not keep the broader “desktop or local web UI / no shared hosted service” wording unless the human later expands it.

<!-- answer-anchor: q-007 -->

---

## q-008 — Invariant pick

**Question:** Which invariant should we refine into the set next?

**A)** Multi-granularity stays first-class: fleet overview and per-agent low-level detail must both remain reachable in-product
**B)** Human-in-the-loop: a human operator can always inspect and intervene; unattended-only is not the default design point
**C)** Local operability: core fleet supervision must remain usable without a shared hosted service
**D)** Something else — name a different invariant

**Answer:** A and B are both interesting, and we should continue drilling down on those.

**Interpretation (for processors):** Dual pick — refine/drill both multi-granularity (A) and human-in-the-loop (B). Do not lock C or D. No accepted invariant markdown yet — propose wording via follow-up refine questions; do not write Invariants into `user.md` until wording is accepted.

<!-- answer-anchor: q-008 -->

---

## q-009 — Links / prior art

**Question:** Are there existing design docs, repos, or related projects that tasks under tod must obey?

**A)** None yet — leave Links empty for intake
**B)** Yes — name the doc(s)/repo(s)/link(s)
**C)** Deferred — add Links later when design exists

**Answer:** Yes, there are. We started implementing something like this in a VS Code extension. I exported all the features from it into a file mentioned below. This needs to have a lot of the same functionality, but it is a very different approach. The previous extension was also called TOD, so don't get confused. It was focused on tasks, which this new one will also focus on, but it will also be much more scalable, as it is intended to deal with many, many tasks at the same time. And so, although we can kind of use that old feature list as inspiration, we are neither limited to it nor are we necessarily going to implement it as it's described there.

`.local\agent\process\projects\tod\refs\features-from-tod-extension.md`

**Interpretation (for processors):** Maps to Links = B with one named prior-art ref. Relationship is inspiration only (shared task focus + overlapping functionality themes); not a binding requirements doc; new approach and higher concurrency scale. Name collision: prior VS Code extension also called TOD. Do not copy feature-list detail into `user.md`; park overflow for design. Propose Links wording for approval; do not edit `user.md` until wording accepted.

<!-- answer-anchor: q-009 -->


---

## q-011 — Constraints wording (local desktop)

**Question:** Approve this proposed Constraints wording for local desktop?

```markdown
## Constraints

Runs as a local desktop application on the engineer’s machine.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — leave Constraints empty for now

Pick A, B, or C?

**Answer:** Yes, that is fine. (A — Accept as written)

**Interpretation (for processors):** Accepted Constraints wording as proposed. Apply to `user.md`. Does not yet include integration-kind Constraints (see q-013).

<!-- answer-anchor: q-011 -->

---

## q-013 — Additional Constraints (integration kinds)

**Question:** Which additional Constraints item (integration kinds from intake) should we refine into the set next?

**A)** Must integrate with local applications (e.g. a code editor) as part of the operator workflow
**B)** Must manage terminal / CLI agent sessions under tod
**C)** Must support micro-VM agent hosts (agent runtimes inside VMs)
**D)** None of these as Constraints yet — leave vendor/runtime specifics to design/tasks
**E)** Something else — name a different constraint

**Recommend:** D — kinds are parked in to-process for design; avoid locking Blaxel/Claude vendors into Constraints now.
Which should we refine into the set next?

**Answer:** 1. Yes  2. Yes!  3. Yes!  4. Yes! — but all these need more detail.

**Interpretation (for processors):** Multi-pick — refine **A**, **B**, and **C** into Constraints (not D “none yet”). Item **4** maps to the volunteered fourth kind from q-006 overflow: agent terminals/SDK running **inside** micro-VMs (not a separate letter on this question). Human wants all of these as Constraints directionally, but **rejects locking current short wording** — each needs more detail via refine questions before acceptance. Do **not** write A–C (or item 4) into `user.md` until detailed wording is accepted. Keep vendor names (Blaxel, Claude) in `to-process.md`; Constraints should stay kind-level once refined. Researcher should open refine questions for A, B, C, and in-VM agent runtime/SDK hosting.

<!-- answer-anchor: q-013 -->

---

## q-016 — Links wording (prior TOD extension)

**Question:** Approve this proposed Links wording for the prior TOD extension feature export?

```markdown
## Links

- Prior TOD VS Code/Cursor extension feature export (inspiration only; not binding; overlapping task functionality, different approach, higher concurrency scale): `.local/agent/process/projects/tod/refs/features-from-tod-extension.md`
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — leave Links empty for now

Pick A, B, or C?

**Answer:** We don't need Links — ignore / leave Links empty. However, keep the mentioned feature-export file around; we will need to reference it later to identify which parts we will maintain and how it will influence this new project.

**Interpretation (for processors):** Maps to **C** — do not write Links into `user.md`. Retain `.local/agent/process/projects/tod/refs/features-from-tod-extension.md` (do not discard). Later work must review that export to decide which parts to maintain and how they influence this project — park that follow-up in `to-process.md`; not an intake Links entry.

<!-- answer-anchor: q-016 -->

---

## q-017 — Where multi-granularity / HITL land

**Question:** Multi-granularity and human-in-the-loop were dropped as Invariants — where should they land for intake?

**A)** Success criteria — refine into measurable checks next
**B)** Constraints — refine as hard limits next
**C)** Drop both from intake for now; leave Invariants empty
**D)** Something else — say where each goes

**Recommend:** A — both are operator outcomes from the approved Goal, not cross-cutting “must remain true” invariants.
Which do you want?

**Answer:** Multi-granularity and human-in-the-loop are kind of requirements. Is that the same as success criteria?

**Interpretation (for processors):** Not a completed A/B/C/D pick — clarifying question about taxonomy. Treat both topics as still in-scope requirements directionally; do **not** write them into `user.md` yet. Do **not** delete `queue/q-017.md` — rewrite it to briefly distinguish Success criteria vs Constraints vs Invariants and re-ask placement. Do not guess that “requirements” means Success criteria.

<!-- answer-anchor: q-017 -->

---

## q-018 — Next success criterion to refine

**Question:** Which measurable success criterion should we refine into the set next? (Scale/CPU-RAM wording was dropped as invalid.)

**A)** Drill-down: from fleet overview to one agent's low-level work detail in a single continuous navigation path
**B)** Situational awareness: answer "what is the fleet doing right now?" from the primary UI without opening external tools
**C)** Concurrent tasks: one engineer can operate many concurrent tasks/agents as the primary unit of work in tod
**D)** Something else — name a different check

**Recommend:** C — matches the Goal’s scale intent and the prior-extension “many tasks at once” framing without the rejected resource-limit wording.
Which should we refine into the set next?

**Answer:** Unsure what “trail down” means. Situational awareness is a feature we will need a requirement for. Concurrent tasks is a very big one — we will need that.

**Interpretation (for processors):** Pursue **B** and **C** into Success criteria / requirements refine next (C is high priority). **A** (“Drill-down”) is unclear to the human (heard/read as “trail down”) — do not lock A wording; researcher may rephrase/explain drill-down separately if still needed. No accepted Success criteria markdown yet — do not write to `user.md`.

<!-- answer-anchor: q-018 -->

---

## q-019 — Constraints wording (local-app integration)

**Question:** Approve this more-detailed Constraints wording for local-app integration?

```markdown
- Must integrate with local applications the engineer already uses (at least a code editor) so agent work can be examined and acted on without leaving the normal desktop workflow.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop this Constraints item

Pick A, B, or C?

**Answer:** Jumping the gun. We need to spell out what the features are, or at least the requirements around those features, before we can finalize the verbiage.

**Interpretation (for processors):** Reject this approval as **premature** (not a permanent drop of the local-app integration need from q-013). Do **not** write this Constraints bullet to `user.md`. Pause further Constraints-wording approvals for integration kinds until features / requirements around them are spelled out first. Update `to-process.md` accordingly.

<!-- answer-anchor: q-019 -->


---

## q-021 — Situational awareness UI scope

**Question:** For situational awareness, what must an engineer be able to answer from the primary UI without opening external tools?

**A)** Aggregate fleet status only (counts, health, busy/idle)
**B)** Aggregate plus per-agent or per-task status at a glance
**C)** Something else — name what must be visible

**Recommend:** B — matches “what is the fleet doing right now?” for many concurrent units.
Which do you want?

**Answer:** Answers A, both A and B, and possibly other things as well.

**Interpretation (for processors):** Minimum accepted direction is **A and B** (aggregate fleet status plus per-agent or per-task status at a glance). Additional visibility beyond A/B is possible but unnamed — do not invent further UI obligations. No accepted Requirements / success-criteria markdown yet — do not write to `user.md`. Delete `queue/q-021.md`; researcher refines measurable SC (and may probe “other things”).

<!-- answer-anchor: q-021 -->

---

## q-022 — Next integration requirement direction

**Question:** Before locking Constraints wording for integrations, which requirement direction should we refine into the set next?

**A)** Integrate with local apps (at least a code editor) in the operator workflow
**B)** Manage terminal / CLI agent sessions under tod
**C)** Support micro-VM agent hosts
**D)** Run agent terminals / SDK inside those micro-VMs
**E)** Something else — name a different requirement direction

**Recommend:** A — you flagged it first; spell requirements before Constraints verbiage.
Which should we refine into the set next?

**Answer:** This is kind of all the above. Yes, we have to do all of those and more.

**Interpretation (for processors):** Pursue **A, B, C, and D** as requirement directions (not Constraints wording yet — still deferred per q-019). “And more” means additional directions may exist but were not named — do not invent them. No accepted Requirements markdown yet — do not write to `user.md`. Delete `queue/q-022.md`; researcher may refine any/all of these into intent + success criteria (and probe unnamed extras).

<!-- answer-anchor: q-022 -->

---

## q-023 — Concurrent-tasks operable unit

**Question:** For the concurrent-tasks requirement, what is the primary operable unit tod must scale around?

**A)** Agents (as in the Goal’s “~100 simultaneously”)
**B)** Tasks (work items; many concurrent tasks as the unit)
**C)** Both — say how they relate for the requirement
**D)** Something else — name the unit

**Recommend:** C — Goal says agents; prior TOD framing emphasized many tasks at once.
Which do you want?

**Answer:** We have to do both.

**Interpretation (for processors):** Directionally **C** — both agents and tasks are in scope for the concurrent-tasks requirement. How they relate for the requirement was **not** stated — do not invent the relationship; do **not** write Requirements wording to `user.md` yet. Do **not** delete `queue/q-023.md` — rewrite it to ask how agents and tasks relate for this requirement.

<!-- answer-anchor: q-023 -->

---

## q-017 — Placement (Requirements vs Constraints vs Invariants)

**Question:** Clarifying your last answer: project `user.md` now tracks **Requirements** as numbered items. Each requirement is an intent statement plus nested measurable **success criteria**. Other sections:

- **Requirements** — what must be achieved (intent + success criteria checks)
- **Constraints** — hard limits on how it must be built or run (e.g. local desktop only)
- **Invariants** — properties that must remain true across the product (you already dropped multi-granularity / HITL from here)

You said multi-granularity and human-in-the-loop are kind of requirements. Where should each land for intake?

**A)** Both as Requirements — refine intent + success criteria next
**B)** Both as Constraints — refine as hard limits next
**C)** Split — say which section for multi-granularity and which for human-in-the-loop
**D)** Drop both from intake for now
**E)** Something else — say where each goes

**Recommend:** A — that matches calling them requirements; success criteria nest under each item.
Which do you want?

**Answer:** Multigranularity and human in the loop are requirements.

**Interpretation (for processors):** Maps to **A** — both land under **Requirements** (intent + nested success criteria to refine next). Not Constraints or Invariants. No accepted intent/SC wording yet — do not write to `user.md`. Delete `queue/q-017.md`; researcher refines each into measurable Requirements items for approval.

<!-- answer-anchor: q-017b -->

---

## q-023 — Agents/tasks relation

**Question:** Clarifying your last answer: for concurrent tasks, you said we have to do **both** agents and tasks.

How should agents and tasks relate in the requirement (what must stay true for intake)?

**A)** Tasks are the work items; each task may involve one or more agents — scale is many concurrent tasks, with agents as the workers under them
**B)** Agents are the primary units (~100); tasks are how work is labeled/organized on agents
**C)** Both must be first-class operable units in the UI/workflow (neither is only a subordinate label) — say any extra rule that ties them
**D)** Something else — say how they relate

**Recommend:** A — matches prior TOD “many tasks at once” plus Goal scale on agents as workers.
Which do you want?

**Answer:** Tasks and agents actually have a many-to-many relationship with each other. Tasks are user-specific. These are all the tasks the user has to worry about, and there could be hundreds of them. Agents are the fleet they're managing that generally is working on those tasks, and they're kind of associating them together. They might have multiple agents working on a single task.

**Interpretation (for processors):** Maps to **D** / enriched **C** — both are first-class; relationship is **many-to-many**. Intake facts to carry into Requirements refine (not yet accepted wording for `user.md`):
- Tasks: user-specific work the engineer must track; scale may be hundreds
- Agents: the managed fleet that generally works on those tasks
- Association: operators associate agents and tasks; multiple agents may work on one task (and many-to-many implies a task may link many agents and an agent may link many tasks)
Do **not** invent extra cardinality rules. No accepted Requirements markdown yet — do not write to `user.md`. Delete `queue/q-023.md`; park detail in `to-process.md`; researcher drafts intent + SC for approval.

<!-- answer-anchor: q-023b -->

---

## q-024 — Agent-scale success check

**Question:** For the concurrent tasks/agents requirement, should the measurable agent-scale success check stay at ~100 simultaneous agents (from the Goal), or a different number?

**A)** ~100 simultaneous agents
**B)** A different number — give it
**C)** No numeric agent-scale check in intake — say what check instead

**Recommend:** A — matches the approved Goal.
Which do you want?

**Answer:** Yes, 100 is fine for the success check.

**Interpretation (for processors):** Maps to **A** — accept **~100 simultaneous agents** as the measurable agent-scale success check for the concurrent tasks/agents requirement. Do not write full Requirements markdown to `user.md` until intent + nested SC wording is offered and accepted; park the accepted number in `to-process.md`. Delete `queue/q-024.md`.

<!-- answer-anchor: q-024 -->

---

## q-025 — Task-count success check

**Question:** For the concurrent tasks/agents requirement, what task-count should the measurable success check use?

**A)** At least ~100 concurrent tasks
**B)** At least ~500 concurrent tasks
**C)** “Hundreds” as soft guidance only — no hard task-count check in intake
**D)** Something else — give a number or check

**Recommend:** A — aligns with Goal scale without inventing a larger bar.
Which do you want?

**Answer:** Success Check 500.

**Interpretation (for processors):** Maps to **B** — accept **at least ~500 concurrent tasks** as the measurable task-count success check. Same as q-024: park accepted number; do not invent full `user.md` Requirements text yet. Delete `queue/q-025.md`.

<!-- answer-anchor: q-025 -->

---

## q-026 — Situational awareness beyond aggregate + per-unit

**Question:** Besides aggregate fleet status and per-agent/per-task status at a glance, must the primary UI show anything else for situational awareness in intake?

**A)** Nothing else for intake — aggregate plus per-unit status is enough
**B)** Yes — name what else must be visible

**Recommend:** A — “other things” were unnamed; lock A+B first.
Which do you want?

**Answer:** Oh, that's a good question. The answer is yes there is other, but I'm not sure what yet. Ask me more to figure it out?

**Interpretation (for processors):** Directionally **B** (there is other) but **incomplete** — nothing else named yet. Do **not** invent additional SA obligations. Do **not** delete `queue/q-026.md` — rewrite it with concrete probes (including candidates volunteered in the same turn under q-027: cloud-VM vs local locus; labels/tags grouping) so the human can pick must-vs-nice. Do not write to `user.md`.

<!-- answer-anchor: q-026 -->

---

## q-027 — Multi-granularity fail case

**Question:** For the multi-granularity requirement, what would fail it — examination that stops too coarse?

**A)** Stopping at fleet-only (no per-task / per-agent examination)
**B)** Stopping at per-agent or per-task summary (no live / low-level work detail)
**C)** Something else — name the fail case

**Recommend:** B — Goal requires low-level examination, not only summaries.
Which do you want?

**Answer:** Multi-granularity — not sure how to answer the A/B framing. We need to see: (1) status for the entire fleet; (2) what's going on for this particular task; (3) what's currently being run in cloud VMs; (4) what's running locally. It would be nice to group tasks together by labels or tags or whatever — stuff like that might be useful.

**Interpretation (for processors):** Not a clean A/B pick — reframed as required examination surfaces. Intake direction (must-level, pending accepted wording): operator can examine **fleet-wide status**, **per-task status**, **cloud-VM work**, and **local work**. **Labels/tags grouping** was volunteered as nice-to-have / useful, **not** as a hard fail criterion — do not promote tags to Requirements until explicitly required. Do not invent low-level “live detail” as accepted SC from this answer alone (A/B not chosen). No `user.md` write. Delete `queue/q-027.md`; park in `to-process.md`; researcher drafts multi-granularity intent + SC for approval. Same-turn candidates may inform modified q-026 probes.

<!-- answer-anchor: q-027 -->

---

## q-031 — Micro-VM host ownership boundary

**Question:** For micro-VM agent hosts, what must tod own in intake?

**A)** Attach/supervise agents already running in VMs (VM lifecycle elsewhere)
**B)** Provision/start/stop micro-VM hosts as part of tod
**C)** Something else — name the boundary

**Recommend:** A — keeps VM vendor lifecycle out of intake until design.
Which do you want?

**Answer:** Mostly, tod is going to: launch them; supervise; manage; stop; start — there generally are a lot. They're mostly tied to tasks specifically — e.g. here's a task: launch a micro-VM host to work on that task. Then we might want to attach to the console, or write it's for running Claude in there or something. We might want to attach to it or detach and switch it between autonomous mode versus interactive mode, that kind of thing. Also, manage visibility into usage: how many VM seconds are we using over some time period; what is the current rate of burn (how many VM seconds per second are we running right now); or something like that.

**Interpretation (for processors):** Maps to **C** / beyond **B** — tod owns micro-VM **lifecycle and operations**, not attach-only (**A** rejected). Intake requirement direction (no accepted wording yet — do not write `user.md`):
- Lifecycle: launch / start / stop (and broadly manage / supervise) micro-VM hosts
- Task-tied: provisioning/ops are generally in service of a user task (e.g. launch a host to work on that task)
- Console: attach / detach
- Mode: switch between autonomous vs interactive
- Usage visibility: VM-seconds over a period; current burn rate (VM-seconds per second) or equivalent
Vendor/runtime specifics (e.g. Claude in the VM) and exact metric formulas → `to-process.md` for design; keep *kinds* in intake. Delete `queue/q-031.md`; researcher drafts Requirements intent + SC for approval.

<!-- answer-anchor: q-031 -->

---

## q-026 — Situational awareness / primary UI (clarifying)

**Question:** Clarifying: you said situational awareness needs **something beyond** aggregate fleet status and per-agent/per-task status, but you are not sure what yet.

From your multi-granularity notes, which of these (if any) must the **primary UI** support for situational awareness in intake?

**A)** Distinguish / see work running in **cloud VMs** vs **locally** (runtime locus)
**B)** **Group tasks** by labels or tags (you called this nice-to-have — only pick if it is actually required for intake)
**C)** Both A and B as must-haves for intake
**D)** Neither of those — name a different “what else” must be visible
**E)** On second thought, nothing else for intake — aggregate + per-unit status is enough

**Recommend:** A — matches runtime diversity already in scope; keep labels/tags optional unless you want them required.
Which do you want?

**Answer:** “Primary” depends on what I’m doing at the time — may be different views by focus (persona/view). Sometimes a task-management view (organize tasks, mark statuses); sometimes an agent view (how many running/stuck, responding to them — responding is really important). There will be a primary UI for task management and a primary UI for agent management; they are closely related and I have to do both together. Proactive thinking: I have this task, I need to get it done, what’s the status? Reactive: when agents need information or get unstuck, notifications — details like here’s the task, agents on it, this agent has this question, go respond. Cloud VM vs local is less important as a differentiation: it needs to be visible but isn’t a primary UI concept — more a tag / agent-type on an agent. Grouping: we will want to group by tasks and by tag. Status is important. Sorting and filtering tasks is really important. Task-based UI is probably the primary UI, but there would definitely be a significant UI focused on the agent side too.

**Interpretation (for processors):** Not a clean A–E pick — reframes “primary UI” and names must-level SA / workflow surfaces. Intake direction (no accepted wording — do not write `user.md`):
- Dual closely related UIs: **task management** (likely the main primary) and **agent management** (significant); operator does both together; focus-dependent views
- **Proactive:** task-centric status / get-it-done
- **Reactive / HITL:** notifications when agents block or need info; drill to task + agents + the agent’s question and respond (responding is important)
- **Cloud vs local:** must be **visible**, but **not** a primary UI concept — treat as tag / agent-type attribute (revises q-027 “primary surface” framing)
- **Grouping:** required to group by **tasks** and by **tag** (upgrades prior “nice-to-have” labels/tags)
- **Task list ops:** status, **sorting and filtering** tasks are important
Delete `queue/q-026.md`; park in `to-process.md` (incl. design overflow for view/persona model); researcher drafts SA / related Requirements + SC for approval. Cross-inform HITL and multi-granularity items.

<!-- answer-anchor: q-026b -->

---

## q-028 — Human-in-the-loop: always from tod

**Question:** For the human-in-the-loop requirement, what must the engineer always be able to do from tod?

**A)** Inspect only (see work; intervention happens outside tod)
**B)** Inspect and intervene (e.g. pause, redirect, or stop work from tod)
**C)** Something else — name the must-have

**Recommend:** B — “in the loop” usually means intervene, not only watch.
Which do you want?

**Answer:** I’ll always be able to do it from tod. Inspect and intervene — both and more. They can look at what the agent is doing, interrupt it, submit (kind of like an interruptible prompt), and cancel it; stop it; export it; view its transcript; view all the things it’s done; view any changes it’s made in files; and more. Focusing on HITL: passively, agents get to points where they need human questions / hit a blocker requiring human interaction — that needs a notification process and a queue of notifications to manage. The user goes through that queue, sees what task it is about, which agents are involved, and responds to the request.

**Interpretation (for processors):** Maps to **B and more** (**C**) — always from tod (not outside). Rejects **A** (inspect-only). Intake HITL direction (no accepted wording — do not write `user.md`):
- Active control from tod: inspect live work; interrupt / interruptible-prompt submit; cancel/stop; export; view transcript / history of actions; view file changes made by the agent (and related ops — list is illustrative, not an exhaustive locked checklist)
- Passive HITL: notification process + **managed notification queue** when agents need human input or hit blockers; from a notification, see task + involved agents + respond to the request
Aligns with / deepens q-026b reactive path. Delete `queue/q-028.md`; park in `to-process.md`; researcher drafts Requirements intent + measurable SC for approval (do not treat every verb as a separate locked SC until offered and accepted).

<!-- answer-anchor: q-028 -->

---

## q-029 — Local-app integration set

**Question:** For the local-app integration requirement, which apps must tod integrate with for intake?

**A)** At least one code editor (e.g. VS Code / Cursor) — name which if you care
**B)** Code editor plus other named local apps — list them
**C)** Something else — name the app set

**Recommend:** A — you already named a code editor as the example.
Which do you want?

**Answer:** Definitely the code editor — right now the focus is Zed. Also, we’ll be able to run agents locally; in particular Claude Code, probably in a terminal, but we need to be able to drive it. We’ve talked about using Claude SDK in the cloud micro-VM; ideally use the same mechanism both in micro-VM and locally if possible — that probably needs a spike to research.

**Interpretation (for processors):** Maps to **A** (code editor) with named current focus **Zed**, plus local **agent-driving** (Claude Code, likely terminal) which also belongs with terminal/CLI / in-VM SDK requirement directions — not only “other desktop apps.” Intake kinds (no accepted Requirements/Constraints wording — do not write `user.md`):
- Local code-editor integration; current product focus = Zed (vendor name → `to-process` / design; kind = code editor for intake drafts)
- Drive local agents (Claude Code / terminal) from tod
- Prefer **one mechanism** for agent runtime local and in micro-VM if feasible — **spike/research** needed (park as open research item; do not invent feasibility)
Delete `queue/q-029.md`; update `to-process.md`; researcher drafts requirement wording for approval separately from Constraints polish (q-019).

<!-- answer-anchor: q-029 -->

---

## q-030 — Terminal / CLI session management boundary

**Question:** For managing terminal / CLI agent sessions under tod, what must tod be able to do in intake?

**A)** View/attach to sessions tod did not necessarily start
**B)** Start and supervise sessions from tod
**C)** Both start and attach/view
**D)** Something else — name the capability

**Recommend:** C — fleet management usually needs both.
Which do you want?

**Answer:** I don’t think we’ll do any attachment to sessions that tod did not start, at least not initially. Maybe someday, but it’s not the initial goal — that gets really hard; it depends on how they were started and we don’t have a communication process there, so A is probably a no. Of course, we definitely want to start and supervise sessions from tod.

**Interpretation (for processors):** Maps to **B** — start and supervise sessions from tod. **Rejects A** (and thus **C**) for initial intake: no attach/view of sessions tod did not start. “Maybe someday” is out of scope for initial Requirements — do not write as a current obligation. No accepted Requirements wording yet — do not write `user.md`. Delete `queue/q-030.md`; park in `to-process.md`.

<!-- answer-anchor: q-030 -->

---

## q-032 — Meaning of simultaneous / concurrent scale

**Question:** For the ~100-agent and ~500-task scale checks, what does “simultaneous / concurrent” mean?

**A)** Actively running or working at the same time
**B)** Tracked/managed in tod (may be idle)
**C)** Something else — define it

**Recommend:** A — matches fleet-management “at once” scale.
Which do you want?

**Answer:** Actively running or working at the same time — probably just trying to run 100 agents at the same time might be a problem (vendor limits). There would be 500 tasks in the UI and 100 agents showing up in the UI; they might not be actively running. That brings up whether actively running means processing them (e.g. transcripts): we won’t have to process transcripts in real time — we’ll just store them locally; that should be fine. We should have a scale test requirement, maybe 10 local agents and 10 VM agents running at the same time as a scalability test.

**Interpretation (for processors):** For the **~100-agent / ~500-task** checks, maps to **B** (tracked/managed in tod UI; may be idle) — not **A** at that magnitude (vendor limits make 100 actively running problematic). Separately, intake should include an **active-run scale-test** direction: on the order of **~10 local + ~10 VM agents running at the same time** (human’s “maybe” numbers — park; refine into accepted SC wording later). Transcript handling: **store locally**; **no** real-time transcript-processing obligation from this answer. No accepted Requirements markdown yet — do not write `user.md`. Delete `queue/q-032.md`; update `to-process.md`.

<!-- answer-anchor: q-032 -->

---

## q-033 — Performance / resource-limit success check

**Question:** Beyond ~100 agents and ~500 concurrent tasks, must intake include a performance or resource-limit success check?

**A)** No — those capacity numbers are enough for intake
**B)** Yes — name an observable performance check
**C)** Soft/hard CPU/RAM (or similar) limits — give numbers, or say defer to design

**Recommend:** A — earlier CPU/RAM wording was dropped; keep intake on capacity unless you want a concrete check now.
Which do you want?

**Answer:** Yeah, we do want those. Kind of arbitrarily: the application itself should be under 500 MB of RAM (really should be much smaller; doesn’t need that much — settle on 500 MB). CPU: if literally doing nothing, virtually zero — maybe under 2% or something, but it should be zero at idle. Even when the user is clicking around on the UI, CPU should still be under 5%.

**Interpretation (for processors):** Maps to **C** — include resource-limit success checks for the **tod application itself** (not agent-workload RAM). Accepted numeric direction pending formal Requirements/Constraints wording approval:
- RAM: application under **500 MB**
- CPU idle (doing nothing): virtually **zero** (human floated “under 2%” as a possible proxy but emphasized idle should be zero — refine measurable idle check carefully; do not invent a different number)
- CPU during light UI interaction (clicking around): under **5%**
Earlier dropped vague CPU/RAM scale wording is superseded by these app-focused checks. No accepted markdown yet — do not write `user.md`. Delete `queue/q-033.md`; park in `to-process.md`.

<!-- answer-anchor: q-033 -->

---

## q-034 — Other integration requirement directions

**Question:** Besides local apps, terminal/CLI sessions, micro-VM hosts, and in-VM agent runtimes, must intake name any other integration requirement direction now?

**A)** No — those four are enough for intake
**B)** Yes — name the additional direction

**Recommend:** A — “and more” was unnamed; avoid inventing extras.
Which do you want?

**Answer:** “In-VM agent runtimes” isn’t something I remember saying — likely a dictation/framing issue; we’re not doing “in-VM agent runtimes” as a thing. We have: local apps / the code editor; terminal CLI sessions; local running agents (maybe that was mislabeled “in-VM” — confusing; just local running agents: spawn agents locally and interact with them); micro-VM hosts / micro-VM agents running in micro-VMs. Other integrations: maybe phase two integrate with Cursor IDE. That’s everything I can think of at the moment — useful to double-check `refs/features-from-tod-extension.md` for integrations not named here. Remembered after: want to integrate with **Linear**; also **Slack** and **GitHub** at the very least.

**Processor review of features-from-tod-extension.md (integrations mentioned):** VS Code/Cursor host; Treehouse; Git; Linear; GitHub PRs; Claude Wrap / terminal sessions; browser open for issue/PR URLs. **Slack is not in that export** — human-volunteered. Do not auto-promote Treehouse/Git/Claude Wrap/VS Code-host as new intake Requirements unless human accepts them; human explicitly named Linear, Slack, GitHub (+ phase-two Cursor).

**Interpretation (for processors):** Maps to **B**, plus taxonomy correction. Replace bogus “in-VM agent runtimes” kind with **local running agents** (spawn/interact locally). Keep micro-VM hosts/agents as separate. Additional requirement directions for intake (no accepted wording — do not write `user.md`):
- Linear
- Slack
- GitHub (at least)
- Cursor IDE — phase two / later (not initial must unless refined)
Export candidates noted for researcher probe only: Treehouse, Git, prior-extension VS Code/Cursor host patterns. Delete `queue/q-034.md`; update `to-process.md` (fix q-013/q-022 kinds).

<!-- answer-anchor: q-034 -->

---

## q-035 — Approve Micro-VM agent hosts Requirements item

**Question:** Approve this proposed Requirements item for micro-VM agent hosts?

```markdown
### Micro-VM agent hosts

tod owns lifecycle and operations for micro-VM agent hosts used on the engineer’s tasks — including launch, start, stop, and supervise — plus console attach/detach and switching hosts between autonomous and interactive modes.

**Success criteria:**
- From tod, launch, start, stop, and supervise micro-VM hosts generally tied to a user task
- From tod, attach to and detach from a host console
- From tod, switch a host between autonomous and interactive mode
- tod surfaces VM usage over a time period (VM-seconds or equivalent) and current burn rate (VM-seconds per second or equivalent)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Mostly correct but confusing. We run an agent on a micro-VM and interact with the agent there. Switching hosts between autonomous and interactive is wrong — switch **running agents** between autonomous and interactive (be more precise). First success-criterion bullet needs more precise verification process wording: launch and start are not the same; verify in order — launch; verify launched; verify status that it’s running; supervise/manage; interact; stop and verify stopped. Generally tied to a user task. Third bullet is okay (with agent precision). Do **not** go into how it will be tested — too detailed for intake; that’s planning.

**Interpretation (for processors):** Maps to **B** — do **not** accept as written; do **not** write this draft (or an invented rewrite) into `user.md` until a revised wording is explicitly accepted. Do **not** delete `queue/q-035.md` — rewrite the proposed markdown with: (1) agent-on-micro-VM framing; (2) mode switch on **agents** not hosts; (3) ordered lifecycle/verify SC without test-procedure detail; (4) keep usage bullet; (5) ask A/B/C again. Park notes in `to-process.md`.

<!-- answer-anchor: q-035 -->

---

## q-036 — Approve Situational awareness Requirements item

**Question:** Approve this proposed Requirements item for situational awareness?

```markdown
### Situational awareness

tod provides closely related task-management and agent-management UIs so the engineer can see what work is doing and organize it without leaving tod.

**Success criteria:**
- Aggregate fleet status and per-agent or per-task status are visible at a glance
- Both a task-management UI and an agent-management UI are available and usable together
- Work can be grouped by task and by tag; cloud-VM vs local is visible as an attribute/tag (not a separate primary UI concept)
- Tasks can be sorted and filtered; task status is visible in the task-management UI
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** First success-criteria bullet should be **per agent and per task** — so we can look at it for either one. Other than that minor tweak, it sounds fine.

**Interpretation (for processors):** Maps to **B** — not accept-as-written. Change SC1 from “per-agent **or** per-task” to “per-agent **and** per-task” (both views available). Do **not** write to `user.md` until revised wording is accepted. Rewrite `queue/q-036.md` with the tweak and re-ask A/B/C. Update `to-process.md`.

<!-- answer-anchor: q-036 -->

---

## q-037 — HITL managed notification queue as intake SC

**Question:** For human-in-the-loop, must a managed notification queue (when agents need human input or hit blockers) be an intake success criterion?

**A)** Yes — required for intake
**B)** No — defer the notification queue to design; keep inspect + intervene controls only for now

**Recommend:** A — you called responding via that queue really important.
Which do you want?

**Answer:** We need to manage the notification queue because we need to be able to take action on the notifications as they come in. We need to make sure that we don’t miss any, so those notifications are persistent until they are acted upon and resolved.

**Interpretation (for processors):** Maps to **A** — managed notification queue is a required intake success-criterion direction. Additional accepted facts: act on notifications as they arrive; notifications are **persistent until acted upon and resolved** (don’t miss any). No full Requirements markdown accepted yet — do not write `user.md`. Delete `queue/q-037.md`; update `to-process.md` (HITL / q-028).

<!-- answer-anchor: q-037 -->

---

## q-038 — In-VM agent terminal/SDK capability (dismissed)

**Question:** For agent terminals / SDK inside micro-VMs tod manages, what must tod be able to do in intake?

**A)** Launch and supervise those in-VM agent runtimes from tod
**B)** Attach/view only to runtimes started elsewhere in the VM
**C)** Both launch/supervise and attach/view
**D)** Something else — name the capability

**Recommend:** C — matches owning the micro-VM host plus driving agents inside it.
Which do you want?

**Answer:** Dismiss question 38 and revisit if you already have enough details for that.

**Interpretation (for processors):** Explicit **waive/dismiss** for now. Prior answers already cover the substance: q-031/q-035 (tod owns lifecycle + ops for agents on micro-VMs, including launch/stop/supervise/interact, console attach/detach, mode switch); q-030 (start/supervise from tod; no attach to sessions tod did not start — apply same boundary unless later overridden); q-034 (drop “in-VM agent runtimes” as a confusing kind; use local agents + micro-VM agents). Do **not** invent a new pick among A–D. Do **not** write `user.md`. Delete `queue/q-038.md`. Park: researcher may reopen only if a real gap remains after drafting micro-VM agent Requirements from existing direction.

<!-- answer-anchor: q-038 -->

---

## q-039 — Approve Terminal / CLI agent sessions Requirements item

**Question:** Approve this proposed Requirements item for terminal / CLI agent sessions?

```markdown
### Terminal / CLI agent sessions

tod starts and supervises terminal / CLI agent sessions used in the operator workflow. Initial scope does not require attaching to sessions tod did not start.

**Success criteria:**
- From tod, start terminal / CLI agent sessions
- From tod, supervise sessions that tod started
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Confused whether this is redundant or separate from the other kinds. Put another way: what we need is that tod can launch, interact with, manage, and supervise agent sessions in various places and modes — local and remote. Unsure whether interactions for local vs remote are the same; they are **exactly the same**, so do **not** define them separately.

**Interpretation (for processors):** Maps to **B** (and partially **C** on this siloed draft) — do **not** accept this terminal-only item as written. Human wants **one** agent-session capability set (launch, interact, manage, supervise) that applies across loci (local and remote/micro-VM), not separate local-vs-remote requirement definitions. Keep “no attach to sessions tod did not start” unless later overridden (q-030). Do **not** write `user.md`. Rewrite `queue/q-039.md` with a unified agent-sessions draft for re-approval; note in `to-process.md` that parallel micro-VM / terminal drafts (q-035 etc.) should be consolidated rather than duplicated.

<!-- answer-anchor: q-039 -->

---

## q-041 — Idle-CPU success-check bound

**Question:** For the tod application idle-CPU success check, which measurable bound should intake use?

**A)** Under 1% when idle (doing nothing)
**B)** Under 2% when idle
**C)** Something else — give a number or check

**Recommend:** B — you floated under 2% while wanting idle near zero.
Which do you want?

**Answer:** Thought we already established this, but that’s okay. Under 2% is fine.

**Interpretation (for processors):** Maps to **B** — accept **under 2% when idle** as the measurable idle-CPU success-check bound for the tod application. Updates q-033 direction (resolve prior “virtually zero / maybe under 2%” ambiguity). No full Requirements/Constraints markdown accepted yet — do not write `user.md`. Delete `queue/q-041.md`; update `to-process.md`.

<!-- answer-anchor: q-041 -->

---

## q-035 — Approve Micro-VM agents Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Micro-VM agents

tod owns lifecycle and operations for agents running on micro-VM hosts for the engineer’s tasks — including launch and stop as distinct operations, supervise/manage, and interaction with the agent on the micro-VM — plus console attach/detach and switching running agents between autonomous and interactive modes.

**Success criteria:**
- From tod, for an agent on a micro-VM generally tied to a user task: launch it; verify it has launched; verify its status shows it is running; supervise/manage it; interact with it; stop it and verify it has stopped
- From tod, attach to and detach from the agent’s console
- From tod, switch a running agent between autonomous and interactive mode
- tod surfaces VM usage over a time period (VM-seconds or equivalent) and current burn rate (VM-seconds per second or equivalent)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Prefer defining this requirement as more of a “tod can manage agents and the following environments,” then numbered bullet points for local and micro-VM. Then “The following operations are supported,” with bullet points of the supported operations. Under success criteria: “For each of the target environments, we know tod can do the following,” then the list of actions that can be taken. Requirements for local and remote are deliberate and the same — do not state them twice, but make sure both are covered.

**Interpretation (for processors):** Maps to **B** — do **not** accept the micro-VM-only draft; do **not** write `user.md` until a revised unified draft is explicitly accepted. Rewrite `queue/q-035.md` into environments + shared operations + per-environment SC shape (local and micro-VM listed once; operations stated once). Carry forward prior ops direction (launch → verify launched → verify running → supervise/manage → interact → console attach/detach → mode switch → stop/verify stopped) and micro-VM usage SC. Keep q-030 boundary (no attach to sessions tod did not start) in the draft body. **Obsolete `queue/q-039.md`** — this answer is the preferred unified local+remote shape that q-039 was seeking. Update `to-process.md`.

<!-- answer-anchor: q-035-r2 -->

---

## q-036 — Approve Situational awareness Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Situational awareness

tod provides closely related task-management and agent-management UIs so the engineer can see what work is doing and organize it without leaving tod.

**Success criteria:**
- Aggregate fleet status and per-agent and per-task status are visible at a glance
- Both a task-management UI and an agent-management UI are available and usable together
- Work can be grouped by task and by tag; cloud-VM vs local is visible as an attribute/tag (not a separate primary UI concept)
- Tasks can be sorted and filtered; task status is visible in the task-management UI
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Generally correct; tighten verbiage. Requirement body: “tod provides integrated task management and agent management UIs.” SC second bullet: “Integrated task management and agent management UIs are available.” Replace third and fourth bullets with one: tasks and agents can be sorted, filtered, and grouped (design later defines sort/filter keys). Keyboard efficiency is important but separate — want a cross-cutting requirement that all actions in tod can be efficiently accomplished via the keyboard (not part of this item).

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-036.md` with tightened body + SC (keep glance status SC unless dropped; tighten UI-available SC; single sort/filter/group SC for tasks and agents). Do **not** create a new queue file for keyboard efficiency — park as open separate Requirements/Constraints direction in `to-process.md` for the researcher. Do not fold keyboard into this item.

<!-- answer-anchor: q-036-r2 -->

---

## q-040 — Approve Concurrent tasks and agents Requirements item

**Question:** Approve this proposed Requirements item for concurrent tasks and agents?

```markdown
### Concurrent tasks and agents

Operators manage tasks and agents as first-class units with a many-to-many association (including multiple agents on one task). Tasks are the user-specific work set; agents are the managed fleet associated with those tasks.

**Success criteria:**
- tod can track/manage at least ~100 agents in the UI (agents may be idle)
- tod can track/manage at least ~500 tasks in the UI (tasks may be idle)
- Scale test: at least ~10 local agents and ~10 micro-VM agents can run actively at the same time
- Agent transcripts are stored locally; real-time transcript processing is not required for this scale
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Use “users” (not “operators”) here and be consistent elsewhere — people who use tod are users unless talking about some other kind of thing. SC third bullet: point is ~10 local and ~10 micro-VM agents can run concurrently / simultaneously — prefer “run concurrently” (or “can be running simultaneously”). Drop the transcripts bullet from this item’s success criteria — agent transcripts are available from the UI (not necessarily stored locally) and may be a requirement somewhere else, but not a success criterion on this particular requirement.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-040.md`: users terminology; keep ~100 agents / ~500 tasks UI track/manage SCs; simplify concurrent-run SC to ~10 local + ~10 micro-VM can run concurrently; remove transcripts SC from this item. Terminology: prefer **users** for tod end-users in open queue drafts — update other open queue files that say “operator(s)” to “user(s)” (q-042, q-043). Park transcript availability (from UI; not necessarily local storage) as separate Requirements direction in `to-process.md` for researcher — do not invent a queue file here.

<!-- answer-anchor: q-040 -->

---

## q-042 — Approve Human-in-the-loop Requirements item

**Question:** Approve this proposed Requirements item for human-in-the-loop?

```markdown
### Human-in-the-loop

The engineer can always inspect agent work and intervene from tod, including acting on agent questions and blockers through a managed notification queue.

**Success criteria:**
- From tod, inspect what an agent is doing and intervene (at least interrupt or interruptible-prompt submit, and cancel/stop)
- tod maintains a managed notification queue when agents need human input or hit blockers
- Notifications persist until acted upon and resolved
- From a notification, the user can see the related task, involved agents, and respond
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Agent transcripts belong on this item more than on concurrent tasks — this is where the user can see agent transcripts and intervene. Generally good. Do not use “questions” wording. Rest of the item looks good.

**Interpretation (for processors):** Maps to **B** (accept substance with edits) — do **not** write `user.md` until revised wording explicitly accepted. Rewrite `queue/q-042.md`: (1) use **user** not engineer; (2) include viewing agent transcripts in body + inspect SC; (3) drop “agent questions” — keep human input / blockers / notification queue; (4) keep remaining SCs. Absorb q-040 transcripts park into this item’s direction. Re-ask A/B/C.

<!-- answer-anchor: q-042 -->

---

## q-043 — Approve Multi-granularity examination Requirements item

**Question:** Approve this proposed Requirements item for multi-granularity examination?

```markdown
### Multi-granularity examination

Users can examine work in tod at multiple granularities — fleet-wide, per task, and per agent — including work running locally and on micro-VMs.

**Success criteria:**
- Fleet-wide status is examinable in tod
- Status for a particular task is examinable in tod
- Status for a particular agent is examinable in tod
- Work running locally and work running on micro-VMs is examinable in tod (locus visible as an attribute/tag, not a separate primary UI concept)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Looks redundant — asks whether a requirement already covers this; feels very familiar / already covered.

**Interpretation (for processors):** Maps to **C** — drop as redundant. Covered by Situational awareness (q-036): aggregate fleet status and per-agent and per-task status at a glance, plus integrated task/agent management UIs; local vs micro-VM locus as attribute/tag remains prior direction (q-026b) under SA design overflow / environments (q-035), not a separate Multi-granularity Requirements item. Do **not** write `user.md`. Delete `queue/q-043.md`. Update `to-process.md` (q-027 / multi-granularity absorbed into SA).

<!-- answer-anchor: q-043 -->

---

## q-044 — Approve Application resource limits Requirements item

**Question:** Approve this proposed Requirements item for tod application resource limits?

```markdown
### Application resource limits

The tod application itself stays within fixed resource bounds on the engineer’s machine (separate from agent or vendor workload limits).

**Success criteria:**
- Application RAM stays under 500 MB
- Application CPU stays under 2% when idle (doing nothing)
- Application CPU stays under 5% during light UI interaction (clicking around)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Just change the third success-criterion bullet to under 5% under normal use.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-044.md`: SC3 → “Application CPU stays under 5% under normal use” (replace light UI interaction / clicking around). Prefer **user’s** machine over engineer’s for terminology consistency (q-040). Keep RAM under 500 MB and idle under 2%. Re-ask A/B/C. Update `to-process.md`.

<!-- answer-anchor: q-044 -->

---

## q-045 — Linear integration minimum capability

**Question:** For Linear integration in requirements gathering, what must tod be able to do at minimum?

**A)** Link tod tasks to existing Linear issues
**B)** Create and/or update Linear issues from tod
**C)** Both link and create/update
**D)** Something else — name it

**Recommend:** C unless you only need one-way linking at first.
Which do you want?

**Answer:** Yes on A; leave off B and C. Need to associate tod tasks with Linear issues — a tod task may have one or more Linear issues listed on it. Additional integration: create a tod task from a Linear issue — user says create new task based on a Linear issue ticket ID; tod goes to Linear, pulls that ticket ID and content, and creates a task based on that.

**Interpretation (for processors):** Accept **A** (associate/link); **reject B and C** (do not require create/update of Linear issues from tod for intake). Additional accepted direction (beyond pure A): create a tod task from a Linear issue by ticket ID (pull content from Linear → create task). Cardinality: one tod task may list one or more Linear issues. No full Requirements markdown accepted yet — do not write `user.md`. Delete `queue/q-045.md`. Park in `to-process.md` for researcher to draft Linear Requirements item later. Do not invent a queue file here.

<!-- answer-anchor: q-045 -->

---

## q-047 — Keyboard efficiency intake check

**Question:** For keyboard efficiency as a separate requirement, what must be true for intake?

**A)** Every user action in tod is reachable via keyboard (no mouse-only-required actions)
**B)** Core workflows are keyboard-complete; some mouse-only affordances may remain
**C)** Something else — name the check

**Recommend:** A — matches “all actions … via the keyboard.”
Which do you want?

**Answer:** Unsure how the “separate requirement” framing reads; reading A as “every user action” (not “every user has a keyboard”). B is not sufficient — someone could do every single thing in tod via the keyboard and never touch the mouse; **A** is what we want.

**Interpretation (for processors):** Maps to **A** — every user action in tod is reachable via keyboard; no mouse-only-required actions; full keyboard-only operation must be possible. Reject B (core-only / allow mouse-only remainders). Confirms prior direction that keyboard efficiency is a **separate** Requirements (or Constraints) item from Situational awareness (q-036-r2). No full Requirements markdown accepted yet — do not write `user.md`. Delete `queue/q-047.md`. Update `to-process.md` for researcher to draft Accept/Modify/Reject wording later. Do not invent a queue file here.

<!-- answer-anchor: q-047 -->

---

## q-035 — Approve Agents and environments Requirements item (round 3)

**Question:** Modified per your last answer — approve this revised Requirements item (one item covering both environments; operations stated once)?

```markdown
### Agents and environments

tod can manage agents and the following environments:
1. Local
2. Micro-VM

Agents are generally tied to a user task. Initial scope does not require attaching to sessions tod did not start.

The following operations are supported:
- Launch an agent
- Verify the agent has launched
- Verify its status shows it is running
- Supervise / manage the agent
- Interact with the agent
- Attach to and detach from the agent’s console
- Switch a running agent between autonomous and interactive mode
- Stop the agent and verify it has stopped

**Success criteria:**
- For each of the target environments, we know tod can do the following: launch an agent; verify it has launched; verify its status shows it is running; supervise/manage it; interact with it; attach to and detach from its console; switch it between autonomous and interactive mode; stop it and verify it has stopped
- For micro-VM environments, tod surfaces VM usage over a time period (VM-seconds or equivalent) and current burn rate (VM-seconds per second or equivalent)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Framing: “tod can manage agents **in** the following environments” (not “and”). Operations = what the user can do: launch an agent; review an agent’s status. Remove supervise/manage (no meaning). Interact → more specific: submit a prompt; interrupt the agent. Remove attach/detach console — primary interaction is submit prompt, view transcript, and interrupt (stop the process and/or submit a new prompt that interrupts current activity). Keep switch autonomous↔interactive. Keep stop agent; do not say “verify it is stopped” (covered by review status). Success criteria: only that in each of the supported environments, the user can do any of the supported operations — do not re-list ops or add extra status/detail SCs; status field meaning and prior micro-VM usage metering details are design/later-phase.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-035.md` accordingly. Park status-field semantics and VM-seconds/burn-rate metering in `to-process.md` as design/later (removed from this item’s SC). Re-ask A/B/C.

<!-- answer-anchor: q-035-r3 -->

---

## q-036 — Approve Situational awareness Requirements item (round 3)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Situational awareness

tod provides integrated task management and agent management UIs.

**Success criteria:**
- Aggregate fleet status and per-agent and per-task status are visible at a glance
- Integrated task management and agent management UIs are available
- Tasks and agents can be sorted, filtered, and grouped
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** That looks fine.

**Interpretation (for processors):** Maps to **A** — accept the revised Situational awareness Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-036.md`.

<!-- answer-anchor: q-036-r3 -->

---

## q-040 — Approve Concurrent tasks and agents Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Concurrent tasks and agents

Users manage tasks and agents as first-class units with a many-to-many association (including multiple agents on one task). Tasks are the user-specific work set; agents are the managed fleet associated with those tasks.

**Success criteria:**
- tod can track/manage at least ~100 agents in the UI (agents may be idle)
- tod can track/manage at least ~500 tasks in the UI (tasks may be idle)
- At least ~10 local agents and ~10 micro-VM agents can run concurrently
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** That looks fine.

**Interpretation (for processors):** Maps to **A** — accept the revised Concurrent tasks and agents Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-040.md`.

<!-- answer-anchor: q-040-r2 -->

---

## q-042 — Approve Human-in-the-loop Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Human-in-the-loop

The user can always inspect agent work and intervene from tod, including viewing agent transcripts and acting on blockers through a managed notification queue.

**Success criteria:**
- From tod, inspect what an agent is doing (including viewing agent transcripts) and intervene (at least interrupt or interruptible-prompt submit, and cancel/stop)
- tod maintains a managed notification queue when agents need human input or hit blockers
- Notifications persist until acted upon and resolved
- From a notification, the user can see the related task, involved agents, and respond
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Most of it is accurate, but redundant with another requirement that already covers user operations (view transcript, intervene, etc.). Avoid duplication for traceability. Pare Human-in-the-loop down to primarily the notification queue — focus is the agent reaching out to the user when it needs help via that queue.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-042.md` to notification-queue focus only; remove inspect/transcript/intervene SCs (owned by Agents and environments / q-035). Keep: managed queue when agents need human input or hit blockers; persist until acted upon and resolved; from a notification see related task, involved agents, and respond. Re-ask A/B/C. Update `to-process.md` (traceability / anti-duplication).

<!-- answer-anchor: q-042-r2 -->

---

## q-044 — Approve Application resource limits Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Application resource limits

The tod application itself stays within fixed resource bounds on the user’s machine (separate from agent or vendor workload limits).

**Success criteria:**
- Application RAM stays under 500 MB
- Application CPU stays under 2% when idle (doing nothing)
- Application CPU stays under 5% under normal use
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** That looks fine.

**Interpretation (for processors):** Maps to **A** — accept the revised Application resource limits Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-044.md`.

<!-- answer-anchor: q-044-r2 -->

---

## q-050 — Slack integration minimum capability

**Question:** For Slack integration in requirements gathering, what must tod be able to do at minimum?

**A)** Surface Slack activity related to work inside tod (e.g. mentions or thread updates)
**B)** Send messages to Slack from tod
**C)** Both surface Slack activity and send messages
**D)** Something else — name it

**Recommend:** A unless outbound posting is a must for intake.
Which do you want?

**Answer:** Not A (surfacing activity would be convenient but is not the ask). Include **B** (send messages to Slack from tod). Primary: **read from Slack when requested** — user copies a link to a Slack channel, thread, or message (or a channel name), pastes it into tod; tod goes and reads that content for whatever current action the user is working on. Biggest use case: pull content from Slack based on the user pointing at where to go. Support both URLs and channel names.

**Interpretation (for processors):** Reject A as the intake minimum (proactive surface mentions/thread updates). Accept **B** (send messages from tod). Primary accepted direction is on-demand **read** via user-supplied Slack URL (channel/thread/message) or channel **name** — pull content into the user’s current action. Not a pure A/B/C pick — record as B + read-on-request (D-shaped). No full Requirements markdown accepted yet — do not write `user.md`. Delete `queue/q-050.md`. Park in `to-process.md` for researcher to draft Slack Requirements later. Do not invent a queue file here.

<!-- answer-anchor: q-050 -->

---

## q-048 — Approve Keyboard efficiency Requirements item

**Question:** Approve this proposed Requirements item for keyboard efficiency?

```markdown
### Keyboard efficiency

Every user action in tod is reachable via the keyboard; a user can operate tod fully without using the mouse.

**Success criteria:**
- Every user action available in tod is reachable via keyboard (no mouse-only-required actions)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** OK, for question 48. Revisit question 48. We updated the process and guidelines to avoid this redundancy.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. The draft’s Success criteria only paraphrase the requirement statement (artifact discipline / no paraphrase duplication). Drop the Success criteria block; keep the measurable statement (aligned with q-047 **A**). Rewrite `queue/q-048.md` accordingly and re-ask A/B/C. Update `to-process.md`.

<!-- answer-anchor: q-048 -->

---

## q-042 — Approve Human-in-the-loop Requirements item (round 3)

**Question:** Modified per your last answer — approve this revised Requirements item (notification-queue focus; inspect/intervene/transcript ops live under Agents and environments)?

```markdown
### Human-in-the-loop

When an agent needs help, it can reach the user through a managed notification queue in tod.

**Success criteria:**
- tod maintains a managed notification queue when agents need human input or hit blockers
- Notifications persist until acted upon and resolved
- From a notification, the user can see the related task, involved agents, and respond
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** All right, for question 42, yeah, that looks good.

**Interpretation (for processors):** Maps to **A** — accept the revised Human-in-the-loop Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-042.md`.

<!-- answer-anchor: q-042-r3 -->

---

## q-051 — Approve Slack integration Requirements item

**Question:** Approve this proposed Requirements item for Slack integration?

```markdown
### Slack integration

Users can send messages to Slack from tod and, on demand, read Slack content by pasting a Slack URL (channel, thread, or message) or a channel name; tod fetches that content for the user’s current action. Proactive surfacing of Slack activity (mentions or thread updates) inside tod is not required for this item.

**Success criteria:**
- From tod, send a message to Slack
- Given a pasted Slack URL to a channel, thread, or message, tod fetches that content for the user’s current action
- Given a pasted Slack channel name, tod fetches that channel’s content for the user’s current action
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** For question 51, you do not have to specify things that are not requirements. If it's not specified as a requirement, then it's not a requirement. Don't need to say it, so we remove that part. Here's the thing: users are not going to be sending messages to Slack. We will not have a particular feature, and actually, it's probably going to be a phase two. Let's mark that phase two, or just a future phase. I guess we can do that. The future phase requirement will have various notification channels for the user, where Todd will send messages to the user through various channels to notify them of specific activity that they've requested notifications for. One of the channels for those notifications will be Slack, so I can specify that. This particular requirement is more about just the underlying Slack support, so it can be very pretty simple. Sending a message to Slack, you could mark that as phase two. I don't know what we want to say, not in phase one, maybe? Maybe just mark it not in phase one, or just mark it phase two. Let's just mark it phase two. That'll be simpler. Sending a message to Slack is phase two. The rest of that sounds pretty good. The other clarification would just be the way it's worded: "Todd fetches that content, maybe to use in the user's current action." The way it's written, it's ambiguous: is it fetching content from the user's current action or to it? Clarify the verbiage there.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-051.md`: (1) remove the “proactive surfacing … is not required” non-requirement sentence; (2) remove send-message from this item — park **send messages to Slack** as **phase two**; (3) keep on-demand read (URL / channel name) as the simple underlying Slack support item; (4) clarify fetch wording to “fetches that Slack content **to use in** the user’s current action” (not ambiguous from/to). Park future-phase overflow: multi-channel user notifications (activity the user requested), with Slack as one channel. Re-ask A/B/C. Do not invent new queue files here.

<!-- answer-anchor: q-051 -->

---

## q-053 — Code-editor integration minimum capability

**Question:** For code-editor integration in requirements gathering, what must tod be able to do at minimum?

**A)** From tod, open or focus a file (and optionally a line) in the configured code editor
**B)** From the code editor, open or focus the related tod task or agent
**C)** Both directions
**D)** Something else — name the must-have editor actions

**Recommend:** A unless round-trip from the editor is required for intake.
Which do you want?

**Answer:** For question 53, yes, A. not B

**Interpretation (for processors):** Maps to **A** — from tod, open or focus a file (and optionally a line) in the configured code editor. Explicitly **not B** — do not require opening/focusing related tod task or agent from the code editor for intake. No full Requirements markdown accepted yet — do not write `user.md`. Delete `queue/q-053.md`. Park in `to-process.md` for researcher to draft editor Requirements later. Do not invent a queue file here.

<!-- answer-anchor: q-053 -->

---

## q-054 — Shared drive mechanism for local and micro-VM

**Question:** For driving agents both locally and in micro-VMs, is one shared drive mechanism required for intake, or only preferred if feasible?

**A)** Preferred if feasible — separate local vs micro-VM drive paths are allowed if a spike shows one mechanism cannot cover both
**B)** Required — intake must commit to one shared drive mechanism for local and micro-VM
**C)** Something else — name it

**Recommend:** A — matches “if possible” / spike language already given.
Which do you want?

**Answer:** For question 54: there's not a requirement for a single drive mechanism. However, it's determined that it has to be done, so I guess the answer is A, because if it's feasible, it's always good to not have duplication or redundancy or whatever, right? It certainly would be preferred if it can, but it doesn't have to be.

**Interpretation (for processors):** Maps to **A** — shared drive mechanism is **preferred if feasible**, not a hard intake requirement (**not B**). Separate local vs micro-VM drive paths allowed if one mechanism cannot cover both. Preference (avoid duplication) is design/spike guidance — do **not** invent a Requirements item mandating one mechanism. Do not write `user.md`. Delete `queue/q-054.md`. Update `to-process.md` (align q-029 spike language). Do not invent a queue file here.

<!-- answer-anchor: q-054 -->

---

## q-035 — Approve Agents and environments Requirements item (round 4)

**Question:** Modified per your last answer — approve this revised Requirements item?

```markdown
### Agents and environments

tod can manage agents in the following environments:
1. Local
2. Micro-VM

Agents are generally tied to a user task. Initial scope does not require attaching to sessions tod did not start.

The following operations are supported (what the user can do):
- Launch an agent
- Review an agent’s status
- Submit a prompt to an agent
- View an agent’s transcript
- Interrupt an agent (stop the process and/or submit a new prompt that interrupts its current activity)
- Switch a running agent between autonomous and interactive mode
- Stop an agent

**Success criteria:**
- In each of the supported environments, the user can do any of the supported operations
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** OK, for question 35, you can just leave off the success criteria, and then this is approved. Just go ahead and go with it, as long as you just delete the success criteria.

**Interpretation (for processors):** Accept with explicit modification — drop the Success criteria block; accept the remainder as written. Record accepted wording (no SC) in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-035.md` (no re-ask needed). Aligns with artifact discipline (omit SC that only restates environments × ops).

<!-- answer-anchor: q-035-r4 -->

---

## q-048 — Approve Keyboard efficiency Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item (no Success criteria; statement is already measurable)?

```markdown
### Keyboard efficiency

Every user action in tod is reachable via the keyboard; a user can operate tod fully without using the mouse.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Okay, for question 48, that's correct.

**Interpretation (for processors):** Maps to **A** — accept the revised Keyboard efficiency Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-048.md`.

<!-- answer-anchor: q-048-r2 -->

---

## q-051 — Approve Slack integration Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item (on-demand Slack read only; send-to-Slack is phase two)?

```markdown
### Slack integration

Users can, on demand, read Slack content by pasting a Slack URL (channel, thread, or message) or a channel name; tod fetches that Slack content to use in the user’s current action.

**Success criteria:**
1. Given a pasted Slack URL to a channel, thread, or message, tod fetches that content to use in the user’s current action
2. Given a pasted Slack channel name, tod fetches that channel’s content to use in the user’s current action
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 51. This is another example where I think it would be better to write it as just the requirement, no success criteria, and just have it be a bullet list of what can be done.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-051.md`: drop Success criteria; fold capabilities into the requirement as a bullet/numbered list of what can be done (on-demand Slack read via URL or channel name; fetch to use in the user’s current action). Keep send-to-Slack as phase two (not in this item). Re-ask A/B/C.

<!-- answer-anchor: q-051-r2 -->

---

## q-052 — GitHub integration minimum capability

**Question:** For GitHub integration in requirements gathering, what must tod be able to do at minimum?

**A)** Associate tod tasks with existing GitHub issues and/or pull requests
**B)** Create and/or update GitHub issues or PRs from tod
**C)** Both associate and create/update
**D)** Something else — name it

**Recommend:** A unless write-back to GitHub is required for intake.
Which do you want?

**Answer:** Question 52. We can associate tasks with pull requests. I think that's it for now. The one other thing, though, would be to open a browser to the associated pull request.

**Interpretation (for processors):** Associate tod tasks with **pull requests** (PR-focused; not expanding to create/update — **not B**). Additional must-have: from tod, **open a browser** to the associated pull request. Issues association not stated — do not invent. No full Requirements markdown accepted yet — do not write `user.md`. Delete `queue/q-052.md`. Park in `to-process.md` for researcher to draft GitHub Requirements later. Do not invent a queue file here.

<!-- answer-anchor: q-052 -->

---

## q-055 — Approve Code editor integration Requirements item

**Question:** Approve this proposed Requirements item for code-editor integration?

```markdown
### Code editor integration

From tod, the user can open or focus a file (and optionally a line) in the configured code editor.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** All right, question 55. You can approve that as written. However, there's a feature that hasn't come up yet: all these different agents are most likely going to be working either on different file systems or different work trees, and definitely on different branches. One of the important things about this code editor integration is that we need to be able to view the code that any particular agent is working on. That means we need to be able to open up a particular branch in the code editor. I guess that actually does affect this particular requirement because we need to be able to open up a particular branch in the code editor. We need to open up a specific file in it, and in some cases, we need to open up maybe diffs in it. I'm still not quite sure. I want a way that we can show the diffs. I don't know if we can. The change set for an agent session: what changes did they make? I want to see if it's possible. I don't know if that's possible to do that or not, but that's probably something we need to launch a spike on at some point. Is whether we can show this agent session, these are the changes they made, and show that inside the code editor

**Interpretation (for processors):** Maps to **B** (self-corrected after “approve as written”) — do **not** accept the file-only draft. Expand Code editor integration: from tod, open a **particular branch** in the configured editor and open/focus a **specific file** (optional line), so the user can view the code an agent is working on (agents likely on different filesystems/worktrees/branches). Diffs / showing an agent session’s **changeset** inside the editor — desired but uncertain; park as **design spike** (feasibility), not locked Requirements wording yet. Rewrite `queue/q-055.md` with branch + file capabilities (no SC block; capability list). Re-ask A/B/C. Do not write `user.md`. Do not invent a queue file for the spike.

<!-- answer-anchor: q-055 -->

---

## q-056 — Approve Linear integration Requirements item

**Question:** Approve this proposed Requirements item for Linear integration?

```markdown
### Linear integration

Users can associate tod tasks with Linear issues and create tod tasks from Linear issues.

**Success criteria:**
1. A tod task can list one or more linked Linear issues
2. From tod, create a new task from a Linear issue ticket ID (tod pulls the issue content and creates the task)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** All right, question 56: I think I mentioned it somewhere else, so you might have caught this, but we also need to be able to launch a browser to the associated Linear issue. I just realized that Linear is required for us to support Linear as an issue tracker, just like it's required for us to support GitHub as a code repository. However, the actual implementation of those, I want them to be kind of generic so that, in the future, maybe we'll add support for other code repositories and other issue trackers. There is a requirement that the integration for issue trackers and the integrations for code repositories are kind of generic. We support specific features for all issue trackers and all code repositories, and the requirement is to support Linear for issue tracking and GitHub for code repositories, so we should clarify these in that way.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-056.md`: (1) add open/launch browser to associated issue; (2) reframe as **issue tracker integration** with a shared capability set; **Linear** is the required issue tracker for intake; (3) drop SC block — capability list in the requirement (match recent style); keep associate (one or more) + create tod task from ticket ID + open browser. Park parallel framing for **code repository integration** (shared capabilities; **GitHub** required; associate PRs + open browser per q-052) in `to-process.md` for researcher — do **not** invent a queue file here. Update Linear/GitHub to-process notes. Re-ask A/B/C.

<!-- answer-anchor: q-056 -->

---

## q-057 — External integration kinds completeness

**Question:** For requirements gathering, these are the external integration kinds named so far:

1. Code editor
2. Linear
3. Slack
4. GitHub

Any others to include at this point?

**A)** No — these are enough for now
**B)** Yes — name them

**Recommend:** A unless you know a gap.
Which do you want?

**Answer:** Easy to externalize, I guess. Kind of the same so far:
- Code editor
- Linear
- Slack
- GitHub
Other than the agents themselves, those are the only integrations that I know of. As I just mentioned, we support integrations with issue trackers, and Linear is the one we support in Phase 1. We support integrations with code repositories, and GitHub is the one we support in Phase 1.

**Interpretation (for processors):** Maps to **A** — no additional external integration kinds beyond those named (aside from agents themselves, which are not this list). Taxonomy clarification (align q-056): prefer **issue tracker** (Linear = Phase 1) and **code repository** (GitHub = Phase 1) as the generic kinds, with Linear/GitHub as the Phase 1 implementations — not forever-vendor-only kinds. Slack and code editor remain as named. Do not write `user.md`. Delete `queue/q-057.md`. Update `to-process.md`. Do not invent queue files here.

<!-- answer-anchor: q-057 -->

---

## q-058 — Desktop platforms

**Question:** Which desktop platforms must tod support in requirements gathering?

**A)** The user’s current primary desktop OS only (name it if you care which)
**B)** Windows, macOS, and Linux
**C)** Something else — name the set

**Recommend:** A unless a multi-OS first release is required.
Which do you want?

**Answer:** Okay, for question 58: B, we're going to support all three.

**Interpretation (for processors):** Maps to **B** — Windows, macOS, and Linux. No full Requirements/Constraints markdown accepted yet — do not write `user.md`. Delete `queue/q-058.md`. Park in `to-process.md` for researcher to draft platform support wording (likely Constraints alongside local-desktop). Do not invent a queue file here.

<!-- answer-anchor: q-058 -->

---

## q-059 — Write accepted Requirements to user.md

**Question:** OK to add these already-accepted Requirements items to project user.md?

1. Situational awareness
2. Concurrent tasks and agents
3. Application resource limits
4. Human-in-the-loop
5. Agents and environments
6. Keyboard efficiency

**A)** Yes — write them as accepted
**B)** Not yet — say what to hold
**C)** Modify — say which to change first

**Recommend:** A — each item’s wording was already accepted.
Which do you want?

**Answer:** Okay, for question 59: yes go ahead

**Interpretation (for processors):** Maps to **A** — explicit permission to write the six already-accepted Requirements items into project `user.md` (accepted wording from to-process / prior anchors). Delete `queue/q-059.md`. Mark those to-process entries as copied to `user.md`.

<!-- answer-anchor: q-059 -->

---

## q-060 — Constraints terminology (engineer → user)

**Question:** Approve this revised Constraints item (terminology only — engineer → user)?

```markdown
Runs as a local desktop application on the user’s machine.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — keep the current Constraints wording

Pick A, B, or C?

**Answer:** All right, for question 60, this is approved, sir.

**Interpretation (for processors):** Maps to **A** — accept revised Constraints wording; update project `user.md` Constraints (engineer → user). Delete `queue/q-060.md`.

<!-- answer-anchor: q-060 -->

---

## q-051 — Approve Slack integration Requirements item (round 3)

**Question:** Modified per your last answer — approve this revised Requirements item (capability list in the requirement; no Success criteria)?

```markdown
### Slack integration

Users can, on demand, read Slack content; tod fetches it to use in the user’s current action:
1. Paste a Slack URL to a channel, thread, or message
2. Paste a Slack channel name
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Okay, question 51: approved

**Interpretation (for processors):** Maps to **A** — accept the revised Slack integration Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-051.md`.

<!-- answer-anchor: q-051-r3 -->

---

## q-056 — Approve Issue tracker integration Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item (generic issue-tracker capabilities; Linear required; open browser included)?

```markdown
### Issue tracker integration

Issue-tracker integrations share a common capability set. Linear is the required issue tracker.

From tod, the user can:
1. Associate a tod task with one or more linked issues
2. Create a tod task from an issue ticket ID (tod pulls the issue content and creates the task)
3. Open a browser to an associated issue
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 56: approved

**Interpretation (for processors):** Maps to **A** — accept the revised Issue tracker integration Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-056.md`.

<!-- answer-anchor: q-056-r2 -->

---

## q-062 — Approve Code editor integration Requirements item (round 2)

**Question:** Modified per your last answer — approve this revised Requirements item (worktree/workspace + branch + file; diffs/changeset in editor stay a later spike)?

```markdown
### Code editor integration

From tod, the user can open the configured code editor to view the code a particular agent is working on:
1. Open that agent’s worktree or workspace when it is distinct
2. Open or switch to a particular branch
3. Open or focus a specific file (and optionally a line)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Alright, question 62: you can simplify this to just open an agent's work tree workspace or branch, and then the other one: open a specific file. Open the way you have. Number three is fine as is. We need to track the thing about a diff/change set. If you're not putting that here, where are you putting it? We still need to do that spike or something.

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-062.md`: merge former (1)+(2) into one capability — open an agent’s worktree/workspace **or** branch; keep open/focus specific file (optional line) as the second capability. Diffs/changeset-in-editor stay **out** of this Requirements item; reinforce open **design spike** in `to-process.md` (from q-055) so it is not lost — do **not** invent a queue file for the spike here. Re-ask A/B/C.

<!-- answer-anchor: q-062 -->

---

## q-063 — Approve Code repository integration Requirements item

**Question:** Approve this proposed Requirements item for code-repository integration (generic capabilities; GitHub required)?

```markdown
### Code repository integration

Code-repository integrations share a common capability set. GitHub is the required code repository.

From tod, the user can:
1. Associate a tod task with one or more linked pull requests
2. Open a browser to an associated pull request
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 63. That's fine.

**Interpretation (for processors):** Maps to **A** — accept the Code repository integration Requirements wording as written. Record accepted text in `to-process.md` / journal for later `user.md` promotion. Do **not** edit `user.md` without explicit permission. Delete `queue/q-063.md`. Update q-052 entry accordingly.

<!-- answer-anchor: q-063 -->

---

## q-067 — Write accepted Requirements to user.md / standing write permission

**Question:** (queue file removed as stale; substance continued as q-069 batch write + q-072 standing permission) OK to add newly accepted Requirements to project user.md / write each as accepted going forward?

**Answer:** For question 67, you don't need to ask this question. You probably just got confused because we updated the process. Yes, you should be adding to user.md as each requirement is accepted.

**Interpretation (for processors):** Waive/drop the redundant write-permission ask (process now: **write on accept**). Standing rule: on Accept of specific wording, write that item into `user.md` in the same turn — no separate permission question. Backfill now: write already-accepted Slack, Issue tracker, and Code repository items into `user.md`. Delete `queue/q-067.md` if present; this answer **obsoletes** `queue/q-069.md` and `queue/q-072.md` — delete those too. Journal the standing write-on-accept policy for this interview.

<!-- answer-anchor: q-067 -->

---

## q-062 — Approve Code editor integration Requirements item (round 3)

**Question:** Modified per your last answer — approve this revised Requirements item (worktree/workspace or branch + file; diffs/changeset stay a design spike in to-process, not this item)?

```markdown
### Code editor integration

From tod, the user can open the configured code editor to view the code a particular agent is working on:
1. Open that agent’s worktree, workspace, or branch
2. Open or focus a specific file (and optionally a line)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Okay, so for question number 62, that's generally correct, except shouldn't it make a note of the spike or reference the spike in some way?

**Interpretation (for processors):** Maps to **B** — do **not** write `user.md` until revised wording accepted. Rewrite `queue/q-062.md`: keep capabilities (1)+(2); add an explicit note in the Requirements item that agent-session changeset/diffs in the editor are deferred to a design spike (not required here). Keep the open spike in `to-process.md`. Leave `queue/q-070.md` alone (separate confirm of parking the spike). Re-ask A/B/C.

<!-- answer-anchor: q-062-r3 -->

---

## q-066 — Approve desktop platforms Constraints item

**Question:** Approve this additional Constraints item (desktop platforms)?

```markdown
- Supports Windows, macOS, and Linux
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 66, yes, that looks correct.

**Interpretation (for processors):** Maps to **A** — accept the desktop-platforms Constraints wording as written. Write into project `user.md` Constraints now (write-on-accept). Mark q-058 to-process entry accordingly. Delete `queue/q-066.md`.

<!-- answer-anchor: q-066 -->

---

## q-070 — Park agent changeset/diffs in code editor as design spike

**Question:** Confirm this design/planning follow-up is enough for agent changeset diffs in the code editor (not a current Requirements item)?

Spike: determine whether tod can show an agent session’s changeset/diffs inside the configured code editor.

**A)** Yes — park as a design spike only
**B)** Promote to a Requirements item now — say the wording you want
**C)** Something else — name it

**Recommend:** A — feasibility is still unknown.
Which do you want?

**Answer:** Question 70. Yes, this is a spike that needs to be done. so park as a spike

**Interpretation (for processors):** Maps to **A** — park as design spike only; do **not** write a Requirements item into `user.md`. Update `to-process.md` spike entry to record explicit human confirm (q-070). Delete `queue/q-070.md`. Leave `queue/q-062.md` unchanged (separate Accept of Code editor wording that references this spike).

<!-- answer-anchor: q-070 -->

---

## q-073 — Pursue persistence and credential management

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet in scope):

1. Fleet state persistence — tasks, agents, and open notifications survive tod application restarts
2. External account linking — user can authorize tod to access Slack, Linear, and GitHub

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** A unless persistence or account-linking must be explicit for this phase.
Which do you want?

**Answer:** For question 73:
1. We definitely need persistence. That's a requirement.
2. We definitely need to manage credentials. We want to do both of those. I want you to take a look at the reference document. We have that `.local/agent/process/projects/tod/refs/features-from-tod-extension.md` document. We did both of these in that other project that we're kind of replacing. You can look there to see what was done there and propose requirements based on what was done there.

**Interpretation (for processors):** Maps to **B** — pursue **both** (1) and (2); rename (2) toward **credential management** per human wording. Do **not** write `user.md` until official wording is Accepted. Read `refs/features-from-tod-extension.md` and park **draft Requirements candidates** (plus design overflow: storage/credential mechanics) in `to-process.md` for the researcher to turn into Accept questions. Delete `queue/q-073.md`. Do not invent queue files.

<!-- answer-anchor: q-073 -->

---

## q-074 — Pursue single-user and local durable data Constraints

**Question:** For an app like this, a few more Constraints categories still look worth considering (not yet locked):

1. Single-user — tod is for one user on their machine (no multi-user shared fleet)
2. Local durable data — tod’s durable state lives on the user’s machine (no required cloud store for core state)

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** A unless either must be an explicit cross-cutting limit for this phase.
Which do you want?

**Answer:** All right, for question 74:
1. Tod is just for one user on their machine. That's correct.
2. Also, yes, durable data. Persistence is just local. That's fine. The only additional requirement on the second one, local durable data, is, however we do end up doing it, it should be in a way that makes it easy for the user to back up, copy somewhere else, stuff like that.

**Interpretation (for processors):** Maps to **B** — pursue **both** as Constraints. Item (2) gains an extra obligation: durable local storage must be easy for the user to **back up / copy elsewhere**. Do **not** write `user.md` until official wording is Accepted. Park draft Constraints candidates in `to-process.md` (align with q-073 persistence Requirement; keep backup/portability as Constraint wording for researcher Accept). Delete `queue/q-074.md`. Do not invent queue files.

<!-- answer-anchor: q-074 -->

---

## q-075 — Invariants coverage for this phase

**Question:** I’ve reviewed the current Invariants for this phase. No must-always-hold rules look necessary beyond what’s already covered by Requirements and Constraints — I don’t see a compelling invariant gap to propose next.

Did we miss anything important?

**A)** No — leave Invariants empty for now
**B)** Yes — name what we missed

**Recommend:** A unless you already know a must-always-hold rule.
Which do you want?

**Answer:** All right, for question 75, I don't even know what invariance examples would be. so nothing

**Interpretation (for processors):** Maps to **A** — leave Invariants empty for now. Do **not** invent invariants or write to `user.md` Invariants. Delete `queue/q-075.md`. Brief journal / to-process note that invariants deferred until human has concrete must-always-hold rules.

<!-- answer-anchor: q-075 -->

---

## q-076 — Outbound Slack / phase-two send message

**Question:** Confirm this later-phase follow-up is enough for outbound Slack from tod (not a current Requirements item)?

Phase two: from tod, send a message to Slack.

**A)** Yes — park as phase two only
**B)** Promote to a Requirements item now — say the wording you want
**C)** Something else — name it

**Recommend:** A — on-demand read is already in scope.
Which do you want?

**Answer:** Okay, question 76: we're not going to define that notifications stuff at all, so let's just delete it. Just don't include any kind of requirement or mention sending a message to Slack at all. When we go to find phase two, we'll do it then, so just delete it.

**Interpretation (for processors):** Maps to **C** / discard — do **not** promote outbound Slack; do **not** park phase-two “send a message to Slack” or related multi-channel Slack-notify overflow. Strip those mentions from `to-process.md`. Leave accepted on-demand **read** Slack Requirements in `user.md` unchanged. Delete `queue/q-076.md`.

<!-- answer-anchor: q-076 -->

---

## q-077 — Cursor IDE as phase-two integration host

**Question:** Confirm this later-phase follow-up is enough for Cursor IDE as an integration host (not a current Requirements item)?

Phase two: Cursor IDE as an additional integration target (alongside the configured code editor).

**A)** Yes — park as phase two only
**B)** Promote to a Requirements item now — say the wording you want
**C)** Something else — name it

**Recommend:** A — code editor integration already covers the configured editor.
Which do you want?

**Answer:** All right, question 77. Yeah, delete this one too. Once we work on phase two, we'll define the requirements. I don't want to define it now.

**Interpretation (for processors):** Maps to **C** / discard — do **not** promote Cursor IDE; do **not** park as phase-two follow-up. Strip/update phase-two Cursor IDE candidate notes in `to-process.md`. Delete `queue/q-077.md`.

<!-- answer-anchor: q-077 -->

---

## q-078 — Approve Fleet state persistence Requirements item

**Question:** Approve this proposed Requirements item for fleet state persistence?

```markdown
### Fleet state persistence

Tasks, agents, and open notifications survive tod application restarts. Durable fleet state is stored on the user’s machine and written when mutated (no separate explicit “save” required for ordinary mutations).

- Success criteria:
  - After quit and relaunch, previously present tasks, agents, and unresolved notifications are still available
  - Ordinary mutations remain after relaunch without an explicit save step
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** For question 78, I mean, the second sentence, "durable fleet state": no, it's just "all durable state." You don't need "not just fleet state," correct? Written when mutated, that's right. Yes, the rest of that looks great.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — change “Durable fleet state” → “All durable state”; keep the rest (including heading, first sentence, success criteria). Write modified wording into project `user.md` Requirements now. Update q-073 to-process draft accordingly. Delete `queue/q-078.md`.

<!-- answer-anchor: q-078 -->

---

## q-079 — Approve Credential management Requirements item

**Question:** Approve this proposed Requirements item for credential management?

```markdown
### Credential management

The user can store, update, and replace credentials tod needs to access configured external services (Slack, Linear, and GitHub). Credentials live on the user’s machine. When credentials are missing or invalid for an action that needs them, tod prompts the user to supply them.

- Success criteria:
  - User can set and change credentials for Slack, Linear, and GitHub used by tod
  - An action that needs a missing/invalid credential prompts for credentials rather than failing silently
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Ok, question 79, approved

**Interpretation (for processors):** Maps to **A** — accept Credential management wording as written. Write into project `user.md` Requirements now. Update q-073 to-process draft. Delete `queue/q-079.md`.

<!-- answer-anchor: q-079 -->

---

## q-080 — Approve single-user Constraints item

**Question:** Approve this proposed Constraints item (single-user)?

```markdown
Tod is for one user on their machine (no multi-user shared fleet).
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** For question 80, yeah, that's okay.

**Interpretation (for processors):** Maps to **A** — accept single-user Constraints wording as written. Write into project `user.md` Constraints now. Update q-074 to-process. Delete `queue/q-080.md`.

<!-- answer-anchor: q-080 -->

---

## q-081 — Approve local durable data Constraints item

**Question:** Approve this proposed Constraints item (local durable data / backup-friendly storage)?

```markdown
Tod’s durable state lives on the user’s machine under a known local location the user can copy or back up with ordinary filesystem tools (no required cloud store for core state).
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** For Question 81, approved.

**Interpretation (for processors):** Maps to **A** — accept local durable data Constraints wording as written. Write into project `user.md` Constraints now. Update q-074 to-process. Delete `queue/q-081.md`.

<!-- answer-anchor: q-081 -->

---

## q-082 — Pursue import / migration from prior Tod

**Question:** For an app like this (replacing the prior Tod extension), one more feature-set category still looks worth considering (not yet in scope):

1. Import / migration — bring tasks and related data forward from the prior Tod extension into this product

Want to pursue this (or a close variant)?

**A)** No — skip for now
**B)** Yes — keep as stated, or rename / adjust scope

**Recommend:** B if continuity from the extension matters for day-one use; else A and leave migration to a later task.
Which do you want?

**Answer:** Question 82. That's a great suggestion, but yes, I think we do want to support import, but it'll just be import, not migration from the previous Tod. We'll import from a JSON dump. The import should be very lenient as far as the input data. It'll do its best to interpret the input data as best it can. It will define the JSON schema for the input that it supports.

**Interpretation (for processors):** Maps to **B** with adjusted scope — pursue **import** only (not prior-Tod migration). Source = JSON dump; product defines supported JSON schema; import is **lenient** (best-effort interpretation). Do **not** write `user.md` until official wording is Accepted. Park draft Requirements candidate (+ design overflow) in `to-process.md` for researcher Accept question. Delete `queue/q-082.md`. Do not invent queue files.

<!-- answer-anchor: q-082 -->

---

## q-062 — Approve Code editor integration Requirements item (round 4)

**Question:** Modified per your last answer — approve this revised Requirements item (capabilities unchanged; adds an explicit note referencing the changeset/diffs design spike)?

```markdown
### Code editor integration

From tod, the user can open the configured code editor to view the code a particular agent is working on:
1. Open that agent’s worktree, workspace, or branch
2. Open or focus a specific file (and optionally a line)

Note: showing an agent session’s changeset/diffs inside the configured code editor is deferred to a design spike (feasibility unknown); not required by this item.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** All right, for question 62, approved.

**Interpretation (for processors):** Maps to **A** — accept Code editor integration wording (including spike note) as written. Write into project `user.md` Requirements now. Update to-process. Delete `queue/q-062.md`.

<!-- answer-anchor: q-062-r4 -->

---

## q-084 — Constraints coverage for this phase

**Question:** I’ve reviewed the current Constraints for this phase. These top-level areas look complete and coherent for a reasonable application of this kind — I don’t see a compelling Constraints gap to propose next:

1. Local desktop application
2. Desktop platforms (Windows, macOS, Linux)
3. Single-user
4. Local durable data (known backup-friendly location; no required cloud store)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you already know another cross-cutting limit.
Which do you want?

**Answer:** Question 84. Approved.

**Interpretation (for processors):** Maps to **A** — Constraints coverage enough for now; no new Constraints. Do not invent Constraints or edit `user.md`. Delete `queue/q-084.md`.

<!-- answer-anchor: q-084 -->

---

## q-083 — Pursue isolated workspaces / multi-repository

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet in scope):

1. Isolated workspaces — tod can give each agent (or task) an isolated git worktree/workspace to work in, and reclaim it when done
2. Multi-repository — a tod task can target or switch among more than one git repository (not only worktrees of a single repo)

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B on 1 if agents routinely need separate checkouts; A on 2 unless multi-repo is day-one.
Which do you want?

**Answer:** Question 83. That first one, number one, we do require that for local agents. We want to support work trees in particular. On a separate thing, we want to support dev containers. That actually makes a whole other environment. We need to add another environment that we support: dev containers. We can support agents running on the host directly inside a dev container and inside the micro VMs or cloud VM, whatever we want to call it. I don't think we need number two.

**Interpretation (for processors):** Maps to **B** on (1) only — require isolated **worktrees** for **local** agents (reclaim when done). Skip (2) multi-repository. Separately: add **Dev Containers** as a third agent environment alongside host/local and Micro-VM (aka cloud VM naming TBD). Do **not** rewrite accepted Agents and environments in `user.md` until revised wording is Accepted. Park draft Requirements candidates (isolated worktrees item + revised environments list) in `to-process.md` for researcher Accept questions. Delete `queue/q-083.md`. Do not invent queue files.

<!-- answer-anchor: q-083 -->

---

## q-086 — Approve Data import Requirements item

**Question:** Approve this proposed Requirements item for data import?

```markdown
### Data import

Tod can import tasks and related data from a JSON dump (not a dedicated migration from the prior Tod extension). Tod defines a JSON schema for the input it supports and interprets input leniently—doing its best to make sense of the data provided.

- Success criteria:
  - User can import from a JSON dump against tod’s documented import schema
  - When the dump is incomplete or imperfect, tod still imports the records and fields it can interpret (partial success allowed)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 86: Do not mention negative things. Remove the thing about not a dedicated migration. Thank you, but the rest of that looks just fine, so you can approve this with removal of that parenthetical phrase.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — remove “(not a dedicated migration from the prior Tod extension)”; keep the rest. Write modified wording into project `user.md` Requirements now. Update q-082 to-process. Delete `queue/q-086.md`.

<!-- answer-anchor: q-086 -->

---

## q-087 — Approve Isolated worktrees Requirements item

**Question:** Approve this proposed Requirements item for isolated worktrees (local agents)?

```markdown
### Isolated worktrees for local agents

For local agents, tod provides each agent an isolated git worktree to work in and can reclaim that worktree when done.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Okay, for question 87, I think the only addition here is that it's both for local agents and for dev container agents.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — extend scope from local agents only to **local agents and Dev container agents**; keep provide/reclaim worktree wording. Write modified item into project `user.md` Requirements now. Update to-process (worktrees draft consumed; design overflow: worktrees apply to local + Dev container; Micro-VM still open). Delete `queue/q-087.md`.

<!-- answer-anchor: q-087 -->

---

## q-088 — Approve revised Agents and environments Requirements item

**Question:** Approve this revised Agents and environments Requirements item (adds Dev container; Local clarified as host; operations unchanged)?

```markdown
### Agents and environments

tod can manage agents in the following environments:
1. Local (host)
2. Dev container
3. Micro-VM

Agents are generally tied to a user task. Initial scope does not require attaching to sessions tod did not start.

The following operations are supported (what the user can do):
- Launch an agent
- Review an agent’s status
- Submit a prompt to an agent
- View an agent’s transcript
- Interrupt an agent (stop the process and/or submit a new prompt that interrupts its current activity)
- Switch a running agent between autonomous and interactive mode
- Stop an agent
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** For question 88, a minor correction: "Dev container" -> "devcontainer"

delete this whole line: '  Agents are generally tied to a user task. Initial scope does not require attaching to sessions tod did not start.'

'  - Interrupt an agent (stop the process and/or submit a new prompt that interrupts its current activity)' -> It's stopping the process. Just leave it as "interrupt an agent." Leave it at that.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — (1) rename “Dev container” → **devcontainer**; (2) remove task-tie / no-attach-foreign-sessions sentence; (3) Interrupt operation is bare “Interrupt an agent” (no parenthetical). Write revised Agents and environments into project `user.md` item 5 now. Align item 14 term to devcontainer. Update to-process (environments draft consumed). Delete `queue/q-088.md`.

<!-- answer-anchor: q-088 -->
---

## q-089 — Pursue Search / Export / Lifecycle / Background alerts

**Question:** For an app like this (large task/agent fleets), a few more feature-set categories still look worth considering (not yet in scope):

1. Search — find tasks and agents by text or id across the fleet
2. Data export — export tasks and related data to a JSON dump (complements import)
3. Task lifecycle — archive, complete, or waiting states so large task sets stay manageable
4. Background alerts — OS/system notifications when agents need help while tod is not focused

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B on 1 and 3 given ~500 tasks / ~100 agents; A on 2 and 4 unless day-one continuity or background HITL matters.
Which do you want?

**Answer:** For question 89:
- Search: yes, we definitely need search.
- Data export: not yet. We'll defer that, so skip that.
- Task lifecycle: yes, background alerts: we are going to want background alerts, but that's being deferred to a future phase, so not right now.

**Interpretation (for processors):** Maps to **B** on (1) Search and (3) Task lifecycle — pursue for this phase (draft Requirements candidates only; do **not** write `user.md` until Accept). Skip (2) Data export for now. (4) Background alerts — wanted eventually but **deferred to a future phase** (not current Requirements). Park Search + Task lifecycle drafts and deferral notes in `to-process.md`. Delete `queue/q-089.md`. Do not invent queue files.

<!-- answer-anchor: q-089 -->

---

## q-092 — Approve revised Concurrent SC (Devcontainer concurrency)

**Question:** Approve this revised Concurrent tasks and agents success criterion (adds Devcontainer concurrency; clarifies local as host)? The requirement statement and other success criteria stay the same.

Proposed concurrent-run criterion only:

```markdown
- At least ~10 local (host) agents, ~10 Devcontainer agents, and ~10 micro-VM agents can run concurrently
```

**A)** Accept as written
**B)** Modify — say what to change (counts or wording)
**C)** Reject — keep the current local + micro-VM-only criterion

Pick A, B, or C?

**Answer:** For question 92, wasn't this already included somewhere? Oh, you're updating it for the dev container. We don't need to change that. The concurrency scaling thing that we had earlier was already acceptable. That one doesn't need to be updated. For the scaling requirement that we already have, it's okay as it is: 10 local agents, 10 microfibre engines running concurrently, that's fine.

**Interpretation (for processors):** Maps to **C** — Reject. Do **not** change Concurrent tasks and agents success criteria in `user.md`. Keep existing concurrent-run criterion (~10 local agents and ~10 micro-VM agents). “microfibre engines” = micro-VM agents. No Devcontainer concurrent bound for this phase. Delete `queue/q-092.md`.

<!-- answer-anchor: q-092 -->

---

## q-093 — Micro-VM agents and isolated worktrees

**Question:** Isolated worktrees are required for local and Devcontainer agents. Should Micro-VM agents also get isolated git worktrees in this phase?

**A)** Yes — require isolated worktrees for Micro-VM agents too
**B)** No — leave Micro-VM agents out of the worktrees requirement for now
**C)** Defer — decide in design; do not change Requirements now

**Recommend:** B unless Micro-VM checkouts need the same isolation day-one.
Which do you want?

**Answer:** For question 93, one side comment: the word devcontainer, use lowercase d, not uppercase D. Typically, Micro-VM agents are working on a single. They're already a different file system, so they don't need a different work tree. That's why we didn't specify that.

**Interpretation (for processors):** Maps to **B** — do **not** extend Isolated worktrees to Micro-VM agents; leave item 14 as local + devcontainer only. Rationale (park): Micro-VM agents already have a separate filesystem, so they do not need a separate worktree. Terminology standing rule: spell **devcontainer** with lowercase d (user.md already uses that form). No `user.md` Requirements change. Delete `queue/q-093.md`.

<!-- answer-anchor: q-093 -->

---

## q-094 — Approve Search Requirements item

**Question:** Approve this proposed Requirements item for search?

```markdown
### Search

tod can find tasks and agents by text or id across the fleet.

- Success criteria:
  - User can locate a task by its id
  - User can locate an agent by its id
  - User can find tasks and agents by a text query across the fleet
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** For question 94, I don't think we need to go into a lot of detail for this requirement. I think the requirement is just going to be that the user has a highly efficient way of finding tasks and agents. In particular, we want to specify that the search can be fuzzy. It's basically that there has to be a fuzzy search. That's what it is. It's kind of what she wrote. I think we don't need success criteria, just requirements that there's a mechanism to do smart fuzzy search for tasks, agents, and notifications. In fact, a better way to do that is basically in all major lists, right? Anywhere there is a primary list, a table, or not just a list, anywhere there's a list, then we want to support smart fuzzy search on any lists in the UI.

And then we can call out, as examples, tasks, agents, and notifications.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications. Write Requirements item (no success criteria): primary lists in the UI support smart fuzzy search; examples include tasks, agents, and notifications. Drop id/text fleet-wide SC list from the proposal. Mark Search draft in to-process consumed. Delete `queue/q-094.md`.

<!-- answer-anchor: q-094 -->

---

## q-095 — Task lifecycle states

**Question:** For task lifecycle in this phase, which states should tod support?

1. Waiting — set aside as waiting (still in play, separated from active)
2. Archive — archived out of the active set (restorable)
3. Complete — marked done/completed as its own state (distinct from archive)

**A)** 1 and 2 only (waiting + archive)
**B)** All three (waiting + archive + complete)
**C)** Different set — name which numbers (or rename)

**Recommend:** A unless you need a completed state separate from archive.
Which do you want?

**Answer:** Okay, for question 95: The process skill already defines a set of states. We're going to support that set of states for the tasks.

**Interpretation (for processors):** Maps to **C** — different set. Do **not** use waiting / archive / complete as the Requirements model. Direction: tod task lifecycle states match the process skill task lifecycle (`proposed` → `design` → `planning` → `ready` → `active` → `verifying` → `review` → `approved` → `merged` → `released` → `learn` → `done`; paused/blocked orthogonal). Draft/Accept only — do **not** write `user.md` until Accept of specific wording. Update to-process Task lifecycle draft. Delete `queue/q-095.md`.

<!-- answer-anchor: q-095 -->

---

## q-096 — Agent retire / remove vs Stop + idle

**Question:** For agents in this phase, is Stop plus idle tracking enough, or do you also need a retire/remove-from-fleet action (beyond interrupting or stopping a run)?

**A)** Stop + idle is enough — no separate agent retire/remove requirement
**B)** Yes — need retire/remove (or similar) so agents leave the managed fleet

**Recommend:** A unless long-lived agent rows become clutter day-one.
Which do you want?

**Answer:** Okay, question 96: all right, agent management is a little more complicated. They can be processing which means that they are actively working on something. They can be blocked for a human response. I guess when they're first launched they would be starting. They would start in the starting state, and then it would submit the prompt to it. then It would be in the processing state. If it got to a point where it is blocked and it needs a response, then it would switch to the the blocked state. And then sometimes it'll just finish the task for which it was prompted. Let's call that state "Waiting" for now, which is kind of a very fine line between this: "Blocked" means the agent was not able to finish the prompt, and it has to have human input in order to finish its prompt. "Waiting" means that it finished the prompt it was given successfully, and now it's waiting to receive further instructions or be shut down. And then I don't know if we even need a state after that, because if then the user dismisses it and says, "OK, it's done," it shuts down and it's gone. It doesn't really have a state after that.

**Interpretation (for processors):** Supersedes A/B framing with an agent **runtime status** model (direction only — do **not** write `user.md` until Accept of specific wording):

1. **Starting** — launched; prompt not yet running / about to submit
2. **Processing** — actively working on the current prompt
3. **Blocked** — could not finish the prompt; needs human input to continue
4. **Waiting** — finished the prompt successfully; awaiting further instructions or shutdown

Dismiss / “done” → shut down and **gone** (no post-shutdown / retired durable state). Imply **no separate retire/remove Requirements item** for this phase (aligned with A on the original ask). Park agent-status draft in to-process for a later Accept. Delete `queue/q-096.md`.

<!-- answer-anchor: q-096 -->

---

## q-097 — Manual tasks / notes / git association

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet recorded):

1. Manual create and edit of tasks — create/edit title and core fields in tod (not only via import or Linear)
2. Freeform notes on a task
3. Associate a primary git repository (and optional branch) with a task — beyond linked pull requests

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B on 1; A on 2–3 unless notes or repo/branch binding are day-one.
Which do you want?

**Answer:** All right, for question 97:
1. Yes, we have to be able to manually create and edit tasks.
2. Yes, there's a free-form text area for notes on a task.
3. Yes, there's a Git repo, and yes, there's a branch. Both the Git repo and the branch are optional. Most things are optional. The title and a slug are not optional. Everything else is optional.

**Interpretation (for processors):** Maps to **B** on 1, 2, and 3. Park drafts only — do **not** write `user.md` until Accept of specific wording. Field rule: **title** and **slug** required; all other task fields optional (including git repo and branch). Delete `queue/q-097.md`.

<!-- answer-anchor: q-097 -->

---

## q-098 — Task–agent association establishment

**Question:** Tasks and agents already have a many-to-many association. In this phase, how must the user establish that link?

**A)** Implicitly is enough — launching an agent in the context of a task covers association
**B)** Also require explicit associate / disassociate between existing tasks and agents

**Recommend:** A unless re-linking agents after launch is day-one.
Which do you want?

**Answer:** Question 98: I want to go back and change something. Previously, I have said that it's a many-to-many association between tasks and agents. I want to simplify that. I think that would really complicate things. We want a one-to-many from tasks to agents. Most of the time, one task, one agent. However, there will be times when one task has multiple agents. I do want to say, and this is a big change, that we will no longer have one agent associated with multiple tasks. That's gone. Now it's just one-to-many: tasks to agents.

That said, the association is implicit. That's all. When you launch an agent, it is for a specific task, and therefore the association is implicit there.

**Interpretation (for processors):** Maps to **A** on establishment, plus a **Modify** of existing Requirements item 2: replace many-to-many with one-to-many (task → agents); an agent belongs to at most one task; association is implicit on launch for a specific task. Write revised item 2 into `user.md` now. Delete `queue/q-098.md`.

<!-- answer-anchor: q-098 -->

---

## q-099 — Approve Task lifecycle Requirements item

**Question:** Approve this proposed Requirements item for task lifecycle?

```markdown
### Task lifecycle

Each tod task has a lifecycle state from this ordered set:
proposed → design → planning → ready → active → verifying → review →
approved → merged → released → learn → done.

Paused/blocked is an orthogonal status, not a separate lifecycle state.

- Success criteria:
  - User can see a task’s current lifecycle state
  - User can change a task’s lifecycle state within that set
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 99: approved.

**Interpretation (for processors):** Maps to **A** — Accept as written. Write Task lifecycle into project `user.md` now (item 16). Mark Task lifecycle draft in to-process consumed. Delete `queue/q-099.md`.

<!-- answer-anchor: q-099 -->

---

## q-100 — Approve Agent runtime status Requirements item

**Question:** Approve this proposed Requirements item for agent runtime status?

```markdown
### Agent runtime status

Each managed agent has a runtime status from this set:
- Starting — launched; prompt submission pending or in progress
- Processing — actively working on the current prompt
- Blocked — could not finish the prompt; needs human input to continue
- Waiting — finished the prompt successfully; awaiting further instructions or shutdown

When the user dismisses an agent as done, tod shuts it down and removes it from the managed fleet (no durable post-shutdown / retired state).
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 100.
The first bullet point, just starting as launched, has not yet received a prompt.
The second bullet point, processing, just means it's working on a prompt.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications. Write Requirements item 17 now with:
- Starting — launched; has not yet received a prompt
- Processing — working on a prompt
Keep Blocked, Waiting, and dismiss/remove wording as proposed. Mark q-096 agent-status draft consumed. Delete `queue/q-100.md`.

<!-- answer-anchor: q-100 -->

---

## q-101 — Approve Manual task create and edit Requirements item

**Question:** Approve this proposed Requirements item for manual task create and edit?

```markdown
### Manual task create and edit

Users can create and edit tasks in tod (not only via import or Linear). Title and slug are required; all other task fields are optional.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 101: You added a negative parenthesis thing. Again, don't do that. No negative things. "Users can create and edit tasks manually," I guess, is what it's saying manually in tod.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications. Remove the negative parenthetical “(not only via import or Linear)”. Write positive wording: Users can create and edit tasks manually in tod. Keep title and slug required; all other task fields optional. Standing style: no negative parentheticals / “not only via…” contrasts in Requirements wording. Write `user.md` item 18 now. Consume Manual create draft from q-097. Delete `queue/q-101.md`.

<!-- answer-anchor: q-101 -->

---

## q-102 — Delete / settings / bulk actions

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet recorded):

1. Delete tasks — permanently remove a task (distinct from lifecycle `done`)
2. Application settings — configure non-credential preferences in tod
3. Bulk actions — act on multiple selected tasks or agents at once

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** A unless delete, settings, or bulk ops are day-one.
Which do you want?

**Answer:** Question 102:
1. Yes, we need to be able to delete tasks.
2. Yes, we need settings.
3. We're not going to do bulk actions for now.

**Interpretation (for processors):** Maps to **B** on 1 and 2; skip 3. Park drafts only — do **not** write `user.md` until Accept of specific wording. Delete `queue/q-102.md`.

<!-- answer-anchor: q-102 -->

---

## q-103 — Approve Freeform notes on a task Requirements item

**Question:** Approve this proposed Requirements item for freeform task notes?

```markdown
### Freeform notes on a task

Users can add and edit freeform notes on a task.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Alright, question 103. Although that's true, it doesn't need to be a requirement all by itself. That should just be part of the create/edit task requirement.

**Interpretation (for processors):** Maps to **C** on a standalone item — do **not** add a separate Freeform notes Requirements item. Fold freeform notes into existing Manual task create and edit (`user.md` item 18). Update item 18 accordingly. Mark Freeform notes draft consumed. Delete `queue/q-103.md`.

<!-- answer-anchor: q-103 -->

---

## q-104 — Approve Task git repository and branch Requirements item

**Question:** Approve this proposed Requirements item for task git repository and branch?

```markdown
### Task git repository and branch

Users can optionally associate a git repository and optionally a branch with a task.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 104: Same thing, there should be just one create edit task requirement that includes all the different fields and such.

**Interpretation (for processors):** Maps to **C** on a standalone item — do **not** add a separate Task git repository and branch Requirements item. Fold optional git repository and branch into the single Manual task create and edit requirement (`user.md` item 18), alongside freeform notes and other fields. Update item 18 accordingly. Mark git draft consumed; q-097 field drafts closed. Delete `queue/q-104.md`.

<!-- answer-anchor: q-104 -->

---

## q-105 — Approve Delete tasks Requirements item

**Question:** Approve this proposed Requirements item for deleting tasks?

```markdown
### Delete tasks

Users can permanently delete a task.

- Success criteria:
  - User can permanently remove a task from tod
  - After delete, the task is no longer in the managed task set
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 105. Let's roll this into the create/edit/delete requirement.

**Interpretation (for processors):** Maps to **C** on a standalone Delete tasks item — fold permanent delete into the Manual task create/edit requirement (`user.md` item 18), renaming it create/edit/delete. Do not add a separate Delete Requirements item or its proposed success criteria. Mark Delete draft (q-102) consumed. Delete `queue/q-105.md`.

<!-- answer-anchor: q-105 -->

---

## q-106 — Approve Application settings Requirements item

**Question:** Approve this proposed Requirements item for application settings?

```markdown
### Application settings

Users can view and change application preferences in tod. Preferences are stored on the user’s machine and are separate from credentials.

- Success criteria:
  - User can open settings and change a preference
  - Preference changes remain after quit and relaunch
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** All right, question 106. Approved.

**Interpretation (for processors):** Maps to **A** — Accept as written. Write Application settings into `user.md` as item 19 now. Mark Application settings draft (q-102) consumed. Delete `queue/q-106.md`.

<!-- answer-anchor: q-106 -->

---

## q-107 — Destructive confirms / diagnostic logging

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet recorded):

1. Destructive-action confirms — require confirmation before delete task or stop agent
2. Diagnostic logging — view tod’s own logs for troubleshooting

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** A unless confirms or logs are day-one.
Which do you want?

**Answer:** question 107. let's do both of these.

**Interpretation (for processors):** Maps to **B** on 1 and 2. Park drafts only — do **not** write `user.md` until Accept of specific wording. Delete `queue/q-107.md`.

<!-- answer-anchor: q-107 -->

---

## q-108 — Paused/blocked on tasks

**Question:** Task lifecycle already notes paused/blocked as orthogonal (not a lifecycle state). In this phase, must the user be able to set and clear paused/blocked on a task?

**A)** Yes — user can set and clear paused/blocked
**B)** No — leave paused/blocked out of Requirements for now (design later)

**Recommend:** A so blocked work stays visible in a large task set.
Which do you want?

**Answer:** Alright, question 108. I think paused and blocked should just be tags, so we don't necessarily need to call them out explicitly, other than we could potentially include them as example tags. However, I think it'd be reasonable for us to add to the section where we talked about sorting and filtering. We can mention that we want to be able to sort and filter by sort, filter, and group by tags, right? Not only by sort and filter, but also by tags.

**Interpretation (for processors):** Maps to **B** modified — do not add explicit set/clear paused/blocked as orthogonal status. Paused and blocked are **tags** (may appear as example tags only). Update Situational awareness success criterion (item 1): sort, filter, and group **including by tags**. Remove explicit paused/blocked orthogonal line from Task lifecycle (item 16). Park tags model detail in to-process for later Accept if needed. Delete `queue/q-108.md`.

<!-- answer-anchor: q-108 -->

---

## q-109 — Explicit tags Requirements item

**Question:** Situational awareness already requires that tasks and agents can be sorted, filtered, and grouped. Earlier you also wanted grouping by tag. For this phase, should user-defined task tags be an explicit Requirements item?

**A)** Yes — add a tags Requirements item (user can assign tags used for grouping/filtering)
**B)** No — leave tag/group keys to design under the existing sort/filter/group success criterion

**Recommend:** B unless tags are a hard day-one data model.
Which do you want?

**Answer:** all right, question 109. Yes, we need tags requirements.

**Interpretation (for processors):** Maps to **A** — pursue explicit tags Requirements item. Park draft only — do **not** write `user.md` until Accept of specific wording. Example tags may include paused and blocked (q-108). Delete `queue/q-109.md`.

<!-- answer-anchor: q-109 -->

---

## q-110 — Keyboard shortcuts / Micro-VM visibility / launch presets

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet recorded):

1. Customizable keyboard shortcuts — remap bindings beyond the built-in keyboard-only reachability
2. Micro-VM usage visibility — show usage or cost signals for micro-VM agents (metering was parked earlier)
3. Agent launch presets — saved defaults for how new agents are launched

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** A unless one is clearly day-one.
Which do you want?

**Answer:** question 110:
1. In the preferences, customizable keyboard shortcuts. In preferences, there'll be a page or view or something in order to do that.
2. Let's not worry about that for this phase.
3. no

**Interpretation (for processors):** Maps to **B** on 1 only; skip 2 (Micro-VM usage visibility — not this phase) and 3 (agent launch presets). Park Customizable keyboard shortcuts draft only — preferences includes a page/view for remapping shortcuts; do **not** write `user.md` until Accept of specific wording. Delete `queue/q-110.md`.

<!-- answer-anchor: q-110 -->

---

## q-111 — Switch-to-task vs existing agent requirements

**Question:** Prior art had a primary “switch to this task” flow (lease worktree, open editor). Given Launch agent, Open editor, and isolated worktrees are already in Requirements, is a separate switch-to-task Requirements item needed this phase?

**A)** No — existing launch / open-editor / worktree requirements cover it
**B)** Yes — add an explicit switch-to-task (or equivalent) Requirements item

**Recommend:** A unless switch-to-task is a distinct day-one action.
Which do you want?

**Answer:** Question number 111: A, The switch to this task is being replaced by the whole agent thing.

**Interpretation (for processors):** Maps to **A** — do **not** add a separate switch-to-task Requirements item. Prior-art switch-to-task flow is superseded by the agent-centric model (launch agent, open editor, worktrees). No `user.md` change. Delete `queue/q-111.md`.

<!-- answer-anchor: q-111 -->

---

## q-112 — Dirty worktree on reclaim

**Question:** Isolated worktrees already require tod to reclaim a worktree when done. When reclaiming, must tod refuse or warn if the worktree has uncommitted changes?

**A)** Yes — block or warn on dirty worktree before reclaim
**B)** No — reclaim as stated is enough for this phase
**C)** Defer — decide in design; do not add a Requirements item now

**Recommend:** C unless dirty-check is a hard day-one safety gate.
Which do you want?

**Answer:** question 112: Yes, we'll want a warning with options. This will be very similar to how it's defined in the `.local/agent/process/projects/tod/refs/features-from-tod-extension.md` document.

**Interpretation (for processors):** Maps to **A** modified — warn on dirty worktree before reclaim, with user-facing options (prior art DirtyWarning: Check again, Stash local changes, Cancel). Park draft only — do **not** write `user.md` until Accept of specific wording. Reference prior art dirty rules in to-process for design. Delete `queue/q-112.md`.

<!-- answer-anchor: q-112 -->

---

## q-113 — Approve Tags Requirements item

**Question:** Approve this proposed Requirements item for tags?

```markdown
### Tags

Users can assign tags to tasks and agents.

- Success criteria:
  - User can add, remove, and view tags on a task
  - User can add, remove, and view tags on an agent
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** alright, question 113, only tags on tasks for now. I don't think we'll need it on agents.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications. Write Tags into `user.md` item 20 — tasks only (no agent tags). Update Situational awareness SC (item 1): sort/filter/group by tags applies to tasks only. Delete `queue/q-113.md`.

<!-- answer-anchor: q-113 -->

---

## q-114 — Approve Destructive-action confirmation Requirements item

**Question:** Approve this proposed Requirements item for destructive-action confirmation?

```markdown
### Destructive-action confirmation

tod requires user confirmation before destructive actions.

- Success criteria:
  - User must confirm before permanently deleting a task
  - User must confirm before stopping an agent
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** 114. Yeah, that's fine.

**Interpretation (for processors):** Maps to **A** — Accept as written. Write Destructive-action confirmation into `user.md` item 21 now. Mark q-107 destructive confirms draft consumed. Delete `queue/q-114.md`.

<!-- answer-anchor: q-114 -->

---

## q-115 — Approve Diagnostic logging Requirements item

**Question:** Approve this proposed Requirements item for diagnostic logging?

```markdown
### Diagnostic logging

Users can view tod’s own diagnostic logs for troubleshooting.

- Success criteria:
  - User can open or view tod’s diagnostic logs from within the application
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** question 115: We don't need the Successful Criteria. The single-line statement is sufficient.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — requirement statement only; no success criteria. Write Diagnostic logging into `user.md` item 22 now. Mark q-107 diagnostic logging draft consumed. Delete `queue/q-115.md`.

<!-- answer-anchor: q-115 -->

---

## q-116 — Approve Customizable keyboard shortcuts Requirements item

**Question:** Approve this proposed Requirements item for customizable keyboard shortcuts?

```markdown
### Customizable keyboard shortcuts

Users can customize keyboard shortcuts in tod’s application preferences.

- Success criteria:
  - User can open a preferences view for keyboard shortcuts
  - User can remap at least one keyboard shortcut and the new binding takes effect
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** question 116: Just a single-line statement is sufficient. We don't need the success criteria.

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — requirement statement only; no success criteria. Write Customizable keyboard shortcuts into `user.md` item 23 now. Mark q-110 keyboard shortcuts draft consumed. Delete `queue/q-116.md`.

<!-- answer-anchor: q-116 -->

---

## q-117 — Delete task with associated agents

**Question:** When a task that still has associated agents is permanently deleted, what should tod do?

**A)** Block delete until all associated agents are dismissed or stopped
**B)** Delete the task and automatically shut down and remove associated agents
**C)** Delete the task only; leave associated agents in the fleet (orphaned or unchanged)

**Recommend:** B unless you want an explicit cleanup step.
Which do you want?

**Answer:** question 117: A

**Interpretation (for processors):** Maps to **A** — block permanent delete while associated agents remain. Extend Manual task create, edit, and delete (`user.md` item 18) with that rule now. Delete `queue/q-117.md`.

<!-- answer-anchor: q-117 -->

---

## q-118 — Terminal session supervision vs Launch agent

**Question:** Local terminal or CLI agent sessions were discussed earlier. Is Launch agent under Agents and environments enough for this phase, or do we need an explicit terminal-session supervision Requirements item?

**A)** Launch agent is enough — no separate terminal-session item
**B)** Yes — add an explicit terminal/CLI session supervision Requirements item

**Recommend:** A unless starting and supervising terminal sessions is a distinct day-one capability beyond Launch agent.
Which do you want?

**Answer:** question 118. I'm not quite sure what you're asking with this one. So let me just say this instead: We do want a separate function to be able to launch a shell. We want to be able to launch a shell into an agent's environment so that we can run commands, view the file system, etc.

**Interpretation (for processors):** Supersedes A/B framing — pursue a **separate** launch-shell capability (not covered by Launch agent alone). User can launch a shell into an agent’s environment to run commands and view the filesystem. Park draft only — do **not** write `user.md` until Accept of specific wording. Delete `queue/q-118.md`.

<!-- answer-anchor: q-118 -->

---

## q-119 — Approve Dirty worktree warning on reclaim Requirements item

**Question:** Approve this proposed Requirements item for dirty worktree warning on reclaim?

```markdown
### Dirty worktree warning on reclaim

When reclaiming an isolated git worktree, tod warns if the worktree has uncommitted changes and offers options before proceeding.

- Success criteria:
  - If the worktree is not clean, tod shows a warning before reclaim
  - The user can cancel reclaim or choose among options to proceed (such as re-check, stash local changes, or cancel)
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** question 119: The single sentence statement is sufficient. Remove the success criteria

**Interpretation (for processors):** Maps to **B** / accept-with-modifications — requirement statement only; no success criteria. Write Dirty worktree warning on reclaim into `user.md` item 24 now. Mark q-112 reclaim-warning draft consumed (design overflow for option details remains in to-process). Delete `queue/q-119.md`.

<!-- answer-anchor: q-119 -->

---

## q-121 — Task slug uniqueness

**Question:** Must task slugs be unique across all tasks in tod?

**A)** Yes — slugs must be unique
**B)** No — duplicate slugs are allowed

**Recommend:** A — slugs are used as stable short identifiers.
Which do you want?

**Answer:** question 121: yes, test slugs must be unique.

**Interpretation (for processors):** Maps to **A** — “test” = task. Extend Manual task create, edit, and delete (`user.md` item 18): slugs must be unique across all tasks. Delete `queue/q-121.md`.

<!-- answer-anchor: q-121 -->

---

## q-122 — Task title uniqueness

**Question:** Must task titles be unique across all tasks in tod?

**A)** Yes — titles must be unique (case-insensitive)
**B)** No — duplicate titles are allowed

**Recommend:** A — matches prior-art task identity rules.
Which do you want?

**Answer:** question 122: test titles must be unique

**Interpretation (for processors):** Maps to **A** — “test” = task. Extend Manual task create, edit, and delete (`user.md` item 18): titles must be unique across all tasks (case-insensitive). Delete `queue/q-122.md`.

<!-- answer-anchor: q-122 -->

---

## q-123 — Agent errors / pin reorder / duplicate task

**Question:** For an app like this, a few more feature-set categories still look worth considering (not yet recorded):

1. Agent operation errors — clear error and recovery UX when launch, stop, or interrupt fails
2. Task list ordering — pin or manually reorder tasks in the primary list
3. Duplicate task — create a new task from an existing one

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** A unless one is clearly day-one.
Which do you want?

**Answer:** question 123: No, we don't need any of those, but that does remind me of something we do need: some kind of a status area. The status area is where it'll show some kind of text, possibly a spitter. It's just to display to the user current operations as a reaction to user requests. User requests something to happen, and the status area shows them that it's happening.

**Interpretation (for processors):** Maps to **A** on 1–3 (skip agent operation errors, pin/reorder, duplicate task). **Plus** new direction: pursue a **status area** Requirements item — UI area showing text (possibly a spinner) for in-progress operations in response to user actions. Park draft only — do **not** write `user.md` until Accept of specific wording. Do **not** create queue files. Delete `queue/q-123.md`.

<!-- answer-anchor: q-123 -->

---

## q-124 — Notification resolution persistence

**Question:** When the user resolves a notification from the managed queue, must tod record that resolution persistently (so it stays resolved after relaunch)?

**A)** Yes — resolved notifications stay resolved after relaunch
**B)** No — resolution persistence is not required for this phase

**Recommend:** A — aligns with Fleet state persistence and “persist until acted upon.”
Which do you want?

**Answer:** For question number 124: B, no

**Interpretation (for processors):** Maps to **B** — do **not** require durable resolution persistence for notifications across relaunch this phase. No new Requirements item; no `user.md` change unless existing items conflict (note in to-process/journal only). Delete `queue/q-124.md`.

<!-- answer-anchor: q-124 -->

---

## q-125 — Launch into existing devcontainer

**Question:** For devcontainer agents, must tod support launching agents inside an existing devcontainer the user already has open?

**A)** Yes — must support launching into an existing devcontainer
**B)** No — tod may always create or own the devcontainer context for agent launch

**Recommend:** B unless attaching to an already-open devcontainer is day-one.
Which do you want?

**Answer:** question number 125. Yes.

**Interpretation (for processors):** Maps to **A** — extend Agents and environments (`user.md` item 5): tod supports launching devcontainer agents into an existing devcontainer the user already has open. Delete `queue/q-125.md`.

<!-- answer-anchor: q-125 -->

---

## q-126 — Approve Launch shell into agent environment Requirements item

**Question:** Approve this proposed Requirements item for launching a shell into an agent’s environment?

```markdown
### Launch shell into agent environment

Users can launch a shell into an agent’s environment to run commands and view the filesystem.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question number 126: approved

**Interpretation (for processors):** Maps to **A** — Accept as written. Write Launch shell into agent environment into `user.md` item 25 now. Mark q-118 launch-shell draft consumed. Delete `queue/q-126.md`.

<!-- answer-anchor: q-126 -->

---

## q-127 — Auto-generate task slug

**Question:** When creating a task manually, should tod auto-generate the slug from the title if the user does not supply one?

**A)** Yes — auto-generate slug from title
**B)** No — user must always provide the slug explicitly

**Recommend:** A — matches prior-art task creation behavior.
Which do you want?

**Answer:** question number 127: yes, auto-generate slug from title and ticket ID if available. Look at the document features from tod extension, and use the same as outlined in there

**Interpretation (for processors):** Maps to **A** modified — auto-generate slug when not supplied, from title and linked issue ticket id when available; follow prior Tod extension rules (`refs/features-from-tod-extension.md`: slug defaults from title, editable, disambiguate on collision). Extend Manual task create, edit, and delete (`user.md` item 18). Park algorithm detail in to-process for design. Delete `queue/q-127.md`.

<!-- answer-anchor: q-127 -->

---

## q-128 — Configure code editor in settings

**Question:** Code editor integration already lets the user open the configured editor from tod. Must the user be able to configure which code editor tod uses?

**A)** Yes — user can configure the code editor in settings
**B)** No — leave editor choice to design or fixed default for now

**Recommend:** A — without it, code editor integration has no user-set target.
Which do you want?

**Answer:** All right, question 128. No, we're only going to support one for now.

**Interpretation (for processors):** Maps to **B** — one supported code editor this phase; no user configuration of editor choice in settings. Update Code editor integration (`user.md` item 12) accordingly. Delete `queue/q-128.md`.

<!-- answer-anchor: q-128 -->

---

## q-129 — Agent environment type visibility

**Question:** Must each agent show its environment type (local, devcontainer, or Micro-VM) in the UI?

**A)** Yes — environment type is visible on each agent
**B)** No — leave environment visibility to design for now

**Recommend:** A — you wanted local vs cloud-VM locus visible as an agent attribute.
Which do you want?

**Answer:** question 129. Yes.

**Interpretation (for processors):** Maps to **A** — add Situational awareness success criterion (item 1): each agent’s environment type (local, devcontainer, or Micro-VM) is visible in the UI. Delete `queue/q-129.md`.

<!-- answer-anchor: q-129 -->

---

## q-130 — Approve Status area Requirements item

**Question:** Approve this proposed Requirements item for a status area?

```markdown
### Status area

tod provides a status area that displays text about in-progress operations in response to user requests (such as a spinner while work is underway).
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 130: Approved.

**Interpretation (for processors):** Maps to **A** — Accept as written. Write Status area into `user.md` item 26 now. Mark q-123 status area draft consumed. Delete `queue/q-130.md`.

<!-- answer-anchor: q-130 -->

---

## q-131 — Editable slug after creation

**Question:** After a task is created, can the user edit its slug?

**A)** Yes — slug is editable after creation
**B)** No — slug is fixed after creation

**Recommend:** A — matches prior-art editable slug behavior.
Which do you want?

**Answer:** question 131. Yes. Also, if the ticket ID is added or modified later, then the slug will be automatically updated for that unless it has already been manually changed.

**Interpretation (for processors):** Maps to **A** plus modification — extend Manual task create, edit, and delete (`user.md` item 18): slugs editable after creation; when linked issue ticket id is added or changed, auto-update slug from title and ticket id unless user manually changed the slug. Delete `queue/q-131.md`.

<!-- answer-anchor: q-131 -->

---

## q-132 — Status area for operation failures

**Question:** Must the status area also display messages when a user-requested operation fails?

**A)** Yes — failures are shown in the status area
**B)** No — status area is for in-progress feedback only

**Recommend:** A — users need failure feedback in the same place as progress.
Which do you want?

**Answer:** question 132. I think we need something more visible for errors. If we're talking about errors, we need something more visible than just the status area.

**Interpretation (for processors):** Maps to **B** on status-area-for-failures — status area stays in-progress only (item 26 unchanged). **Plus** pursue separate, more prominent error visibility for failed user-requested operations. Park draft only — do **not** write `user.md` until Accept of specific wording. Delete `queue/q-132.md`.

<!-- answer-anchor: q-132 -->

---

## q-133 — Dismiss-as-done confirmation

**Question:** When a user dismisses an agent as done, must tod require confirmation beyond the general destructive-action confirmation for stopping an agent?

**A)** Yes — dismiss-as-done needs its own confirmation
**B)** No — stop-agent confirmation is enough (or no extra confirm for dismiss)

**Recommend:** B unless dismiss-as-done is distinct enough to warrant a separate confirm.
Which do you want?

**Answer:** Question 133. If an agent is already in whatever the final state is (I can't remember what it is), it's finished its prompt, and so it's in that after-success state. If a user shuts that down, there doesn't need to be any confirmation at all. The only time there needs to be a confirmation is if they're going to try and delete it while it's in a non-terminal state, when it's in a state of either processing or blocked or start. It's in some other state, not the last one.

But also, another thing we want to add there is that before we actually delete or shut down an agent, we need to check to see if its working set is dirty, if there are any changes there at all. If there are any changes left over, we want to warn the user first.

**Interpretation (for processors):** Maps to **B** modified — update Destructive-action confirmation (`user.md` item 21): no confirmation when shutting down/removing an agent in **Waiting**; confirmation required in **Starting**, **Processing**, or **Blocked**. Before shutting down or removing any agent, warn if the agent’s working set has changes. No separate dismiss-as-done confirm. Park working-set dirty-check detail in to-process for design. Delete `queue/q-133.md`.

<!-- answer-anchor: q-133 -->

---

## q-134 — Error presentation for failed operations

**Question:** For failed user-requested operations, which prominent error presentation should tod use (separate from the in-progress status area)?

**A)** Modal dialog
**B)** Toast or banner
**C)** Dedicated error area or panel
**D)** Design decides — requirement is only that errors are clearly more visible than the status area

**Recommend:** D unless you already want a specific pattern day-one.
Which do you want?

**Answer:** alright, question 134, let's do a toast or a banner

**Interpretation (for processors):** Maps to **B** — write Operation failure feedback into `user.md` item 27 (toast or banner for failed user-requested operations; separate from status area). Mark q-132 error visibility draft consumed. Delete `queue/q-134.md`.

<!-- answer-anchor: q-134 -->

---

## q-135 — Requirements completeness review

**Question:** I’ve reviewed the current Requirements for this phase. These top-level areas look complete and coherent for a reasonable agent fleet management application — I don’t see a compelling gap to propose next:

1. Fleet UI and task/agent management (situational awareness, fuzzy search, tags, lifecycle, manual create/edit/delete, status area)
2. Agent fleet operations (concurrency, environments, runtime status, isolated worktrees, launch shell)
3. Human-in-the-loop and notifications
4. External integrations (Slack, Linear, GitHub, code editor)
5. Data persistence, import, credentials, and application settings
6. UX efficiency (keyboard reachability, customizable shortcuts, destructive confirms, diagnostic logging)
7. Application resource limits

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** Question number 135: this question seems really off because we're still defining other things, so I think we can skip this question for now

**Interpretation (for processors):** Skip/defer — completeness review is premature while intake is still defining requirements. Do **not** write to `user.md`. Park deferral note in `to-process.md`. Delete `queue/q-135.md`.

<!-- answer-anchor: q-135 -->

---

## q-136 — Supported code editor

**Question:** Code editor integration supports one editor in this phase. Which editor is that supported editor?

**A)** Zed
**B)** VS Code or Cursor
**C)** Other — name it

**Recommend:** A — matches your earlier local editor focus.
Which do you want?

**Answer:** question number 36: Zed, A

**Interpretation (for processors):** Maps to **A** — Zed is the supported editor. Update Code editor integration (`user.md` item 12) to name Zed. Delete `queue/q-136.md`.

<!-- answer-anchor: q-136 -->

---

## q-137 — Multiple shell sessions per agent

**Question:** Can the user open more than one shell session into the same agent’s environment at the same time?

**A)** Yes — multiple concurrent shell sessions per agent
**B)** No — at most one shell session per agent at a time

**Recommend:** B unless parallel shells are day-one.
Which do you want?

**Answer:** question number 137: A, yes

**Interpretation (for processors):** Maps to **A** — update Launch shell into agent environment (`user.md` item 25) to allow multiple concurrent shell sessions per agent. Delete `queue/q-137.md`.

<!-- answer-anchor: q-137 -->

---

## q-138 — Stop agent vs dismiss-as-done

**Question:** Are Stop agent and dismiss-as-done the same user action, or two different actions?

**A)** Same action
**B)** Different actions — stop pauses or halts the run; dismiss-as-done shuts down and removes the agent from the fleet

**Recommend:** B — matches your runtime status model (Waiting then dismiss removes the agent).
Which do you want?

**Answer:** question number 138. I'm not sure what "dismiss as done." I don't know if you know what that is. You made that up, so ignore that. What we're calling "stop agent" is the same thing. I've used the word "term shutdown." I kind of prefer the term "shut down" to make it clear that it's like that agent, its environment, everything is done, so let's use the term "shut down" for that action.

**Interpretation (for processors):** Maps to **A** modified — one user action only; retire “dismiss-as-done” and “stop agent” in favor of **shut down an agent** (agent + environment torn down; removed from fleet). Update `user.md` items 5, 17, and 21 for consistent “shut down” wording. Delete `queue/q-138.md`.

<!-- answer-anchor: q-138 -->

---

## Shell session tracking (q-137 supplement)

**Context:** Follow-up to Launch shell into agent environment (item 25 / q-137).

**Answer:** Another thing to note is that we need to keep track of shells into agent environments so we can switch to them. they could show up on the agent detail page.

**Interpretation (for processors):** Direction only — tod tracks open shell sessions per agent so the user can switch among them; shell sessions may appear on the agent detail page. Park draft in `to-process.md`; do **not** write `user.md` until Accept (likely extends item 25).

<!-- answer-anchor: shell-session-tracking -->

---

## q-139 — Autonomous mode behavior

**Question:** When an agent runs in autonomous mode, must it proceed without user approval for each step until it finishes, is blocked, or is interrupted?

**A)** Yes — autonomous means proceed without per-step user approval
**B)** No — autonomous still requires explicit user approval at each step

**Recommend:** A — that is the usual meaning of autonomous vs interactive.
Which do you want?

**Answer:** all right, question 139. Yes, autonomously proceed unless it actually receives a full block that it cannot resolve.

**Interpretation (for processors):** Maps to **A** — in autonomous mode, the agent proceeds without per-step user approval until it finishes, is interrupted, or hits a block it cannot resolve on its own (aligns with **Blocked** runtime status). Park Autonomous/interactive mode draft in `to-process.md`; do **not** write `user.md` until Accept (researcher contingent: pair with q-144). Delete `queue/q-139.md`.

<!-- answer-anchor: q-139 -->

---

## q-140 — Copy text from agent transcript

**Question:** Must the user be able to copy text from an agent’s transcript inside tod?

**A)** Yes — transcript text is copyable
**B)** No — leave copy behavior to design for now

**Recommend:** A — basic examination affordance for a fleet UI.
Which do you want?

**Answer:** question 140, transcript: yes, but that doesn't need to be a requirement because that is true for basically all text. I wouldn't worry about that one.

**Interpretation (for processors):** Substance is **A** (transcript text should be copyable) but **waive as Requirements** — assumed basic text UI behavior; do **not** add a Requirements item. Park waive note in `to-process.md`. Delete `queue/q-140.md`.

<!-- answer-anchor: q-140 -->

---

## q-141 — Shut down terminology Accept

**Question:** Approve this terminology update for agent shutdown?

```markdown
### Agents and environments (item 5)

Replace “Stop an agent” with “Shut down an agent” in the supported operations list.

### Agent runtime status (item 17)

When the user shuts down an agent, tod tears down the agent and its environment and removes it from the managed fleet (no durable post-shutdown state).
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** question 141, but that's not a requirement. That's what you need to do to the requirement document. You just need to update the document to use the term "shutdown" instead of "stop".

**Interpretation (for processors):** Not a new Requirements item — doc terminology pass only (replace “stop” with **shut down** for agent teardown). Already applied via q-138 in `user.md` items 5, 17, and 21; verify no stray “stop” remains. Delete `queue/q-141.md`.

<!-- answer-anchor: q-141 -->

---

## q-142 — Agent dirty working set recovery options

**Question:** When shutting down an agent whose working set has changes, must tod offer recovery options similar to dirty worktree reclaim (such as re-check, stash, or cancel)?

**A)** Yes — offer similar recovery options
**B)** No — warn only; user decides whether to proceed
**C)** Defer — decide in design; requirement is warn only for now

**Recommend:** C — q-133 already requires a warning; option details can stay in design.
Which do you want?

**Answer:** All right, question 142. Yes, offer similar recovery options.

**Interpretation (for processors):** Maps to **A** — update Destructive-action confirmation (`user.md` item 21): when shutting down an agent with a dirty working set, warn and offer recovery options similar to dirty worktree reclaim (item 24). Delete `queue/q-142.md`.

<!-- answer-anchor: q-142 -->

---

## q-143 — Agent transcript persistence

**Question:** Must agent transcripts persist across tod application restarts?

**A)** Yes — transcripts survive relaunch
**B)** No — transcripts are session-only for this phase

**Recommend:** A — aligns with examining agent work over time.
Which do you want?

**Answer:** all right, question 143. Yes.

**Interpretation (for processors):** Maps to **A** — update Fleet state persistence (`user.md` item 10) to include agent transcripts surviving relaunch. Delete `queue/q-143.md`.

<!-- answer-anchor: q-143 -->

---

## q-144 — Interactive mode behavior

**Question:** When an agent runs in interactive mode, must it wait for user input before each step?

**A)** Yes — interactive means wait for user input at each step
**B)** No — interactive still proceeds automatically between steps

**Recommend:** A — usual pairing with autonomous mode.
Which do you want?

**Answer:** question 144. Yes.

**Interpretation (for processors):** Maps to **A** — write Agent autonomous and interactive modes into `user.md` item 28 (pair with q-139 autonomous behavior). Mark q-139 draft consumed. Delete `queue/q-144.md`.

<!-- answer-anchor: q-144 -->

---

## q-145 — Environment type at agent launch

**Question:** When launching an agent, must the user choose which environment type to use (local, devcontainer, or Micro-VM)?

**A)** Yes — user picks environment type at launch
**B)** No — environment is inferred from task or defaults

**Recommend:** A — three environments are explicitly in scope.
Which do you want?

**Answer:** Question 145: Yes, the user picks it, but there should be some kind of a control somewhere on there that chooses the current mode. When they launch it, it'll use the current mode, but they can go and choose the current mode. The current mode, though, could also have a setting that says it's the equivalent of "ask every time." If they try to launch an agent on a task but the current mode is not specified, then it would prompt them to choose one. Otherwise, if the current mode is already set to a particular type, it would just launch that type.

**Interpretation (for processors):** Maps to **A** modified — user chooses environment type at launch. tod provides a control for the current default environment type; launches use that default. Default may be a specific type or ask every time; prompt at launch when ask every time or unset. Update Agents and environments (`user.md` item 5). Delete `queue/q-145.md`.

<!-- answer-anchor: q-145 -->

---

## q-146 — Launch without initial prompt

**Question:** When launching an agent, must the user be able to launch without submitting an initial prompt immediately?

**A)** Yes — launch first; prompt can come later
**B)** No — an initial prompt is required at launch

**Recommend:** A — matches the Starting runtime status.
Which do you want?

**Answer:** question 146. yes

**Interpretation (for processors):** Maps to **A** — update Agents and environments (`user.md` item 5): launching an agent does not require an initial prompt. Aligns with **Starting** runtime status (item 17). Delete `queue/q-146.md`.

<!-- answer-anchor: q-146 -->

---

## q-147 — Shell session tracking Accept

**Question:** Approve extending Launch shell into agent environment (item 25) with shell session tracking?

```markdown
tod tracks open shell sessions per agent so the user can switch among them; open shell sessions are visible on the agent detail page.
```

**A)** Accept as written
**B)** Modify — say what to change
**C)** Reject — drop or rework this item

Pick A, B, or C?

**Answer:** Question 147. Approved.

**Interpretation (for processors):** Maps to **A** — extend Launch shell into agent environment (`user.md` item 25) with shell session tracking as written. Mark shell-session-tracking draft consumed. Delete `queue/q-147.md`.

<!-- answer-anchor: q-147 -->

---

## q-148 — Agent detail page

**Question:** Must tod provide an agent detail page or view for each managed agent?

**A)** Yes — each agent has a detail page/view
**B)** No — leave agent detail layout to design for now

**Recommend:** A — shell sessions and examination need a place to land.
Which do you want?

**Answer:** question 148. There will be an agent detail page. We're not gonna design it until the design stage.

**Interpretation (for processors):** Maps to **A** modified — write Agent detail page into `user.md` item 29 (each managed agent has one). Page layout and composition deferred to design — park in `to-process.md`. Delete `queue/q-148.md`.

<!-- answer-anchor: q-148 -->

---

## q-149 — Autonomous or interactive mode at launch

**Question:** When launching an agent, must the user choose autonomous or interactive mode?

**A)** Yes — user picks mode at launch
**B)** No — mode defaults; user can switch later only

**Recommend:** A unless a single default is enough day-one.
Which do you want?

**Answer:** Question 149. I think this needs to be similar to how I talked before about there being a kind of a control that shows the environment mode. I think about the same thing, where there's a control that chooses autonomous versus interactive mode. The user picks it, but they can set it in the mode. If they already have it set, it will just automatically go with what's already set. One of the choices will also be "Ask every time." If they choose that, then they'll be prompted when they launch something.

**Interpretation (for processors):** Maps to **A** modified — same pattern as environment type at launch (q-145): control for current default autonomous/interactive mode; launches use default; default may be autonomous, interactive, or ask every time; prompt at launch when ask every time or unset. Update Agent autonomous and interactive modes (`user.md` item 28). Delete `queue/q-149.md`.

<!-- answer-anchor: q-149 -->

---

## q-150 — Interrupt vs shut down

**Question:** When the user interrupts an agent, must the agent stay in the managed fleet (not shut down)?

**A)** Yes — interrupt halts current activity; agent remains in the fleet
**B)** No — interrupt also shuts down the agent

**Recommend:** A — interrupt and shut down are separate operations in item 5.
Which do you want?

**Answer:** question 150. Yes, the only thing that removes an agent is shutdown.

**Interpretation (for processors):** Maps to **A** — interrupt halts current activity; agent stays in fleet. Shut down is the only operation that removes an agent. Update Agent runtime status (`user.md` item 17). Delete `queue/q-150.md`.

<!-- answer-anchor: q-150 -->

---

## q-151 — Submit prompt to Waiting agent

**Question:** When an agent is in Waiting status, must the user be able to submit a new prompt without relaunching the agent?

**A)** Yes — submit another prompt to a Waiting agent
**B)** No — Waiting means shut down or relaunch only

**Recommend:** A — matches “awaiting further instructions” in item 17.
Which do you want?

**Answer:** question 151. Yeah, the user can submit another prompt

**Interpretation (for processors):** Maps to **A** — update Agent runtime status (`user.md` item 17): when in Waiting, user can submit a new prompt without relaunching. Delete `queue/q-151.md`.

<!-- answer-anchor: q-151 -->

---

## q-152 — Requirements completeness review (second pass)

**Question:** I’ve reviewed the current Requirements for this phase. These top-level areas look complete and coherent for a reasonable agent fleet management application — I don’t see a compelling gap to propose next:

1. Fleet UI and task/agent management (situational awareness, fuzzy search, tags, lifecycle, manual create/edit/delete, status area)
2. Agent fleet operations (concurrency, environments, runtime status, worktrees, launch shell, autonomous/interactive modes)
3. Human-in-the-loop and notifications
4. External integrations (Slack, Linear, GitHub, Zed)
5. Data persistence, import, credentials, and application settings
6. UX efficiency (keyboard reachability, customizable shortcuts, destructive confirms, operation failure feedback, diagnostic logging)
7. Application resource limits

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** Question 152: This question is: Too soon! We are still dividing stuff! Why would you bring this out?

**Interpretation (for processors):** Skip/defer — same as q-135; completeness review premature while intake still defining requirements. Do **not** write to `user.md`. Park deferral note in `to-process.md`. Delete `queue/q-152.md`.

<!-- answer-anchor: q-152 -->

---

## q-153 — Task detail page

**Question:** Must tod provide a task detail page or view for each task?

**A)** Yes — each task has a detail page/view
**B)** No — leave task detail layout to design for now

**Recommend:** A — pairs with agent detail and multi-granularity examination.
Which do you want?

**Answer:** question 153. Yes, it's basically kind of the same as the edit also. It's a view/edit view, but we're not going to design it at this point. We'll design it during the design phase.

**Interpretation (for processors):** Maps to **A** modified — write Task detail page into `user.md` item 30 (view/edit task fields; pairs with item 18). Page layout and composition deferred to design — park in `to-process.md`. Delete `queue/q-153.md`.

<!-- answer-anchor: q-153 -->

---

## q-154 — Launch agent requires task context

**Question:** When launching an agent, must the user launch it in the context of a specific task?

**A)** Yes — every agent launch is for a specific task
**B)** No — agents can be launched without task context

**Recommend:** A — matches one-to-many task→agent association in item 2.
Which do you want?

**Answer:** Question 154. Yes.

**Interpretation (for processors):** Maps to **A** — every agent launch is for a specific task. Update Agents and environments (`user.md` item 5). Delete `queue/q-154.md`.

<!-- answer-anchor: q-154 -->

---

## q-155 — Agent transcript full history

**Question:** Must an agent’s transcript show the full history of prompts and responses for that agent session?

**A)** Yes — full session transcript history is visible
**B)** No — only the current or latest prompt needs to be visible

**Recommend:** A — needed for examining agent work over time.
Which do you want?

**Answer:** question 155. Yes.

**Interpretation (for processors):** Maps to **A** — update Agents and environments (`user.md` item 5): viewing an agent’s transcript shows full session prompt/response history. Delete `queue/q-155.md`.

<!-- answer-anchor: q-155 -->

---

## q-156 — Post-interrupt runtime status

**Question:** After the user interrupts an agent, which runtime status should it show?

**A)** Waiting
**B)** Blocked
**C)** Design decides — interrupt behavior is enough for this phase

**Recommend:** C unless you want a specific status day-one.
Which do you want?

**Answer:** question 156: Blocked.

**Interpretation (for processors):** Maps to **B** — after interrupt, agent runtime status is Blocked. Update Agent runtime status (`user.md` item 17). Delete `queue/q-156.md`.

<!-- answer-anchor: q-156 -->

---

## q-157 — Associated task on agent detail page

**Question:** From the agent detail page, must the associated task be visible?

**A)** Yes — associated task is visible on the agent detail page
**B)** No — leave that association visibility to design

**Recommend:** A — ties agent examination back to task context.
Which do you want?

**Answer:** question 157. Yes.

**Interpretation (for processors):** Maps to **A** — extend Agent detail page (`user.md` item 29): associated task visible on the page. Delete `queue/q-157.md`.

<!-- answer-anchor: q-157 -->

---

## q-158 — Associated agents on task detail page

**Question:** From the task detail page, must associated agents be visible?

**A)** Yes — associated agents are visible on the task detail page
**B)** No — leave that association visibility to design

**Recommend:** A — pairs with task→agent one-to-many model.
Which do you want?

**Answer:** Question 158. Yes.

**Interpretation (for processors):** Maps to **A** — extend Task detail page (`user.md` item 30): associated agents visible on the page. Delete `queue/q-158.md`.

<!-- answer-anchor: q-158 -->

---

## q-159 — Submit prompt to Blocked agent

**Question:** When an agent is Blocked, must the user be able to respond via Submit prompt (not only through the notification queue)?

**A)** Yes — Submit prompt works for Blocked agents
**B)** No — notification queue is the only response path

**Recommend:** A — both paths should reach the agent.
Which do you want?

**Answer:** Question 159: Yes

**Interpretation (for processors):** Maps to **A** — update Agent runtime status (`user.md` item 17): user can submit a prompt to a Blocked agent without relaunching (in addition to notification queue). Delete `queue/q-159.md`.

<!-- answer-anchor: q-159 -->

---

## q-160 — Runtime status on agent detail page

**Question:** Must the agent detail page show the agent’s current runtime status (Starting, Processing, Blocked, or Waiting)?

**A)** Yes — runtime status is visible on the agent detail page
**B)** No — leave status placement to design

**Recommend:** A — status is core to examining a running agent.
Which do you want?

**Answer:** question 160. Yes, but don't go into all the details about the detail pages right now. We'll design that in the design phase. Don't go into that right now.

**Interpretation (for processors):** Maps to **A** modified — extend Agent detail page (`user.md` item 29): runtime status visible on the page. Do **not** add layout/placement detail; page composition remains design-phase (same as q-148). Park deferral in existing agent detail page layout overflow in `to-process.md`. Delete `queue/q-160.md`.

<!-- answer-anchor: q-160 -->

---

## q-164 — Open agent detail page from notification

**Question:** From a notification in the managed queue, must the user be able to open the related agent detail page?

**A)** Yes — notification flow can open the agent detail page
**B)** No — leave notification navigation to design

**Recommend:** A — HITL already requires seeing involved agents and responding.
Which do you want?

**Answer:** Question 164. Yes.

**Interpretation (for processors):** Maps to **A** — extend Human-in-the-loop (`user.md` item 4): from a notification, user can open the related agent detail page. Delete `queue/q-164.md`.

<!-- answer-anchor: q-164 -->

---

## q-165 — Open task detail page from notification

**Question:** From a notification in the managed queue, must the user be able to open the related task detail page?

**A)** Yes — notification flow can open the task detail page
**B)** No — leave notification navigation to design

**Recommend:** A — HITL already requires seeing the related task from a notification.
Which do you want?

**Answer:** Okay, again, this is a similar question to q-164. We're not going into the details. We're not designing the pages. We're not designing the views right now, so don't go into the details of these pages. We will do that in the design phase.

**Interpretation (for processors):** Maps to **A** modified — extend Human-in-the-loop (`user.md` item 4): from a notification, user can open the related task detail page. Do **not** add layout/navigation detail; notification and detail-page composition remain design-phase (same standing rule as q-160/q-164). Park in `to-process.md`. Delete `queue/q-165.md`.

<!-- answer-anchor: q-165 -->

---

## q-167 — Submit prompt while Processing

**Question:** When an agent is Processing, must the user be able to submit a new prompt without relaunching (halting current work)?

**A)** Yes — submit prompt to a Processing agent halts current work
**B)** No — Processing agents accept new prompts only after Interrupt or another explicit step

**Recommend:** A — submit prompt is already a supported operation in item 5.
Which do you want?

**Answer:** When an agent is processing, the user can submit a new prompt that will be either an interrupting prompt or just an add-to-queue prompt, either one. They can choose.

**Interpretation (for processors):** Supersedes A/B — when Processing, user can submit a new prompt and choose interrupting (stops current work) or add-to-queue. Update Agent runtime status (`user.md` item 17). Prompt queue mechanics → design overflow in `to-process.md`. Delete `queue/q-167.md`.

<!-- answer-anchor: q-167 -->

---

## q-161 / q-162 / q-163 / q-166 — Detail-page view questions deferred

**Context:** User deferred the following open queue questions to the design phase; do not lock view/layout answers in Requirements during intake.

**q-161 — Agent detail page: transcript visible?**
**A)** Yes — transcript on agent detail page **B)** No — design decides
**q-162 — Agent detail page: autonomous/interactive mode visible?**
**A)** Yes **B)** No — design decides
**q-163 — Task detail page: linked Linear issues and GitHub PRs visible?**
**A)** Yes **B)** No — design decides
**q-166 — Open Zed from agent detail page (not only fleet list)?**
**A)** Yes **B)** No — fleet list enough

**Answer:** The current questions 161, 162, 163, and 166 are deferred to the design phase. We're not designing views during this process state.

**Interpretation (for processors):** Defer/skip all four — do **not** write `user.md` Requirements for these view-placement items now. Park in `to-process.md` for design. Delete `queue/q-161.md`, `q-162.md`, `q-163.md`, `q-166.md`.

<!-- answer-anchor: q-161-q-166-deferred -->

---

## q-168 — Worktree reclaim trigger

**Question:** When must tod reclaim an agent’s isolated git worktree?

**A)** When the user shuts down the agent — reclaim as part of teardown (subject to dirty-worktree warning)
**B)** Only when the user explicitly requests reclaim — shut down leaves the worktree in place
**C)** Design decides — provide-and-reclaim capability is enough for this phase

**Recommend:** A — shut down tears down the agent environment; worktree reclaim fits that flow.
Which do you want?

**Answer:** For question 168, the work tree gets reclaimed and shut down, but the user can also explicitly reclaim it.

**Interpretation (for processors):** Maps to **A** modified — reclaim on shut down (subject to item 24) **and** user can explicitly reclaim. Update Isolated worktrees (`user.md` item 14). Delete `queue/q-168.md`.

<!-- answer-anchor: q-168 -->

---

## q-169 — Autonomous/interactive mode switch timing

**Question:** When the user switches a running agent between autonomous and interactive mode, when must the new mode take effect?

**A)** Immediately — the agent’s next step follows the new mode
**B)** Only after the current prompt or step finishes

**Recommend:** A — mode switch is an explicit user intervention on a live agent.
Which do you want?

**Answer:** For question 169, both your answers are basically the same. It happens in a non-disruptive way, whatever that happens to be, right? It doesn't interrupt the agent, but it starts as soon as it makes sense.

**Interpretation (for processors):** Supersedes A/B — mode switch is non-disruptive: does not interrupt current work; new mode applies as soon as it makes sense. Update Agent autonomous and interactive modes (`user.md` item 28). Delete `queue/q-169.md`.

<!-- answer-anchor: q-169 -->

---

## q-170 — Submit prompt from notification

**Question:** From a notification in the managed queue, must the user be able to submit a prompt to the blocked agent through tod (not only navigate to the agent elsewhere)?

**A)** Yes — respond includes submitting a prompt through tod
**B)** No — user opens the agent and submits a prompt there

**Recommend:** A — HITL emphasizes responding to blocked agents from the notification flow.
Which do you want?

**Answer:** question 170. Yes.

**Interpretation (for processors):** Maps to **A** — extend Human-in-the-loop (`user.md` item 4): from a notification, user can submit a prompt to the related agent through tod. Delete `queue/q-170.md`.

<!-- answer-anchor: q-170 -->

---

## q-171 — Task lifecycle state jumps

**Question:** When changing a task’s lifecycle state, must the user be able to jump to any other state in the ordered set (not only the next or previous state)?

**A)** Yes — any state in the set is selectable
**B)** No — only adjacent transitions for this phase

**Recommend:** A — large task sets need flexible status correction without walking every step.
Which do you want?

**Answer:** question 171: Getting in too many details.

**Interpretation (for processors):** Defer to design — do **not** lock lifecycle transition rules in Requirements now. Park in `to-process.md`. Delete `queue/q-171.md`.

<!-- answer-anchor: q-171 -->

---

## q-172 — New devcontainer launch

**Question:** For devcontainer agents, must tod also support launching into a newly created devcontainer (not only an existing devcontainer the user already has open)?

**A)** Yes — both existing and newly created devcontainers must be supported
**B)** No — existing devcontainer only is enough for this phase

**Recommend:** A — item 5 already requires existing; new devcontainer launch completes the environment capability.
Which do you want?

**Answer:** Defer to the design phase for now. We'll just say existing dev container.

**Interpretation (for processors):** Maps to **B** for this phase — existing devcontainer only (already in `user.md` item 5). New devcontainer launch deferred to design. Park in `to-process.md`. Delete `queue/q-172.md`.

<!-- answer-anchor: q-172 -->

---

## q-173 — Auto notification on Blocked

**Question:** When an agent enters Blocked status, must tod automatically add a notification to the managed queue?

**A)** Yes — Blocked agents automatically create a queue notification
**B)** No — the user or agent must explicitly raise a notification some other way

**Recommend:** A — HITL expects agents to reach the user when blocked without extra steps.
Which do you want?

**Answer:** Question 173: The answer is yes.

**Interpretation (for processors):** Maps to **A** — extend Human-in-the-loop (`user.md` item 4): Blocked agents automatically create a queue notification. Delete `queue/q-173.md`.

<!-- answer-anchor: q-173 -->

---

## q-174 — Initial task lifecycle state

**Question:** When the user creates a task manually or from a Linear issue, which lifecycle state must it start in?

**A)** proposed — always the first state in the ordered set
**B)** User picks the initial state at creation time
**C)** Design decides — lifecycle set is enough for this phase

**Recommend:** A — matches the ordered lifecycle model and keeps create flows simple.
Which do you want?

**Answer:** All tasks initially start at proposed.

**Interpretation (for processors):** Maps to **A** — update Task lifecycle (`user.md` item 16): new tasks start in proposed. Delete `queue/q-174.md`.

<!-- answer-anchor: q-174 -->

---

## q-176 — Worktree from task repo/branch

**Question:** When tod creates an isolated git worktree for an agent, must it use the associated task's git repository and branch when those fields are set on the task?

**A)** Yes — worktree is created from the task's repo and branch when set
**B)** No — repo/branch are chosen separately at agent launch regardless of task fields
**C)** Design decides — isolated worktrees are enough for this phase

**Recommend:** A — task git repo/branch (item 18) should drive where the agent works.
Which do you want?

**Answer:** For question 176, it uses info from the task.

**Interpretation (for processors):** Maps to **A** — update Isolated worktrees (`user.md` item 14): worktree uses task git repository and branch when set. Delete `queue/q-176.md`.

<!-- answer-anchor: q-176 -->

---

## q-177 — One worktree per agent

**Question:** When a task has multiple associated agents, must each agent receive its own isolated git worktree?

**A)** Yes — one isolated worktree per agent
**B)** No — agents on the same task may share a worktree

**Recommend:** A — agents likely work on different branches/worktrees.
Which do you want?

**Answer:** Question 177: yes.

**Interpretation (for processors):** Maps to **A** — update Isolated worktrees (`user.md` item 14): each agent gets its own worktree. Delete `queue/q-177.md`.

<!-- answer-anchor: q-177 -->

---

## q-178 — Notification shows agent message

**Question:** Must a notification in the managed queue include the agent's question or blocker message text (not only links to the related task and agents)?

**A)** Yes — the notification shows what the agent needs from the user
**B)** No — user opens the agent to see the question; notification is only a pointer

**Recommend:** A — HITL responding requires knowing what the agent asked without extra navigation.
Which do you want?

**Answer:** Question 178: yes.

**Interpretation (for processors):** Maps to **A** — extend Human-in-the-loop (`user.md` item 4): notifications include agent question/blocker text. Delete `queue/q-178.md`.

<!-- answer-anchor: q-178 -->

---

## q-175 — Explicit notification resolve

**Question:** Must the user be able to explicitly resolve (dismiss) a notification from the managed queue after acting on it?

**A)** Yes — user can mark a notification resolved so it leaves the open queue
**B)** No — resolution mechanics are design-phase; persist-until-resolved is enough for now

**Recommend:** A — item 4 success criteria already require notifications to be resolved.
Which do you want?

**Answer:** Too much detail.

**Interpretation (for processors):** Defer to design — do **not** lock notification resolution/dismiss mechanics in Requirements now. Item 4 already requires notifications persist until resolved. Park in `to-process.md`. Delete `queue/q-175.md`.

<!-- answer-anchor: q-175 -->

---

## q-179 — Import duplicate slug/title

**Question:** When import encounters a task whose slug or title conflicts with an existing task, what must tod do?

**A)** Skip the conflicting record and continue importing the rest
**B)** Fail the entire import
**C)** Import the record with an adjusted slug or title to avoid the conflict

**Recommend:** A — matches lenient partial-success import (item 13).
Which do you want?

**Answer:** Too much detail.

**Interpretation (for processors):** Defer to design — do **not** lock import duplicate-handling rules in Requirements now. Item 13 covers lenient partial-success import. Park in `to-process.md`. Delete `queue/q-179.md`.

<!-- answer-anchor: q-179 -->

---

## q-180 — Shell session restart persistence

**Question:** Must open shell sessions into agent environments survive a tod application restart?

**A)** Yes — shell sessions are restored after relaunch
**B)** No — shell sessions are ephemeral; the user relaunches shells after restart

**Recommend:** B — item 10 persistence covers tasks, agents, transcripts, and notifications, not shells.
Which do you want?

**Answer:** Question 180: yes.

**Interpretation (for processors):** Maps to **A** — update Fleet state persistence (`user.md` item 10): open shell sessions survive restart. Delete `queue/q-180.md`.

<!-- answer-anchor: q-180 -->

---

## q-181 — Agent launch without task git repo

**Question:** When launching a local or devcontainer agent for a task that has no git repository set, must agent launch still succeed?

**A)** Yes — launch succeeds; worktree/repo selection is handled separately (user picks at launch or agent runs without an isolated worktree)
**B)** No — launch is blocked until the task has a git repository set
**C)** Design decides — isolated worktrees are enough for this phase

**Recommend:** A — most task fields are optional (item 18); blocking launch would be unusually strict.
Which do you want?

**Answer:** No, it's blocked.

**Interpretation (for processors):** Maps to **B** — update Isolated worktrees (`user.md` item 14): local/devcontainer agent launch blocked until task has git repository set. Delete `queue/q-181.md`.

<!-- answer-anchor: q-181 -->

---

## q-182 — Task delete reclaims worktrees

**Question:** When permanently deleting a task that has no associated agents but still has unreclaimed isolated worktrees, must tod reclaim those worktrees as part of delete?

**A)** Yes — delete reclaims orphaned worktrees (subject to dirty-worktree warning)
**B)** No — worktree reclaim is a separate explicit operation
**C)** Design decides — explicit reclaim capability is enough for this phase

**Recommend:** A — delete should not leave orphaned worktrees behind.
Which do you want?

**Answer:** Yes, it must reclaim them. Subject to the dirty worktree warning: yes.

**Interpretation (for processors):** Maps to **A** — update Manual task create, edit, and delete (`user.md` item 18): delete reclaims unreclaimed worktrees (subject to item 24). Delete `queue/q-182.md`.

<!-- answer-anchor: q-182 -->

---

## q-175 — Explicit resolve notification

**Question:** Must the user be able to explicitly resolve (dismiss) a notification from the managed queue after acting on it?

**A)** Yes — user can mark a notification resolved so it leaves the open queue
**B)** No — resolution mechanics are design-phase; persist-until-resolved is enough for now

**Recommend:** A — item 4 success criteria already require notifications to be resolved.
Which do you want?

**Answer:** Getting into too many details again. 175: too much detail.

**Interpretation (for processors):** Defer to design — do **not** lock notification resolve/dismiss mechanics in Requirements now. Park in `to-process.md`. Delete `queue/q-175.md`.

<!-- answer-anchor: q-175 -->

---

## q-179 — Import slug/title conflict

**Question:** When import encounters a task whose slug or title conflicts with an existing task, what must tod do?

**A)** Skip the conflicting record and continue importing the rest
**B)** Fail the entire import
**C)** Import the record with an adjusted slug or title to avoid the conflict

**Recommend:** A — matches lenient partial-success import (item 13).
Which do you want?

**Answer:** 179: too much detail.

**Interpretation (for processors):** Defer to design/planning — do **not** lock import conflict behavior in Requirements now. Park in `to-process.md`. Delete `queue/q-179.md`.

<!-- answer-anchor: q-179 -->

---

## q-180 — Shell session persistence

**Question:** Must open shell sessions into agent environments survive a tod application restart?

**A)** Yes — shell sessions are restored after relaunch
**B)** No — shell sessions are ephemeral; the user relaunches shells after restart

**Recommend:** B — item 10 persistence covers tasks, agents, transcripts, and notifications, not shells.
Which do you want?

**Answer:** Question 180: yes.

**Interpretation (for processors):** Maps to **A** — update Launch shell into agent environment (`user.md` item 25): open shell sessions survive application restart. Delete `queue/q-180.md`.

<!-- answer-anchor: q-180 -->

---

## q-181 — Launch without task git repository

**Question:** When launching a local or devcontainer agent for a task that has no git repository set, must agent launch still succeed?

**A)** Yes — launch succeeds; worktree/repo selection is handled separately (user picks at launch or agent runs without an isolated worktree)
**B)** No — launch is blocked until the task has a git repository set
**C)** Design decides — isolated worktrees are enough for this phase

**Recommend:** A — most task fields are optional (item 18); blocking launch would be unusually strict.
Which do you want?

**Answer:** Question 181: no, it's blocked.

**Interpretation (for processors):** Maps to **B** — update Isolated worktrees (`user.md` item 14): launching local/devcontainer agent blocked until task has git repository set. Delete `queue/q-181.md`.

<!-- answer-anchor: q-181 -->

---

## q-182 — Task delete reclaims worktrees

**Question:** When permanently deleting a task that has no associated agents but still has unreclaimed isolated worktrees, must tod reclaim those worktrees as part of delete?

**A)** Yes — delete reclaims orphaned worktrees (subject to dirty-worktree warning)
**B)** No — worktree reclaim is a separate explicit operation
**C)** Design decides — explicit reclaim capability is enough for this phase

**Recommend:** A — delete should not leave orphaned worktrees behind.
Which do you want?

**Answer:** Question 182: yes, it must reclaim them. Subject to the dirty work tree warning: yes.

**Interpretation (for processors):** Maps to **A** — update Manual task create, edit, and delete (`user.md` item 18): delete reclaims unreclaimed worktrees subject to item 24. Delete `queue/q-182.md`.

<!-- answer-anchor: q-182 -->

---

## q-183 — Requirements completeness review

**Question:** I've reviewed the current requirements for this phase. These top-level areas look complete and coherent for a fleet-management app of this kind — I don't see a compelling gap to propose next:

1. Core UI — situational awareness, fuzzy search, status area, error feedback, detail pages
2. Tasks — concurrent model, lifecycle, manual create/edit/delete, tags
3. Agents — environments, runtime status, modes, worktrees, shell access
4. Human-in-the-loop — managed notification queue
5. Integrations — Slack, Linear, GitHub, Zed
6. Persistence & configuration — fleet state, credentials, settings, import
7. Safety — destructive-action confirmation, dirty worktree warning
8. Efficiency — keyboard use, shortcuts, app resource limits, diagnostic logging

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** All right, question 183. I guess we're good to go then. This is enough for now.

**Interpretation (for processors):** Maps to **A** — requirements coverage enough for this phase; no new Requirements. Do not write to `user.md`. Delete `queue/q-183.md`. Set researcher status `complete`.

<!-- answer-anchor: q-183 -->
