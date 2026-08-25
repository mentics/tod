# Interview UI

## Goal

Make it as efficient as possible to create the documentation necessary to proceed to the next state in a process. Secondary goal: with the minimum necessary effort from the user — the whole thing leverages agents and their capabilities as much as possible.

## Requirements

### Question queue & sync

1. Question queue replenishment — When the open question count drops below the replenishment threshold (default 8), the UI automatically starts a researcher run to request more questions. The replenishment threshold is configurable in application settings (not hardcoded preferences).
   - Success criteria:
     - Replenishment runs when open question count falls below the configured threshold (default 8)
     - User can change the replenishment threshold in application settings

2. Queue folder sync — The UI watches the interview question queue folder via OS-level filesystem events and automatically reflects added, removed, or changed questions within one second.

3. Researcher concurrency — When open question count drops below the replenishment threshold (default 8, configurable in application settings), the UI starts a researcher run. If count drops below a second (lower) threshold (default 2, configurable in application settings) while a researcher run is already in progress, the UI starts a second researcher run. Maximum two researcher runs in flight at once.

### Question interaction & answers

4. Efficient question interaction — It is extremely efficient to navigate the questions, choose existing options in the questions when there are multiple choices, and also edit proposed statements directly. For example, if the question is "Approve this statement" or something like that, the user should be able to edit that statement and then submit it back as an approved statement with those changes.
   - Success criteria:
     - From a wording-approval style question, the user can edit the proposed statement text in place and submit that edited text as the answer without copying it elsewhere

5. Answer submission payload — When the user submits an answer for one queued question, the UI passes the question id and user answer text only to the answer processor.

6. Multiple-choice keys and submit — Multiple-choice options are labeled and keyed **1, 2, 3, …** only (not A, B, C). Display uses numbers. No letter-key MC support (no dual-accept, no position mapping from letters). Researcher and any agent-authored MC must emit digit keys via updated prompts/structure/protocol. When the user is not editing Notes, pressing the digit key for an option **immediately submits** that option as the answer for the current question (same submit path as Submit). Digit keys do not merely highlight/select without submitting. (Supersedes select-only letter-key MC behavior; complements workspace Space/Enter on focused MC and click-to-submit.)

7. Submitted-question pending state — After the user submits an answer for a queued question, the UI keeps that question visible but marks it pending / not re-submittable (deactivated) until the answer processor finishes and the question file is removed from the queue folder or modified on disk; if modified after submit, the UI re-enables the question and updates its displayed content to match the modified file.

8. Question actions — For a given queued question, the user can choose actions about the question beyond submitting an answer, including: start a deep-dive branch (separate session), consider/reconsider the question, request more options, and defer/revisit later. Per-action behavior is specified in design (requirements establish that these actions exist).

### Deep-dive branch

9. Deep-dive branch start — The user can start a deep-dive branch from a specific queued question. The branch uses a separate session that is not an interview session (simpler session model, no question queue).

10. Deep-dive branch closure — The deep-dive runs in a separate non-interview session. The UI does not auto-detect an answer or auto-submit to the parent question. The user can take text from the deep-dive (e.g. via a “Use this” control) into the parent question’s answer field, edit if needed, and submit the parent question normally.

### Agent orchestration

11. Agent session strategy (v1) — v1 uses a fresh researcher session on each replenishment and a fresh answer-processor session per answer submission; session strategy for researcher and answer-processor may be revisited after measurement.

12. Agent run feedback — The UI shows visible status for in-flight, success, and failure of researcher replenishment runs and answer-processor runs. Errors must be visible to the user.

13. Error recovery — When a researcher or answer-processor run fails, the UI surfaces the failure and supports user recovery without leaving the failure invisible or silently dropped. On answer-processor failure after submit, the UI re-enables submit on that question and shows the error. On researcher replenishment failure, the UI auto-retries up to three times with exponential backoff and shows the error each time; after three failures it stops auto-retry and the user must have UI control to manually kick off question generation (researcher) as recovery.

### Session management

14. Multiple simultaneous interviews — The UI supports multiple interview sessions at the same time.

15. Interview session list and launch — The UI provides a list of interview sessions and the ability to launch a new interview from that list view.

16. Interview session archive — The UI provides an archive of interview sessions. Archived sessions remain reopenable from the archive view with session files preserved on disk. While a session is archived, the UI refuses answer submission and replenishment for that session (archived means inactive / not mutatable for agent work).

17. New interview session creation — When the user launches a new interview from the session list, the user provides context for what the interview is for (e.g. new project, new task, existing project/task). The UI submits the appropriate command and context to the researcher agent (question generator). The researcher agent creates interview session scaffolding (config, transcript, queue, etc.) as part of that first request. The UI does not create session scaffolding separately.

