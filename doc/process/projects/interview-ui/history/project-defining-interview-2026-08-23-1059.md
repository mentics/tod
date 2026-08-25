# Project defining interview — interview-ui — 2026-08-23

## Session

**User request:** `/process new project: let's build the interview ui`

**Prior context:** Product ideas volunteered in `.local/doc/interview-ui/ideas.md` (parked in entity `to-process.md`).

---

## q-001

**Question:** You're building a UI for structured interviews (reads a question queue folder, live updates, per-question answers, optional deep-dive branches, auto-replenish when the queue runs low). In one sentence, what is the main outcome Interview UI should deliver for you?

**Answer:** The goal of the Interview UI: make it as efficient as possible to create the documentation necessary to proceed to the next state in a process. That's deliberately generic because this interviewer and interview UI will be used in a number of places and ways — fundamentally a collaboration between the user and agents, and from a goal perspective I don't want to restrict it. Secondary goal: with the minimum necessary effort from the user — really important because the whole thing leverages agents and their capabilities as much as possible.

<!-- answer-anchor: q-001 -->

---

## q-002

**Question:** Who is the primary day-to-day operator of Interview UI — you alone on your machine, teammates on shared projects, or both?

**Answer:** Primary user is just one user, the local user, who is typically a software engineer.

<!-- answer-anchor: q-002 -->

---

## q-003

**Question:** Where should Interview UI run?

**A)** Standalone desktop app (separate from the main tod window)
**B)** Embedded view or panel inside the existing tod GPUI desktop app
**C)** Web app in a browser
**D)** Other (describe briefly)

**Recommend:** B — keeps the interview workflow inside the tod ecosystem you already use.
Which do you want?

**Answer:** B — running inside the tod desktop app.

<!-- answer-anchor: q-003 -->

---

## q-004

**Question:** When the question queue folder changes on disk, how quickly must the UI reflect new or removed questions?

**A)** Near-instant (within about one second of filesystem events)
**B)** Within a few seconds is fine
**C)** Manual refresh only is acceptable for the first release

**Recommend:** A — matches your OS-level file watching intent from the initial ideas.
Which do you want?

**Answer:** A — near-instant (within about one second of filesystem events).

<!-- answer-anchor: q-004 -->

---

## q-005

**Question:** When a user submits an answer to one queued question, how should that reach the session that processes answers and updates the transcript?

**A)** Start a fresh processing session for every submission
**B)** Reuse one long-lived processing session across submissions in the same interview
**C)** Ship v1 with fresh sessions per submission and measure before deciding reuse

**Recommend:** C — matches your "measure and see" note in the initial ideas.
Which do you want?

**Answer:** C — ship v1 with fresh processing sessions per submission and measure before deciding reuse.

<!-- answer-anchor: q-005 -->

---

## q-006

**Question:** When the open question count drops below a threshold, the UI should automatically request more questions. What threshold should trigger replenishment?

**A)** Below 3 open questions
**B)** Below 5 open questions
**C)** Below 8 open questions (match the default researcher target)
**D)** Other number (specify)

**Recommend:** A — keeps a small buffer without over-spawning researcher runs.
Which do you want?

**Answer:** C — below 8 open questions triggers replenishment. However this should be configurable in settings (not hardcoded preferences — available in application settings).

<!-- answer-anchor: q-006 -->

---

## q-007

**Question:** For a deep-dive branch on a specific queued question, what should the branch produce when it finishes?

**A)** A separate interview transcript only — the parent question stays open until answered in the main flow
**B)** An answer the user can submit back to resolve the parent queue question
**C)** Both — branch explores details, then the user submits a synthesized answer to the parent
**D)** Other (describe briefly)

**Recommend:** C — exploration plus closure on the original question.
Which do you want?

**Answer:** If the user needs to discuss a particular question separately, the end result at some point in that branch process will find an answer. It submits to the answer processor agent. The UI handles bringing the answer back and submitting it as the answer to the parent question — automatic, no copy-paste by the user.

<!-- answer-anchor: q-007 -->

---

## q-008

