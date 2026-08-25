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

6. Keyboard multiple-choice selection — When a question is multiple-choice, the user can efficiently select one of the options using the keyboard. The UI must support this.

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

18. Interview completion — The interview is complete when the question queue is empty and the most recent generate-questions request to the researcher returned no further questions. When complete, the UI marks the interview as complete and shows a clear finished state; the user may manually open related docs (e.g. scope user.md) from there.

## Constraints

1. Runs embedded inside the tod GPUI desktop app (not standalone or web).

2. Platform/runtime, security/privacy, compatibility, and resource bounds inherit from the tod project. Interview UI is views/functionality encapsulated inside the tod desktop app; running inside tod inherits all environmental requirements the tod project already establishes (see `doc/process/projects/tod/user.md`). Do not duplicate tod constraints in this project.

3. Primary operator — The sole intended user is the single local user on the machine, typically a software engineer.

4. Self-enclosed project phase — For this initial project phase, Interview UI is self-enclosed inside the tod application: session list, launch new interview from that list, and archive — do not define integration from the rest of the tod application (task-list → launch interview) yet.

5. Session durability — In-progress interview sessions survive tod restarts without silent loss of session state.

6. On-disk interview protocol compatibility — Queue, config, and transcript on-disk formats remain compatible with researcher and answer-processor agents.

7. Interview session persistence — The UI persists interview session state and metadata for interview sessions it manages.