18. Interview completion — The interview is complete when the question queue is empty and the most recent generate-questions request to the researcher returned no further questions. When complete, the UI marks the interview as complete and shows a clear finished state; the user may manually open related docs (e.g. scope user.md) from there. **Complete only when queue empty:** the workspace shows the Complete / finished state only when there are **no open questions** in the bound queue (and replenishment is not in flight / not still expected per the rules above). SQLite `complete` status must not keep showing Complete when open questions are present; if the queue has open questions again, the workspace must show question body UI and allow answering (must not stick Complete over a non-empty queue).

### Selection, layout & workspace navigation

19. Selection visibility — Selected rows in the interview session list and the open-question list (and keyboard focus chrome on those rows / response controls) must be clearly distinguishable from the background. Color alone is insufficient when contrast is weak. Align with `refs/process/other/ux-design.md` (**Color and contrast**, **States and feedback**, **Actionable lists**): strong contrast for essential UI; design the full interactive state set; interactive list affordances must not rely on color alone.

20. Single-line question list rows — Open-question list rows are a **single line**: question `id` and short label on one line (ellipsis truncation of the combined line is OK when needed). Do **not** stack id above label on separate lines. (Promotes Accept/`preview.png` intent from `artifacts/visual/interview-workspace/`.)

21. Workspace keyboard and pointer navigation — In the interview workspace, the user can drive question answering with keyboard and mouse as follows:

   **Focus / keyboard**
   1. With focus on the open-question list, **Right arrow** moves focus to the uppermost interactive control in the response column (top MC option when the question has MC options; otherwise the first interactive response control).
   2. With focus in the response column, **Up/Down** move among that column’s interactive controls; **Left arrow** returns focus to the question list. The middle question-body column has nothing interactive and is **not** a keyboard focus stop (Left ↔ Right only).
   3. The currently focused control (and the selected question when list-focused) is **visibly highlighted** so focus location is obvious.
   4. When an MC option is focused, **Space** or **Enter** submits that option as the answer for the current question (same submit path as Submit).
   5. Digit-key MC immediate submit (labels **1, 2, 3…**) is specified under requirement 6 — this workspace requirement does **not** keep letter-key select-without-submit.

   **Notes edit mode**
   6. The Notes text area uses an explicit edit mode for keyboard focus: moving focus onto the textbox alone does not edit; **Enter** or **Space** enters edit mode; **Escape** exits edit mode (does **not** leave the workspace — see requirement 22); while editing, arrow keys navigate text as in a normal editor (column Left/Right and response Up/Down do not apply until edit mode is exited). Digit keys do not submit MC while Notes are in edit mode.
   7. **While editing Notes, Ctrl+Enter submits** the current answer immediately (same submit path as Submit). This is the single Ctrl+Enter rule for Notes.

   **Mouse**
   8. **Clicking an MC option immediately submits** that option as the answer (same submit path as Submit); it does not merely select without submitting.
   9. **Clicking the Notes text box enters edit mode immediately** so the user can type right away (mouse path does not require a separate Enter/Space to start editing).

22. Workspace Escape — From the interview workspace, **Escape does not navigate to the session list**. Escape exits Notes edit mode when Notes are being edited (see requirement 21). When Notes are not in edit mode, Escape does not leave the workspace (no-op unless a nested workspace chrome dismisses, if any). Leaving the workspace uses an explicit control (e.g. **Back to interviews**), not Escape.

23. Response column layout — In the interview workspace response column, multiple-choice option labels are shown in full: they **must not truncate with ellipsis**; long labels **wrap** onto additional lines. The response column (including MC options and the Notes field) **fills available horizontal space** in the workspace and **grows as the window widens** — it must not use a fixed maximum width that clips content. Horizontal fill is required; vertical fill of the window is not required.

## Constraints

1. Runs embedded inside the tod GPUI desktop app (not standalone or web).

2. Platform/runtime, security/privacy, compatibility, and resource bounds inherit from the tod project. Interview UI is views/functionality encapsulated inside the tod desktop app; running inside tod inherits all environmental requirements the tod project already establishes (see `doc/process/projects/tod/user.md`). Do not duplicate tod constraints in this project.

3. Primary operator — The sole intended user is the single local user on the machine, typically a software engineer.

4. Self-enclosed project phase — For this initial project phase, Interview UI is self-enclosed inside the tod application: session list, launch new interview from that list, and archive — do not define integration from the rest of the tod application (task-list → launch interview) yet.

5. Session durability — In-progress interview sessions survive tod restarts without silent loss of session state.

6. On-disk interview protocol compatibility — Queue, config, and transcript on-disk formats remain compatible with researcher and answer-processor agents.

7. Interview session persistence — The UI persists interview session state and metadata for interview sessions it manages.