**Question:** For constraints on an app like this, a few categories still look worth considering (not yet recorded):

1. Platform and runtime limits (OS support, must run alongside tod, offline use)
2. Security and privacy (local-only data, no network for v1)
3. Compatibility (must read the existing interview queue file format unchanged)
4. Resource bounds (max concurrent agent sessions, filesystem watch limits)

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B — at least compatibility (#3) seems binding given the file-based queue design.
Which do you want?

**Answer:** For all four constraint categories (platform/runtime, security/privacy, compatibility, resource bounds): inherit from the tod project. This is just views/functionality encapsulated inside the tod desktop app. Do NOT copy tod constraints into this project — only reference that running inside the tod desktop app inherits all environmental requirements the tod project already establishes (see doc/process/projects/tod/user.md).

<!-- answer-anchor: q-008 -->

---

## q-009

**Question:** Queue folder sync — The UI watches the interview question queue folder via OS-level filesystem events and reflects added, removed, or changed questions within one second.

Success criteria:
1. After a queue file is created, deleted, or modified on disk, the UI list matches within one second
2. Sync uses filesystem watching (not manual refresh only)

Accept, Modify, or Reject this requirement wording?

**Answer:** Modify. Remove the success criteria. Clarify automatic without user interaction — just add the word "automatically". Accepted modified wording: Queue folder sync — The UI watches the interview question queue folder via OS-level filesystem events and automatically reflects added, removed, or changed questions within one second.

<!-- answer-anchor: q-009 -->

---

## q-010

**Question:** Primary operator — The sole intended user is the single local user on the machine, typically a software engineer.

Accept, Modify, or Reject this constraint wording?

**Answer:** Accept as-is: Primary operator — The sole intended user is the single local user on the machine, typically a software engineer.

<!-- answer-anchor: q-010 -->

---

## q-011

**Question:** When several questions are queued, how should the user navigate them?

**A)** List view — see all open questions and pick any to answer in any order
**B)** Single-question focus — UI shows one question at a time (next/previous or auto-advance)
**C)** Other (describe briefly)

**Recommend:** A — matches your initial idea of a list the user interacts with separately.
Which do you want?

**Answer:** Choose A (list view direction), but do NOT define views/design yet — design stage later; don't be over-restrictive. Primary requirement: it will be extremely efficient to navigate the questions, choose existing options in the questions when there are multiple choices, and also edit proposed statements directly. For example, if the question is "Approve this statement" or something like that, the user should be able to edit that statement and then submit it back as an approved statement with those changes. Park "list view" as directional preference for design; do not over-constrain UI layout in user.md.

<!-- answer-anchor: q-011 -->

---

## q-012

**Question:** When the user submits an answer for one queued question, what should the UI pass to the answer processor?

**A)** Question id and user answer text only
**B)** Question id, answer text, and interview session paths (config, transcript, queue)
**C)** Other (describe briefly)

**Recommend:** B — the processor needs session paths to update the transcript and queue.
Which do you want?

**Answer:** A — Question id and user answer text only.

<!-- answer-anchor: q-012 -->

---

## q-013

**Question:** When replenishment triggers (open count below threshold), how should the UI invoke the researcher agent?

**A)** Start a fresh researcher session each time
**B)** Reuse one long-lived researcher session for the interview
**C)** Ship v1 with fresh sessions each time and measure before deciding reuse

**Recommend:** A — matches your initial idea that a fresh view is useful for each replenishment run.
Which do you want?

**Answer:** Start with the simplest implementation and measure over time; may choose differently later. Same approach applies to answer-processor sessions (previous q-005): start simple, measure. For researcher on replenishment: start with fresh researcher session each time (simplest). v1 uses fresh researcher sessions on replenishment; session strategy for researcher and answer-processor may be revisited after measurement.

<!-- answer-anchor: q-013 -->

---

## q-014

**Question:** How should the user start a deep-dive branch on a specific queued question?

**A)** Action on that question's row (e.g. branch button)
**B)** Separate command or menu that picks a question from the list
**C)** Other (describe briefly)

