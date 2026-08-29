# Task CRUD

Project: `doc/process/projects/tod/`

## Goal

Let users create, edit, and permanently delete tasks from the main task list: inline create compose, edit slide-over covering the agent pane, project association, field editing (including tags and repo/branch), and confirm-before-delete — without leaving the situational main chrome for routine create.

## Requirements

1. Inline create compose — New task opens an inline compose row at the top of the task list (same density as a normal row), not a separate full-screen create page and not a heavy modal.
   - Success criteria:
     - Activating New task (or its shortcut) shows a compose row in the task list pane
     - Agent pane remains visible during compose

2. Compose fields — Compose includes project selector and title; Create and Create & edit controls with shortcut badges on those controls.
   - Success criteria:
     - User can choose a project (or no project) and enter a title before create
     - Create and Create & edit are visible on the compose row with on-control shortcut badges

3. Create selection — While composing, list selection is on the compose row only (no other task row stays selected).
   - Success criteria:
     - With compose open, only the compose row shows selection chrome

4. Enter creates — Enter (or Create) creates the task immediately in `proposed`, persists title (and project), dismisses compose, and selects the new row in the list. When the trimmed title matches the ticket-id pattern (e.g. `TOD-142`), Enter runs **from ticket** import instead: call the configured issue tracker (Linear by default), fetch issue fields, create or select the task (task-list req 32).
   - Success criteria:
     - After Create/Enter with a non-empty title that is not a ticket id, a new proposed task exists and is selected in the list
     - After Create/Enter with a ticket id, tod imports from the issue tracker or selects an existing task for that ticket
     - Empty title does not create

5. Ctrl+Enter creates and edits — Ctrl+Enter (or Create & edit) creates first (title persisted), then opens the edit slide-over for that task.
   - Success criteria:
     - After Create & edit, the task exists before the edit panel shows, and edit shows the persisted title

6. Esc cancels compose — Esc cancels compose with no task created.
   - Success criteria:
     - Esc with compose open leaves no new task

7. Edit slide-over — Edit covers the agent panel; the task list stays visible on the left. Esc/Close restores the agent panel.
   - Success criteria:
     - Opening edit hides the agent fleet pane under the edit panel while the task list remains
     - Closing edit shows the agent pane again

8. Open edit — Edit slide-over opens via **Create & edit** after create (req 5) or via task-list edit control (**E** / title on selected row). Row activation (Enter / row chrome) runs lifecycle next; it does not open edit.
   - Success criteria:
     - **E** or activating title on the selected row opens edit for that task
     - Row activation (Enter or non-actionable row chrome) does not open the edit slide-over
     - **Create & edit** still opens edit for the newly created task
     - The task list remains visible while edit covers the agent pane

9. Edit fields — Edit supports: title; Linear issue and GitHub PR (link or short value); slug; tags via chip input; repository root (filesystem path) and branch; notes (taller multi-line). No lifecycle control in edit. No worktree field on the task.
   - Success criteria:
     - User can edit each listed field in the slide-over
     - Lifecycle is not editable in the slide-over
     - Worktree is not a task edit field

10. Single Linear and PR links — On this edit surface, Linear issue and GitHub PR each accept at most one value (link or short id). Multi-issue / multi-PR association is out of scope for this task’s edit UI.

11. Title uniqueness — Create and edit must not leave two tasks with the same title (case-insensitive). When a create or edit would collide, tod blocks the change and shows a visible error (toast or banner).
   - Success criteria:
     - A colliding create/edit does not persist the colliding title
     - The user sees a toast or banner for the failure

12. Slug uniqueness — Create and edit must not leave two tasks with the same slug. When a create or edit would collide, tod blocks the change and shows a visible error (toast or banner).

13. Slug required — A task always has a non-empty slug. Clearing or blanking the slug in edit is blocked; tod shows a visible error (toast or banner) and keeps the previous slug.
    - Success criteria:
      - An edit that would leave slug empty does not persist
      - The user sees a toast or banner for the failure

14. Slug auto-update from ticket — When the linked issue ticket id is added or changed in edit, tod auto-updates the slug from the title and ticket id unless the user has manually changed the slug on that task.
    - Success criteria:
      - Adding or changing Linear ticket id updates the slug when the user has not manually edited slug
      - After a manual slug edit, changing ticket id leaves the slug unchanged

15. Tags chip input — Tags use a standard chip input: chips + caret in one control; Enter/comma commits; Backspace on empty removes last; × removes a chip.
   - Success criteria:
     - User can add a tag with Enter or comma and remove a tag with × or Backspace-on-empty

16. Project association — Tasks may belong to an optional project/namespace. Create uses a reusable project selector (pick, no project, or create project inline in the dropdown). Edit does not treat project as a primary day-to-day field; **Move to project** is a bottom action (uncommon migration; may revalidate constraints).
    - Success criteria:
      - User can assign a project at create, including creating a new project name inline
      - User can move a task to another project from edit via Move to project
      - A task can exist with no project

17. Bottom edit actions — **Move to project** and **Delete task** are real buttons with shortcut badges (no ellipsis in labels).
    - Success criteria:
      - Both actions appear as buttons with on-control shortcut badges
      - Labels do not use trailing ellipses

18. Permanent delete — Permanent delete requires confirmation; blocked while associated agents remain (per project requirements).
    - Success criteria:
      - Delete requires an explicit confirm step before the task is removed
      - Delete is blocked when the task still has associated agents

19. Delete confirm toast — Permanent delete uses the shared non-autohide confirmation toast with **No** and **Yes** (not a separate modal). **No** dismisses and cancels; **Yes** proceeds with delete when allowed.
    - Success criteria:
      - Activating Delete task shows a non-autohide confirm toast with No and Yes
      - No cancels without deleting
      - Yes deletes only when the task has no associated agents

20. Blocked delete affordance — While the task still has associated agents, **Delete task** does not open the confirm toast. The control is disabled (or otherwise non-activating), and the blocked reason is visible on or next to the control.
    - Success criteria:
      - With associated agents present, activating Delete task does not show the confirm toast and does not delete
      - The user can see why delete is blocked without completing a confirm step

21. Persistence of edits — Ordinary field mutations persist without an explicit Save (per project fleet persistence).
    - Success criteria:
      - Changing a field in edit and closing the panel leaves the change after reopen/relaunch when persistence is available

22. Post-delete selection — After **Yes** on delete confirm removes the task, edit closes and the list selects the next task below the deleted row, or the previous row if none below; if the list is empty, nothing is selected.

23. Title required — A task always has a non-empty title. Clearing or blanking the title in edit is blocked; tod shows a visible error (toast or banner) and keeps the previous title.

24. Edit follows list selection — While the edit slide-over is open, selecting a different task in the list retargets the edit slide-over to that task.

25. Delete confirm identifies task — The permanent-delete confirmation toast includes the title of the task being deleted.

26. Compose cancel restores selection — When Esc cancels compose with no task created, list selection returns to the task that was selected before compose opened; if none was selected, none is selected.

27. Tags stay unique — Committing a tag that is already on the task does not add another chip; the existing chip remains.

28. New task closes edit — While the edit slide-over is open, activating **New task** closes edit, then opens inline compose.

29. Compose project default — When **New task** opens compose, the project selector starts on the project from the last successful create (including create-and-edit). If there has been no successful create yet, or the last successful create had no project, it starts on no project.

30. Whitespace-only title is empty — A title that is only whitespace (spaces/tabs) is treated as empty: create does not create; edit blocks the change, shows a visible error (toast or banner), and keeps the previous title.

31. Create slug collision auto-suffix — On create, when the auto-generated slug already belongs to another task, tod auto-suffixes the slug until unique, then creates.
    - Success criteria:
      - Create whose first generated slug collides still creates a task with a unique slug
      - Create is not blocked solely because the first generated slug was taken