**Recommend:** A — keeps branch tied to the question being explored.
Which do you want?

**Answer:** A — Action on that question's row (e.g. branch button). Outcome-level: user can start a deep-dive branch from a specific queued question; park UI chrome detail for design.

<!-- answer-anchor: q-014 -->

---

## q-015

**Question:** When displaying a queued question's body from its markdown file, should the UI render structured parts (option letters A/B/C, Recommend lines) as formatted UI, or show the raw markdown text?

**A)** Render options and recommendations as formatted UI elements
**B)** Show the question body as plain/markdown text only
**C)** Other (describe briefly)

**Recommend:** A — makes option-style questions easier to answer.
Which do you want?

**Answer:** Sort of A plus more. Goal: when it's a multiple-choice question, the user can keyboard-select one of the options. The UI must support this. Interview agents will be updated to reply in specific formats that are easy for the UI to parse and display appropriately. Goal for this: user can efficiently use the keyboard to select one of the options.

<!-- answer-anchor: q-015 -->

---

## q-016

**Question:** How should Interview UI know which interview session's queue folder to watch?

**A)** Passed in when opening the view (e.g. interview config path from the hosting process flow)
**B)** User picks from a list of active or recent interview sessions
**C)** Fixed path per project entity (one interview at a time)
**D)** Other (describe briefly)

**Recommend:** A — fits embedding inside tod when a process step launches the interview.
Which do you want?

**Answer:** Support multiple simultaneous interviews. User may launch an interview for a particular task/state; eventually from task → launch interview (A-like). Also want a view of interview sessions (B) and an archive of interview sessions. For THIS initial project (task list part of tod not implemented yet): build self-enclosed — list of interview sessions and ability to launch a new interview from that list view. Current phase of this project: do NOT define integration from the rest of the tod application; self-enclosed functionality inside the tod application.

<!-- answer-anchor: q-016 -->

---

## q-017

**Question:** Efficient question interaction — It is extremely efficient to navigate the questions, choose existing options in the questions when there are multiple choices, and also edit proposed statements directly. For example, if the question is "Approve this statement" or something like that, the user should be able to edit that statement and then submit it back as an approved statement with those changes.

Success criteria:
1. From a wording-approval style question, the user can edit the proposed statement text in place and submit that edited text as the answer without copying it elsewhere

Accept, Modify, or Reject this requirement wording (statement + success criteria)?

**Answer:** Accept as-is (requirement 4 efficient question interaction + success criteria about in-place edit and submit for wording-approval questions).

<!-- answer-anchor: q-017 -->

---

## q-018

**Question:** For requirements on an Interview UI like this, a few categories still look worth considering (not yet recorded as top-level areas):

1. Agent run feedback — visible in-flight / success / failure for researcher replenishment and answer-processor runs
2. Interview completion — how the UI treats an interview when the queue is empty and no further questions are coming
3. Error recovery — what the UI does when an agent run fails (retry, leave question open, surface the error)

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B — at least #1 and #2 usually matter for an agent-driven UI.
Which do you want?

**Answer:** Pursue all three categories with user's substance:
- Agent run feedback: yes, especially errors must be visible. For a question where user submitted an answer, deactivate/gray it out. Queue can MODIFY existing questions (not just remove/add) based on clarifications — if answer submitted and queue file modified, that's a response: re-enable question in UI and update display with new content.
- Interview completion: yes — mark when interview is complete and take new action based on that.
- Error recovery: yes, need that too.
Write requirements from this substance (measurable where possible). Don't over-design UI chrome but capture outcomes.

<!-- answer-anchor: q-018 -->

---

## q-019

**Question:** I've reviewed the current constraints for this phase. These top-level areas look complete and coherent for a reasonable application of this kind — I don't see a compelling gap to propose next:

1. Embedded inside the tod GPUI desktop app
2. Environmental requirements inherit from tod (no duplication)
3. Primary operator — single local user / software engineer
4. Self-enclosed project phase — session list, launch, and archive only (no tod task-list integration yet)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** B — No, constraints not complete. Do NOT treat constraints as done. Delete queue file; researcher will follow up on gaps.