32. Move to project confirm — After the user picks a destination (or no project) via **Move to project**, tod shows a non-autohide confirmation toast with **No** and **Yes** before changing the association. The toast identifies the destination project name, or that the task will have no project. **No** dismisses and cancels; **Yes** applies the move.

33. Tag identity is case-insensitive — Under **Tags stay unique**, tag identity ignores letter case: committing a tag that matches an existing chip except for case does not add another chip.

34. Empty tag draft is a no-op — On the edit tags chip input, pressing Enter or comma while the draft text is empty leaves the chips unchanged and shows no error.

35. Blocked create keeps compose — When create from compose is blocked (for example **Title uniqueness**), compose stays open with the draft title and project, tod shows a visible error (toast or banner), and keyboard focus returns to the compose title input.

36. Compose yields to row activation — While inline compose is open, activating an existing task row cancels compose with no task created, then runs that row’s lifecycle next action (task-list). Activating the selected row’s title or **E** opens edit instead (req 17).

37. Esc cancels delete confirm — While the permanent-delete confirmation toast is open, Esc dismisses the toast and cancels delete (same as **No**).

38. Blocked edit restores field focus — When an edit is blocked (for example **Title uniqueness** or **Slug required**), after the visible error, keyboard focus returns to the field that failed the edit.

39. Whitespace-only slug is empty — A slug that is only whitespace (spaces/tabs) is treated as empty under **Slug required**: edit blocks the change, shows a visible error (toast or banner), and keeps the previous slug.

40. Esc cancels Move confirm — While the Move to project confirmation toast is open, Esc dismisses the toast and cancels the move (same as **No**).

41. Whitespace-only tag draft is a no-op — On the edit tags chip input, pressing Enter or comma while the draft text is only whitespace leaves the chips unchanged and shows no error.

42. Tag commit trims whitespace — When a tag is committed from the chip input, leading and trailing whitespace are removed before the chip is added or matched for uniqueness.

43. Slug uniqueness is case-insensitive — Create and edit must not leave two tasks with slugs that differ only by letter case. A colliding create/edit is blocked with a visible error (toast or banner).

44. Title trims whitespace — On create and edit, leading and trailing whitespace are removed from the title before persist and uniqueness checks. A title that is empty after trim is treated as empty under **Enter creates** / **Title required**.

45. Tag case match keeps existing chip — When a committed tag matches an existing chip except for letter case, tod keeps the existing chip text and does not replace it with the new spelling.

46. Slug trims whitespace — On edit, leading and trailing whitespace are removed from the slug before persist and uniqueness checks. A slug that is empty after trim is treated as empty under **Slug required**.

47. Slug auto-update on ticket clear — When the Linear ticket id is cleared in edit and the user has not manually changed the slug on that task, tod auto-updates the slug from the title alone (no ticket id).

48. Move to project excludes current — The list of choices for **Move to project** does not include the project the task is already in.

49. Compose opens on title — When **New task** opens inline compose, keyboard focus moves to the compose title input.

50. Edit opens on title — When the edit slide-over opens, keyboard focus moves to the title field.

51. Focus after create — After successful Create or Enter from compose, keyboard focus moves to the newly created task row in the list.

52. Edit retarget keeps list focus — When selecting a different task in the list retargets an already-open edit slide-over, keyboard focus remains on the task list (does not jump into the edit fields).

53. Focus after delete — After Yes on delete confirm removes the task, keyboard focus moves to the newly selected task row when one exists; if the list is empty, focus remains in the task list pane with no row selected.

54. Slug auto-update on title change — When the title changes in edit and the user has not manually changed the slug on that task, tod auto-updates the slug from the title and linked issue ticket id (when present).

55. Repo path may be missing — Edit allows any repository root path string the user enters (including a path that does not currently exist on disk). tod does not block saving the field solely because the path is missing.

56. Linear/PR trim whitespace — On edit, leading and trailing whitespace are removed from Linear issue and GitHub PR values before persist. A value that is empty after trim clears that field.

57. Move confirm identifies task — The Move to project confirmation toast includes the title of the task being moved, in addition to the destination project name or that the task will have no project.