<!-- answer-anchor: q-019 -->

---

## q-020

**Question:** After a user archives an interview session, should that session remain reopenable from the archive view with session files preserved on disk?

**A)** Yes — reopenable from archive; files preserved
**B)** No — archive is a terminal close for this phase; reopen not required

**Recommend:** A — keeps history without destroying the session.
Which do you want?

**Answer:** A — archived sessions remain reopenable from archive view with session files preserved on disk.

<!-- answer-anchor: q-020 -->

---

## q-021

**Question:** When the user launches a new interview from the session list, what must the UI do?

**A)** Create the interview session scaffolding (config, transcript, queue) and open that session
**B)** Only open/bind to a session that already exists on disk
**C)** Other (describe briefly)

**Recommend:** A — otherwise "launch new" cannot work in a self-enclosed UI.
Which do you want?

**Answer:** User initiates "create new interview session" with context (what is it for: new project, new task, existing project/task, etc.). UI submits appropriate command/context to the researcher agent (question generator). Researcher creates scaffolding (config, transcript, queue, etc.) as part of that first request — UI does NOT create scaffolding separately.

<!-- answer-anchor: q-021 -->

---

## q-022

**Question:** If replenishment triggers while a researcher run for that same interview is already in progress, what should the UI do?

**A)** Skip starting another researcher until the in-flight run finishes
**B)** Start another researcher session anyway
**C)** Other (describe briefly)

**Recommend:** A — avoids stacked duplicate replenishments.
Which do you want?

**Answer:** Allow multiple researchers running at same time. Setting for max count of concurrent researchers. User notes it's unclear what would trigger a second researcher when one already running (replenishment triggers when count below threshold — why kick off another?). Theoretically okay with multiple but need to define when that might happen. Write requirement for max concurrent researchers setting; park "when second researcher triggers" as open/to-process or gather note for researcher — don't invent trigger without user saying.

<!-- answer-anchor: q-022 -->

---

## q-023

**Question:** After the user submits an answer, until the answer processor finishes and the question file is removed from the queue folder, what should the UI do for that question?

**A)** Keep it visible but mark it pending / not re-submittable
**B)** Hide it immediately from the list (before the queue file is deleted)
**C)** Leave it fully interactive (allow another submit)
**D)** Other (describe briefly)

**Recommend:** A — prevents double submit while still matching file-based truth once processed.
Which do you want?

**Answer:** A — after submit, keep question visible but mark pending / not re-submittable until processor finishes and queue file removed (unless modified per q-018).

<!-- answer-anchor: q-023 -->

---

## q-024

**Question:** While an interview session is archived, must the UI refuse answer submission and replenishment for that session?

**A)** Yes — no answer submit or replenishment while archived
**B)** No — archive only affects listing; agent actions still allowed

**Recommend:** A — archive should mean the session is inactive for agent work.
Which do you want?

**Answer:** A — while archived, refuse answer submission and replenishment; archived means not mutatable / inactive for agent work.

<!-- answer-anchor: q-024 -->

---

## user-note — question actions (volunteered)

**Note:** For a given question, the user can choose additional actions beyond answering. Already covered: branch to separate agent session. Also: reject the question, request more options, defer/revisit later. User can choose actions about the question.

<!-- answer-anchor: user-note-question-actions -->

---

## q-025

**Question:** For interview-specific constraints (beyond inheriting tod's environmental requirements), a few categories still look worth considering:

1. On-disk interview protocol — queue, config, and transcript formats stay compatible with researcher and answer-processor agents
2. UI write authority — which session files the UI may modify directly vs only through agent runs
3. Session durability — in-progress interview sessions survive tod restarts without silent loss
4. Branch session boundaries — deep-dive branches must not corrupt the parent interview's session files

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B — at least #1 and #4 look binding for a file-based agent UI.
Which do you want?

**Answer:** Pursue #3 session durability and #1 on-disk interview protocol. User notes #1 might be more design-stage — park protocol detail in to-process for design if too design-y; write constraint for session durability. For protocol: reference compatibility without over-specifying format in requirements phase if appropriate.

<!-- answer-anchor: q-025 -->

---

## q-026

**Question:** When a researcher replenishment run is already in progress for an interview and open question count is still below the replenishment threshold, should the UI start another researcher run (subject to the max-concurrent setting)?

**A)** No — wait for the in-flight run to finish before starting another
**B)** Yes — start another if still below threshold and under the max-concurrent limit
**C)** Only when the user manually requests another replenishment