58. Manual slug latch — A task counts as having a manually changed slug once the user successfully persists a slug edit on that task (including a persist that leaves the same spelling). Auto-generated slug updates from title or ticket id do not count as manual changes.

59. Focus after compose cancel — After Esc cancels compose with no task created, keyboard focus moves to the restored selected task row when one exists; if none is selected, focus remains in the task list pane with no row selected.

60. Open linked issue/PR — When Linear issue or GitHub PR has a value in edit, the user can open that link in a browser from the edit slide-over. If the stored value is a URL, tod opens it; if it is a short id/number, tod resolves it to a URL using the configured issue-tracker and PR URL templates and opens that. If resolution fails, tod shows a visible error (toast or banner) and does not open a browser location.

61. Permanent delete is final — After Yes on delete confirm removes the task, the deletion is final in this phase — no undo toast and no trash/recovery path for restored tasks.

62. Create selection respects filter — After successful Create or Enter, compose dismisses and the new task is selected only when it matches the active tag filter and fuzzy search (if any). When it does not match, list selection stays unchanged.

63. Compose/edit ownership — Where task-list requirements describe inline New task draft behavior that conflicts with this task’s compose or edit requirements, this task’s requirements win for create and edit UI; task-list should be aligned to match.

64. Compose visible under filter — While inline compose is open, the compose row remains visible even when the active tag filter and/or fuzzy search would otherwise hide it.

65. Ticket import fetch failure — When Create/Enter runs **from ticket** import and the issue tracker fetch fails, compose stays open, tod shows a visible error (toast or banner), and keyboard focus returns to the compose title input.

66. Ticket import duplicate — When Create/Enter runs **from ticket** import and the ticket already has a task, tod does not duplicate; compose dismisses and the list selects the existing task when visible under current filter/search.

67. Edit matches visible selection — The edit slide-over may show a task only while that task is selected and visible in the task list. When a list change (including filter/search change) would hide the task being edited, edit closes unless list selection has moved to another visible task row — then edit retargets to that row.

68. Post-delete selection respects filter — After Yes on delete confirm removes the task, edit closes and list selection moves to the nearest remaining row that matches the active tag filter and fuzzy search (if any). If none are visible, nothing is selected.

69. New task re-activation — While inline compose is already open, activating New task again moves keyboard focus to the existing compose title input and does not open a second compose row.

70. Compose cancel respects filter — After Esc cancels compose with no task created, list selection returns to the previously selected task only when that task matches the active tag filter and fuzzy search (if any). When it does not match, selection moves to a nearest visible row; if none are visible, nothing is selected.

71. Create & edit respects filter — After successful Create & edit, when the new task does not match the active tag filter and/or fuzzy search (if any), list selection stays unchanged and the edit slide-over does not open.

72. Focus after compose cancel respects filter — After Esc cancels compose with no task created, keyboard focus moves to the post-cancel selected task row when one exists; when nothing is selected, focus remains in the task list pane with no row selected.

73. Tag count bound — Edit tags chip input enforces the project maximum of 10 tags per task.

74. Focus after post-delete respects filter — After Yes on delete confirm removes the task, keyboard focus moves to the newly selected visible task row when one exists; when nothing is selected, focus remains in the task list pane with no row selected.

75. Tag draft discarded on edit close — Closing the edit slide-over (Esc or Close) discards any uncommitted tag draft text without adding a chip and without error.

76. Confirm toast wraps long title — Confirmation toasts that include a task title (permanent delete and Move to project) word-wrap the title to at most two lines; the toast may have variable height up to that limit. When the title would exceed two wrapped lines, the second line ends with ellipsis.

77. Title length bound — A task title may be at most 120 characters. On create or edit that would exceed that limit, tod blocks the change and shows a visible error (toast or banner). On edit, the previous title is kept; on create from compose, compose stays open with the draft title and project.

78. Focus after edit close — When the user closes the edit slide-over (Esc or Close), keyboard focus moves to the selected task row in the task list when one is selected; when none is selected, focus remains in the task list pane with no row selected.

79. Esc closes edit from notes — Pressing Esc while focus is in the notes field closes the edit slide-over and restores the agent pane (same as Esc from other edit fields).

80. Close dismisses confirm first — While a delete or Move to project confirmation toast is open, activating **Close** on the edit slide-over dismisses the toast and cancels that action without closing edit.

81. New task dismisses confirm — Activating **New task** while a delete or Move to project confirmation toast is open dismisses the toast (cancelling that action), closes edit, then opens inline compose.

83. Focus after create respects filter — After successful Create or Enter, when the new task does not match the active tag filter and/or fuzzy search (if any), list selection stays unchanged and keyboard focus moves to the task list pane (not a hidden row).

84. One confirm at a time — Activating **Move to project** or **Delete task** while the other action's confirmation toast is open dismisses the open toast (cancelling that pending action) and starts the newly chosen action's flow.

85. Row activation during edit — While the edit slide-over is open, Enter and activating non-actionable row chrome on the selected row still run lifecycle next; edit stays open.

86. Re-open edit refocuses title — While the edit slide-over is already open for the selected task, activating **E** or the title edit affordance moves keyboard focus to the edit title field; edit stays open on the same task.

87. Tag draft discarded on edit retarget — When list selection retargets an already-open edit slide-over, any uncommitted tag draft text in the chip input is discarded without adding a chip and without error.

88. Move picker cancel — Dismissing the Move to project selector without choosing a destination cancels the move flow: no confirmation toast appears and project association stays unchanged.

89. Slug length bound — A task slug may be at most 120 characters. On create or edit that would exceed that limit, tod blocks the change and shows a visible error (toast or banner). On edit, the previous slug is kept; on create from compose, compose stays open with the draft title and project.

90. Inline project duplicate — When inline project create in ProjectSelector is given a name that matches an existing project (case-insensitive), tod selects that existing project instead of creating a duplicate namespace.

91. Manual slug latch persists — After a task counts as having a manually changed slug, title and ticket id changes do not resume auto-generated slug updates unless the user explicitly resets slug to auto-update (for example a control that clears the manual latch and regenerates from current title and ticket id).

92. Tag paste commits — Pasting text into the edit tags chip input splits on commas and newlines, trims each segment, and commits each non-empty segment as a separate tag (subject to **Tags stay unique**, **Tag identity is case-insensitive**, **Tag count bound**, and empty/whitespace no-op rules). Segments that fail validation are skipped without blocking the rest.

93. Repo clear keeps branch — When the user clears the repository root field in edit, the branch value on that task stays unchanged (repository root and branch are independent fields).

94. Compose outside dismiss — While inline compose is open, activating main chrome outside the compose row (not Esc, not a task row activate) cancels compose with no task created (same as Esc).

95. Branch without repo allowed — Edit may persist a branch value while the repository root field is empty. tod does not block saving branch solely because repository root is empty.

96. Move picker Esc vs Close — While the Move to project selector is open, Esc dismisses the selector without a destination choice and edit stays open. Activating **Close** on the edit slide-over dismisses the selector without a destination choice and closes edit.

97. Linear/PR freeform persist — Edit persists any non-empty Linear issue or GitHub PR string the user enters (after trim). tod does not block saving solely because the value is not a URL or recognizable short id; **Open linked issue/PR** shows a visible error when resolution fails.

98. Edit ignores outside pointer — While the edit slide-over is open, activating main chrome outside the edit panel (not Esc, not Close, not a task row activate) does not close edit or change edit field values.

99. Slug reset to auto — After **Manual slug latch persists**, edit exposes a control (for example **Regenerate from title**) that clears the manual latch and immediately regenerates the slug from the current title and linked issue ticket id (when present). Until the user activates that control, title and ticket id changes do not auto-update the slug.

100. Empty inline project blocked — When inline project create is given an empty or whitespace-only name, tod does not create a project or change the selection; the selector stays open and shows a visible error (toast or banner).