**Recommend:** A — avoids duplicate work unless you have a concrete reason for overlap.
Which do you want?

**Answer:** Remove max-concurrent-researchers setting approach (forget that). Replace with two-threshold model: first threshold — when open count drops below it, kick off a researcher; second (lower) threshold — if count drops below that while first researcher running, kick off a second researcher; maximum 2 researchers at once (hard cap, not a user setting).

<!-- answer-anchor: q-026 -->

---

## q-027

**Question:** When an interview is complete (empty queue and no further questions expected), what is the minimum outcome the UI must provide?

**A)** Mark the session complete in the session list only — no other required action
**B)** Mark complete and show a clear finished state; user may manually open related docs (e.g. scope user.md) from there
**C)** Mark complete and automatically prompt or navigate toward the next process step

**Recommend:** B — captures completion without over-specifying workflow integration.
Which do you want?

**Answer:** B — mark complete and show clear finished state; user may manually open related docs. Note in to-process that completion follow-up will be enhanced when tod integration exists.

<!-- answer-anchor: q-027 -->

---

## q-028

**Question:** When an answer-processor run fails after the user submitted an answer, what must the UI allow?

**A)** Re-enable submit on that question (clear pending) and show the error
**B)** Show the error only — no automatic re-enable; user recovers outside the UI
**C)** Offer an explicit retry action that resubmits the same answer payload

**Recommend:** A — matches the pending-state model and lets the user fix and resubmit.
Which do you want?

**Answer:** A — re-enable submit and show error on answer-processor failure.

<!-- answer-anchor: q-028 -->

---

## q-029

**Question:** When a researcher replenishment run fails, what must the UI allow?

**A)** Automatic retry after a delay while count stays below threshold
**B)** Manual retry action visible to the user
**C)** Show the error only — replenishment retries on the next threshold check without extra user action

**Recommend:** B — user stays in control; threshold check alone may not retry a failed run promptly.
Which do you want?

**Answer:** Auto retry up to 3 times with exponential backoff, show error each time. After 3 failures, stop auto retry. User must have UI control to manually kick off question generation (researcher) as recovery when auto retry gives up.

<!-- answer-anchor: q-029 -->

---

## q-030

**Question:** Agent run feedback — The UI shows visible status for in-flight, success, and failure of researcher replenishment runs and answer-processor runs. Errors must be visible to the user.

Success criteria:
1. User can see when a researcher or answer-processor run is in progress, succeeded, or failed
2. Failed runs surface error information visibly (not silent)

Accept, Modify, or Reject this requirement wording (statement + success criteria)?

**Answer:** Modify — remove success criteria block from requirement 13 (redundant with statement). Accept modified wording without SC.

<!-- answer-anchor: q-030 -->

---

## q-031

**Question:** Submitted-question pending state — After the user submits an answer for a queued question, the UI keeps that question visible but marks it pending / not re-submittable (deactivated) until the answer processor finishes and the question file is removed from the queue folder.

Success criteria:
1. After submit, the question cannot be submitted again while its queue file still exists unchanged
2. If the queue file for that question is modified on disk after submit, the UI re-enables the question and updates its displayed content to match the modified file

Accept, Modify, or Reject this requirement wording (statement + success criteria)?

**Answer:** Modify — rewrite requirement 14 as single requirement statement without separate success criteria. Fix wording: pending until answer processor finishes AND queue file removed OR modified (modification re-enables with updated content). No redundancy between statement and SC.

<!-- answer-anchor: q-031 -->

---

## q-032

**Question:** When a deep-dive branch starts on a queued question, must the branch use a separate interview session (own config, transcript, and queue) from the parent interview?

**A)** Yes — separate session scaffolding for each branch
**B)** No — same session; branch is UI mode only on the parent session
**C)** Other (describe briefly)

**Recommend:** A — matches separate branch processing and automatic parent closure in your earlier answer.
Which do you want?

**Answer:** A variant — separate session for deep-dive branch BUT it is NOT an interview session. Different kind of session — simpler, no queue. Update requirement 7 and related branch requirements accordingly.

<!-- answer-anchor: q-032 -->

---

## q-033

**Question:** The two-threshold researcher model starts a second run when open count drops below a lower threshold while the first is in flight. What should the default value for that second (lower) threshold be?

**Answer:** Default second (lower) replenishment threshold = 2. Update requirement 17 and/or settings language accordingly (second threshold configurable? user only said default 2 — if first threshold is configurable in settings, second might need same or default 2 documented).

<!-- answer-anchor: q-033 -->

---

## q-034

**Question:** When the user rejects a queued question, what should the UI do?

**Answer:** Do NOT define reject-question behavior details at requirements stage — defer to design. Keep requirement 18 that fixed question actions exist; park reject behavior in to-process for design.

<!-- answer-anchor: q-034 -->

---

## q-035

**Question:** When the user requests more options on a queued question, what should the UI do?

**Answer:** Same — defer request-more-options behavior to design; park in to-process.

<!-- answer-anchor: q-035 -->

---

## q-036

**Question:** When the user defers a queued question to revisit later, what should the UI do?

**Answer:** Same — defer defer/revisit behavior to design; park in to-process.

<!-- answer-anchor: q-036 -->

---

## q-037

**Question:** A deep-dive branch uses a separate non-interview session (no question queue). What must that branch session provide at minimum — e.g. its own transcript, config, or something else?

**Answer:** Defer branch session minimum artifacts (transcript, config, etc.) to design phase; park in to-process. Do not invent branch session structure in user.md.

<!-- answer-anchor: q-037 -->

---

## q-038

**Question:** For interview-specific constraints (beyond inheriting tod's environmental requirements), a few categories still look worth considering:

1. UI write authority — which session files the UI may modify directly vs only through agent runs
2. Branch session boundaries — deep-dive branches must not corrupt the parent interview's session files

Want to pursue any of these (or a close variant)?

**A)** No — skip these for now
**B)** Yes — which numbers (or rename / add a close variant)

**Recommend:** B — both look binding for a file-based agent UI.
Which do you want?

**Answer:** #1 UI write authority: User says UI never touches .local/agent (given, no rule needed). No negative rules. Positive: UI persists interview sessions themselves — that's what UI needs to write. Write constraint or requirement if measurable: UI persists interview session state/metadata (not agent scratchpad files). Do NOT add "must not write .local/agent" negative constraint. #2 Branch session boundaries: User doesn't understand "branch session boundaries" question — do not write constraint from this. Note in journal/to-process; researcher may clarify later with plain language or drop.

<!-- answer-anchor: q-038 -->

---

## q-039

**Question:** How should the UI determine that an interview is complete — i.e. that the queue is empty and no further questions are expected?

**Answer:** Interview complete when: queue is empty AND the most recent generate-questions request to the researcher returned no further questions. Write this into requirement 15 (update completion detection).

<!-- answer-anchor: q-039 -->

---

## q-040

**Question:** Error recovery — When a researcher or answer-processor run fails, the UI surfaces the failure and supports user recovery without leaving the failure invisible or silently dropped. On answer-processor failure after submit, the UI re-enables submit on that question and shows the error. On researcher replenishment failure, the UI auto-retries up to three times with exponential backoff and shows the error each time; after three failures it stops auto-retry and the user must have UI control to manually kick off question generation (researcher) as recovery.

Accept, Modify, or Reject this requirement wording?

**Answer:** Accept requirement 16 error recovery as written.

<!-- answer-anchor: q-040 -->

---

## q-041