101. Inline project name trim — When inline project create is given a name with leading or trailing whitespace, tod removes that whitespace before case-insensitive duplicate match and before creating a new project. A name that is empty after trim is treated as empty under **Empty inline project blocked**.

102. Repo and branch whitespace-only empty — On edit, leading and trailing whitespace are removed from repository root and branch before persist. A value that is empty after trim clears that field. Clearing repository root leaves branch unchanged.

103. Tag limit blocks commit — When the task already has 10 tags, committing another distinct tag via Enter or comma does not add a chip; tod shows a visible error (toast or banner) and leaves the tag set unchanged.

104. Project name length bound — A project name may be at most 120 characters. When inline project create is given a longer name, tod does not create the project or change the selection; the selector stays open and shows a visible error (toast or banner).

105. Notes unbounded this phase — Edit notes has no maximum length in this phase. tod does not block saving notes solely because of length.

106. Notes plain text this phase — Edit notes are stored and displayed as plain text. tod does not render markdown or other rich formatting in the notes field in this phase.

107. Open link controls — When Linear issue or GitHub PR has a value in edit, each field shows a dedicated **Open** control beside the value. Activating **Open** runs **Open linked issue/PR** for that field. When the field is empty, **Open** is not shown.

108. Blocked delete shows agent count — When **Blocked delete affordance** applies, the visible reason states how many agents are still associated with the task (for example “Delete blocked — 2 agents associated”). Agent names are not required in this phase.

109. Manual slug latched indicator — When **Manual slug latch persists** applies, edit shows a visible indicator on or next to the slug field that auto-update from title and ticket id is off. When the latch is not active, the indicator is not shown.

110. Tag length bound — Each tag on this edit surface may be at most 120 characters. Committing a longer tag via Enter, comma, or paste does not add a chip; tod shows a visible error (toast or banner) and leaves the tag set unchanged.

111. Regenerate slug auto-suffix — When **Slug reset to auto** regenerates a slug and that slug already belongs to another task, tod auto-suffixes until unique and persists the result. Regenerate is not blocked solely because the first generated slug was taken.

112. Tag chip order — Tag chips on the edit slide-over appear in alphabetical order by tag text.

113. Linear and PR length bound — Linear issue and GitHub PR values on edit may each be at most 120 characters. On edit that would exceed that limit, tod blocks the change and shows a visible error (toast or banner). On edit, the previous value for that field is kept.

114. Repo and branch length bound — Repository root and branch values on edit may each be at most 120 characters. On edit that would exceed that limit, tod blocks the change and shows a visible error (toast or banner). On edit, the previous value for that field is kept.

115. Move picker omits current no-project — When the task has no project association, **Move to project** does not offer a no-project destination in the picker. When the task has a project, **No project** remains a valid destination.

116. Live blocked delete in edit — While the edit slide-over is open, **Blocked delete affordance** and **Blocked delete shows agent count** update immediately when associated agents are added or removed, without closing edit.

117. Move inline project create — **Move to project** uses the reusable **ProjectSelector** with inline project create in the dropdown, subject to the same validation rules as compose (duplicate match, trim, length, and empty-name block).

118. Tag paste at cap shows error — When **Tag paste commits** runs and every non-empty segment is skipped solely because **Tag count bound** already applies, tod shows a visible error (toast or banner) and leaves the tag set unchanged.

119. Move picker focus — When **Move to project** opens the project picker, keyboard focus moves into the picker (for example its search field or first choosable destination).

120. Delete confirm rechecks agents — Activating **Yes** on permanent-delete confirm runs the associated-agent check again. If agents exist at that moment, tod does not delete, dismisses the toast, and **Blocked delete affordance** applies.

121. Auto-generated slug truncates — When tod auto-generates a slug (create, **Slug auto-update on title change**, **Slug auto-update from ticket**, **Slug auto-update on ticket clear**, or **Slug reset to auto**), the result is at most 120 characters by truncating from the end if needed. tod does not block the operation solely because the untruncated slug would exceed 120 characters.

122. Move blocked by constraint — When the chosen **Move to project** destination would fail a constraint revalidation check, tod does not show the move confirmation toast, shows a visible error (toast or banner), and leaves project association unchanged.

123. Slug auto-update auto-suffix — When **Slug auto-update on title change**, **Slug auto-update from ticket**, or **Slug auto-update on ticket clear** produces a slug that already belongs to another task, tod auto-suffixes until unique and persists the result. The title or ticket change is not blocked solely because the first generated slug was taken.

124. Tag paste partial skip shows error — When **Tag paste commits** adds at least one new tag but skips one or more other segments (for length, cap, or duplicate rules), tod shows a visible error (toast or banner). Successfully committed tags remain; skipped segments are not added.

125. Tag paste zero commit shows error — When **Tag paste commits** runs and no new tag is added, and **Tag count bound** does not already apply, tod shows a visible error (toast or banner). Segments that were empty or whitespace-only under tag no-op rules do not count as skipped segments for this check.

126. Create & edit runs from ticket — When Create & edit runs and the trimmed compose title matches the ticket-id pattern, tod runs **from ticket** import (same rules as **Enter creates**) before opening the edit slide-over for the resulting task.

127. Compose title placeholder — The inline compose title field shows placeholder copy that indicates the user may enter a title or a ticket id (for example “Title or ticket id (e.g. TOD-142)…” or equivalent).

128. Ticket import applies compose project — When **from ticket** import creates a new task from compose, tod applies the project chosen in the compose project selector (including no project) to that new task.

129. Move keeps edit open — After **Yes** on Move to project confirm successfully changes association, the edit slide-over stays open on the same task with updated project context.

130. Ticket import fetch failure on Create & edit — When Create & edit runs **from ticket** import and the issue tracker fetch fails, compose stays open, tod shows a visible error (toast or banner), and keyboard focus returns to the compose title input.

131. Ticket import duplicate on Create & edit — When Create & edit runs **from ticket** import and the ticket already has a task, tod does not duplicate; compose dismisses, the list selects the existing task when visible under current filter/search, and the edit slide-over opens for that task when **Create & edit respects filter** allows edit to open.

132. Ticket import duplicate hidden by filter — When Create or Enter runs **from ticket** import and the ticket already has a task that does not match the active tag filter and/or fuzzy search (if any), compose stays open with the draft title and project, tod shows a visible toast indicating that the task already exists, list selection stays unchanged, and keyboard focus remains on the compose title input.

133. Ticket import sets slug — When **from ticket** import creates a new task from compose, tod sets the slug from the fetched issue title and imported ticket id using the same auto-generation rules as edit (including truncation and auto-suffix when the first generated slug is taken).

134. Ticket import leaves tags empty — When **from ticket** import creates a new task from compose, tags remain empty; tod does not add tags from fetched issue labels.

135. Ticket import leaves notes empty — When **from ticket** import creates a new task from compose, notes remain empty; tod does not populate notes from the fetched issue description.

136. Ticket import sets title — When **from ticket** import creates a new task from compose, tod sets the task title from the fetched issue title. When the issue has no title, the title is the ticket id string used for import.

137. Ticket import populates Linear — When **from ticket** import creates a new task from compose, tod sets the Linear issue field to the imported ticket id (short id form after trim).

138. Ticket import duplicate hidden on Create & edit — When Create & edit runs **from ticket** import and the ticket already has a task that does not match the active tag filter and/or fuzzy search (if any), compose stays open with the draft title and project, tod shows a visible toast indicating that the task already exists, list selection stays unchanged, the edit slide-over does not open, and keyboard focus remains on the compose title input.

139. Ticket import blocked on title collision — When **from ticket** import would create a new task and the fetched issue title (after trim) collides with an existing task title (case-insensitive), tod does not create the task, compose stays open with the draft title and project, and tod shows a visible error (toast or banner).

140. Ticket import truncates long title — When **from ticket** import creates a new task from compose and the fetched issue title (after trim) exceeds 120 characters, tod truncates the title to 120 characters from the end and creates the task. Import is not blocked solely because the untruncated fetched title exceeds 120 characters.