**Question:** Deep-dive branch closure — If the user needs to discuss a particular question separately, the branch runs in a separate non-interview session (simpler session model, no question queue). At some point in that branch process an answer is found; it submits to the answer processor agent. The UI handles bringing the answer back and submitting it as the answer to the parent question — automatic, no copy-paste by the user.

Accept, Modify, or Reject this requirement wording?

**Answer:** Accept requirement 2 deep-dive branch closure as written.

<!-- answer-anchor: q-041 -->

---

## q-042

**Question:** Deep-dive branch start — The user can start a deep-dive branch from a specific queued question. The branch uses a separate session that is not an interview session (simpler session model, no question queue).

Accept, Modify, or Reject this requirement wording?

**Answer:** Accept requirement 7 deep-dive branch start as written.

<!-- answer-anchor: q-042 -->

---

## q-043

**Question:** Interview completion — The interview is complete when the question queue is empty and the most recent generate-questions request to the researcher returned no further questions. When complete, the UI marks the interview as complete and shows a clear finished state; the user may manually open related docs (e.g. scope user.md) from there.

Accept, Modify, or Reject this requirement wording?

**Answer:** Accept requirement 15 interview completion as written.

<!-- answer-anchor: q-043 -->

---

## q-044

**Question:** Researcher concurrency — When open question count drops below the replenishment threshold, the UI starts a researcher run. If count drops below a second (lower) threshold (default 2, configurable in application settings) while a researcher run is already in progress, the UI starts a second researcher run. Maximum two researcher runs in flight at once (hard cap, not a user setting).

Success criteria:
1. Second threshold defaults to 2 and is configurable in application settings

Accept, Modify, or Reject this requirement wording (statement + success criteria)?

**Answer:** Accept requirement 17 researcher concurrency and success criteria as written.

<!-- answer-anchor: q-044 -->

---

## q-045

**Question:** Question actions — For a given queued question, the user can choose actions about the question beyond submitting an answer, including: start a deep-dive branch (separate session), reject the question, request more options, and defer/revisit later. Per-action behavior is deferred to design (requirements establish that these actions exist, not how each behaves).

Accept, Modify, or Reject this requirement wording?

**Answer:** Accept requirement 18 question actions as written.

<!-- answer-anchor: q-045 -->

---

## q-046

**Question:** Session durability — In-progress interview sessions survive tod restarts without silent loss of session state.

Accept, Modify, or Reject this constraint wording?

**Answer:** Accept constraint 5 session durability as written.

<!-- answer-anchor: q-046 -->

---

## q-047

**Question:** I've reviewed the current constraints for this phase. These top-level areas look complete and coherent for a reasonable application of this kind — I don't see a compelling gap to propose next:

1. Embedded inside the tod GPUI desktop app
2. Environmental requirements inherit from tod (no duplication)
3. Primary operator — single local user / software engineer
4. Self-enclosed project phase — session list, launch, and archive only (no tod task-list integration yet)
5. Session durability — survive tod restarts without silent loss
6. On-disk interview protocol compatibility with researcher and answer-processor agents
7. Interview session persistence — UI persists session state and metadata

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** A — constraints complete for this phase.

<!-- answer-anchor: q-047 -->

---

## q-048

**Question:** I've reviewed the current requirements for this phase. These top-level areas look complete and coherent for a reasonable application of this kind — I don't see a compelling gap to propose next:

1. Question queue replenishment (configurable threshold)
2. Deep-dive branch (start, separate non-interview session, closure back to parent)
3. Queue folder sync (filesystem watch, sub-second)
4. Efficient question interaction (navigation, options, in-place wording edit)
5. Answer submission payload (question id + answer text)
6. Agent session strategy (v1 fresh sessions, measure later)
7. Session management (multiple interviews, list, launch, archive)
8. Agent run feedback and error recovery
9. Interview completion detection and finished state
10. Question actions (existence; per-action behavior deferred to design)

Did we miss anything important?

**A)** No — this is enough for now
**B)** Yes — name what we missed

**Recommend:** A unless you see a real gap.
Which do you want?

**Answer:** A — requirements complete for this phase.

<!-- answer-anchor: q-048 -->

---