141. Focus after ticket import duplicate visible — When Create or Enter runs **from ticket** import duplicate and the existing task is visible under current filter/search, compose dismisses, the list selects that task, and keyboard focus moves to the selected task row in the list.

142. Ticket import duplicate updates interaction timestamp — When Create or Enter runs **from ticket** import duplicate and selects an existing visible task, tod updates that task's interaction timestamp (same as open task edit would).

143. Focus after Create & edit respects filter — After successful Create & edit, when the new task does not match the active tag filter and/or fuzzy search (if any), keyboard focus moves to the task list pane (not a hidden row).

144. Create & edit selects visible new task — After successful Create & edit creates a new task, when that task matches the active tag filter and/or fuzzy search (if any), compose dismisses and list selection moves to that task before the edit slide-over opens.

145. Focus after ticket import duplicate on Create & edit visible — When Create & edit runs **from ticket** import duplicate and opens edit for an existing visible task, compose dismisses, the list selects that task, the edit slide-over opens, and keyboard focus moves to the edit title field.

146. Focus after Create & edit visible — After successful Create & edit creates a new task that matches the active tag filter and/or fuzzy search (if any), the edit slide-over opens and keyboard focus moves to the edit title field.

147. Focus after move confirm — After **Yes** on Move to project confirm successfully changes association, the edit slide-over stays open and keyboard focus remains in the edit slide-over (not the task list).

148. Ticket import duplicate on Create & edit updates interaction timestamp — When Create & edit runs **from ticket** import duplicate, selects an existing visible task, and opens edit, tod updates that task's interaction timestamp (same as **Ticket import duplicate updates interaction timestamp**).

149. Ticket import blocked on Create & edit keeps compose — When Create & edit runs **from ticket** import that is blocked before a task is created, compose stays open with the draft title and project, tod shows a visible error (toast or banner), the edit slide-over does not open, and keyboard focus returns to the compose title input.

150. Plain create auto-generates slug — On plain create from compose, tod auto-generates the task slug from the trimmed title (no ticket id), subject to **Create slug collision auto-suffix** and **Auto-generated slug truncates**.

151. Visual package preview capture — Before advancing to design, captured `preview.png` files exist for both `artifacts/visual/task-create/` and `artifacts/visual/task-edit/` matching the Accepted canvas packages.

152. Ticket import leaves repo and branch empty — When **from ticket** import creates a new task from compose, repository root and branch remain empty; tod does not populate them from fetched issue metadata.

153. Ticket import starts proposed — When **from ticket** import creates a new task from compose, the task starts in the `proposed` lifecycle state.

154. Ticket import title truncation silent — When **from ticket** import truncates the fetched issue title to fit **Title length bound**, import still succeeds without a separate truncation toast or banner.

155. Ticket id pattern case-insensitive — When the trimmed compose title matches the ticket-id pattern case-insensitively (e.g. `tod-142`), tod runs **from ticket** import the same as an exact-case match; **Ticket import populates Linear** stores the imported ticket id in canonical form after trim.

156. Ticket import in-progress feedback — While **from ticket** import fetch is in progress after compose submit, tod shows in-progress status in the status area and disables Create and Create & edit until the fetch completes or fails.

157. Ticket import Esc blocked — While **from ticket** import fetch is in progress after compose submit, Esc has no effect until the fetch completes or fails.

158. Ticket import duplicate case-insensitive — When **from ticket** import checks for an existing task with the same ticket, duplicate detection is case-insensitive on the ticket id string.

## Constraints

1. UI stack — GPUI and gpui-component.

2. Reusable ProjectSelector — Project pick/create is a reusable component for create, move, and future filters.

3. Visual packages — Linked create/edit packages under `artifacts/visual/` are Accepted with preview capture; create and edit UI must match those packages.

4. Cross-task — List action bar and row agent/shell launches are owned by task-list; this task owns compose and edit surfaces.
