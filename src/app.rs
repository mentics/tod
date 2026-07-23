use std::env;

use color_eyre::eyre::WrapErr;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::ListState;

use crate::create::{self, ParsedCreateInput};
use crate::credentials;
use crate::dirty::{self, DirtyAction, DirtyReport};
use crate::linear;
use crate::persist;
use crate::switch;
use crate::task::{self, Task};
use crate::treehouse;
use crate::ui;

/// Primary UI screen / mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    TaskList,
    Archive,
    Edit,
    /// Single-line input for create-new-task.
    CreatePrompt,
    /// Prompt for Linear API key when missing from the keyring.
    CredentialPrompt,
    /// Multiselect modules before leasing a worktree.
    SwitchModules,
    /// Branch name prompt before leasing a worktree.
    SwitchBranch,
    /// Dirty worktree warning before release (and optionally archive).
    DirtyWarning,
}

/// In-progress release (R alone, or as part of archive).
#[derive(Debug)]
pub struct ReleaseState {
    pub task_idx: usize,
    /// When true, archive the task after a successful release.
    pub then_archive: bool,
    pub report: DirtyReport,
    pub actions: Vec<DirtyAction>,
    pub action_cursor: usize,
}

/// Which control is focused in the edit view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFocus {
    Title,
    Branch,
    Modules,
}

#[derive(Debug)]
pub struct EditState {
    /// Index into `App::tasks`.
    pub task_idx: usize,
    pub return_to: View,
    pub focus: EditFocus,
    /// Highlighted row in the available-modules list.
    pub module_cursor: usize,
    pub available_modules: Vec<String>,
}

/// In-progress switch prerequisites (modules / branch) for a task without a worktree.
#[derive(Debug)]
pub struct SwitchPrepState {
    pub task_idx: usize,
    pub module_cursor: usize,
    pub available_modules: Vec<String>,
    pub branch_input: String,
}

#[derive(Debug)]
pub struct App {
    pub tasks: Vec<Task>,
    pub view: View,
    pub list_state: ListState,
    pub archive_state: ListState,
    pub edit: Option<EditState>,
    pub switch_prep: Option<SwitchPrepState>,
    pub release: Option<ReleaseState>,
    /// Create-prompt input buffer.
    pub create_input: String,
    /// Credential-prompt input buffer (Linear API key).
    pub credential_input: String,
    /// Create input waiting while the user supplies Linear credentials.
    pending_create_input: Option<String>,
    pub status: Option<String>,
    should_quit: bool,
}

impl App {
    pub fn new() -> color_eyre::Result<Self> {
        let tasks = persist::load_all_tasks()?;
        let mut list_state = ListState::default();
        // Row 0 is always "Create new task".
        list_state.select(Some(0));
        let mut archive_state = ListState::default();
        if tasks.iter().any(|t| t.archived) {
            archive_state.select(Some(0));
        }
        Ok(Self {
            tasks,
            view: View::TaskList,
            list_state,
            archive_state,
            edit: None,
            switch_prep: None,
            release: None,
            create_input: String::new(),
            credential_input: String::new(),
            pending_create_input: None,
            status: None,
            should_quit: false,
        })
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> color_eyre::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| ui::draw(frame, &mut self))?;
            self.handle_events()?;
        }
        Ok(())
    }

    fn handle_events(&mut self) -> color_eyre::Result<()> {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key)?,
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match self.view {
            View::TaskList => self.handle_task_list_key(key)?,
            View::Archive => self.handle_archive_key(key)?,
            View::Edit => self.handle_edit_key(key)?,
            View::CreatePrompt => self.handle_create_prompt_key(key)?,
            View::CredentialPrompt => self.handle_credential_prompt_key(key)?,
            View::SwitchModules => self.handle_switch_modules_key(key)?,
            View::SwitchBranch => self.handle_switch_branch_key(key)?,
            View::DirtyWarning => self.handle_dirty_warning_key(key)?,
        }
        Ok(())
    }

    // --- Task list ---------------------------------------------------------

    fn handle_task_list_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            // Shift+A opens archive view. Uppercase 'A' usually implies Shift even
            // when the terminal omits KeyModifiers::SHIFT.
            KeyCode::Char('a') | KeyCode::Char('A') if shift => self.open_archive_view(),
            KeyCode::Char('A') => self.open_archive_view(),
            KeyCode::Char('a') => self.archive_selected()?,
            KeyCode::Char('e') | KeyCode::Char('E') => self.open_edit_for_list_selection()?,
            KeyCode::Char('r') | KeyCode::Char('R') => self.release_selected()?,
            KeyCode::Down | KeyCode::Char('j') => self.select_next_list(),
            KeyCode::Up | KeyCode::Char('k') => self.select_previous_list(),
            KeyCode::Enter => self.activate_list_selection(),
            _ => {}
        }
        Ok(())
    }

    /// Rows: [Create new task] + active tasks. Index 0 = create.
    pub fn task_list_row_count(&self) -> usize {
        1 + self.active_tasks().count()
    }

    pub fn active_tasks(&self) -> impl Iterator<Item = (usize, &Task)> {
        self.tasks.iter().enumerate().filter(|(_, t)| !t.archived)
    }

    pub fn archived_tasks(&self) -> impl Iterator<Item = (usize, &Task)> {
        self.tasks.iter().enumerate().filter(|(_, t)| t.archived)
    }

    pub fn archived_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.archived).count()
    }

    /// Map list UI index (1..) to `tasks` index. Index 0 is Create.
    fn tasks_index_for_list_row(&self, row: usize) -> Option<usize> {
        if row == 0 {
            return None;
        }
        self.active_tasks().nth(row - 1).map(|(i, _)| i)
    }

    pub fn selected_list_task(&self) -> Option<&Task> {
        let row = self.list_state.selected()?;
        let idx = self.tasks_index_for_list_row(row)?;
        self.tasks.get(idx)
    }

    fn select_next_list(&mut self) {
        let len = self.task_list_row_count();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().map_or(0, |i| (i + 1) % len);
        self.list_state.select(Some(i));
    }

    fn select_previous_list(&mut self) {
        let len = self.task_list_row_count();
        if len == 0 {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| if i == 0 { len - 1 } else { i - 1 });
        self.list_state.select(Some(i));
    }

    fn activate_list_selection(&mut self) {
        match self.list_state.selected() {
            Some(0) => {
                self.create_input.clear();
                self.view = View::CreatePrompt;
                self.clear_status();
            }
            Some(row) => {
                if let Some(task_idx) = self.tasks_index_for_list_row(row)
                    && let Err(err) = self.start_switch(task_idx)
                {
                    self.set_status(format!("Switch failed: {err:#}"));
                }
            }
            None => {}
        }
    }

    fn open_archive_view(&mut self) {
        self.view = View::Archive;
        self.clear_status();
        let n = self.archived_count();
        if n == 0 {
            self.archive_state.select(None);
        } else if self.archive_state.selected().is_none_or(|i| i >= n) {
            self.archive_state.select(Some(0));
        }
    }

    fn open_edit_for_list_selection(&mut self) -> color_eyre::Result<()> {
        let Some(row) = self.list_state.selected() else {
            return Ok(());
        };
        if row == 0 {
            self.set_status("Select a task to edit");
            return Ok(());
        }
        let Some(task_idx) = self.tasks_index_for_list_row(row) else {
            return Ok(());
        };
        self.open_edit(task_idx, View::TaskList)
    }

    fn archive_selected(&mut self) -> color_eyre::Result<()> {
        let Some(row) = self.list_state.selected() else {
            return Ok(());
        };
        if row == 0 {
            self.set_status("Select a task to archive");
            return Ok(());
        }
        let Some(task_idx) = self.tasks_index_for_list_row(row) else {
            return Ok(());
        };

        if self.tasks[task_idx].worktree.is_some() {
            return self.begin_release(task_idx, true);
        }

        self.finish_archive(task_idx)
    }

    fn release_selected(&mut self) -> color_eyre::Result<()> {
        let Some(row) = self.list_state.selected() else {
            return Ok(());
        };
        if row == 0 {
            self.set_status("Select a task to release its worktree");
            return Ok(());
        }
        let Some(task_idx) = self.tasks_index_for_list_row(row) else {
            return Ok(());
        };

        if self.tasks[task_idx].worktree.is_none() {
            self.set_status("No worktree associated with this task");
            return Ok(());
        }

        self.begin_release(task_idx, false)
    }

    /// Start release: dirty-check first; show warning or proceed.
    fn begin_release(&mut self, task_idx: usize, then_archive: bool) -> color_eyre::Result<()> {
        let Some(path) = self
            .tasks
            .get(task_idx)
            .and_then(|t| t.worktree.as_ref())
            .map(|w| w.path.clone())
        else {
            self.set_status("No worktree to release");
            return Ok(());
        };

        match dirty::inspect_worktree(&path) {
            Ok(report) if report.is_clean() => self.complete_release(task_idx, then_archive),
            Ok(report) => {
                let actions = dirty::menu_actions(&report);
                self.release = Some(ReleaseState {
                    task_idx,
                    then_archive,
                    report,
                    actions,
                    action_cursor: 0,
                });
                self.view = View::DirtyWarning;
                self.clear_status();
                Ok(())
            }
            Err(err) => {
                self.set_status(format!("Dirty check failed: {err:#}"));
                Ok(())
            }
        }
    }

    fn finish_archive(&mut self, task_idx: usize) -> color_eyre::Result<()> {
        let Some(task) = self.tasks.get_mut(task_idx) else {
            return Ok(());
        };
        task.archived = true;
        task.touch();
        persist::save_task(task)?;
        self.sort_tasks();
        self.clamp_list_selection();
        self.release = None;
        self.view = View::TaskList;
        self.set_status("Task archived");
        Ok(())
    }

    /// Return worktree to Treehouse, clear association, optionally archive.
    fn complete_release(&mut self, task_idx: usize, then_archive: bool) -> color_eyre::Result<()> {
        let (stem, path, wt_num) = {
            let Some(task) = self.tasks.get(task_idx) else {
                return Ok(());
            };
            let Some(wt) = task.worktree.as_ref() else {
                self.set_status("No worktree to release");
                return Ok(());
            };
            (task.file_stem.clone(), wt.path.clone(), wt.number)
        };

        if let Err(err) = treehouse::return_worktree(&path) {
            self.release = None;
            self.view = View::TaskList;
            self.set_status(format!("Treehouse return failed: {err:#}"));
            return Ok(());
        }

        let task_idx = self
            .tasks
            .iter()
            .position(|t| t.file_stem == stem)
            .ok_or_else(|| color_eyre::eyre::eyre!("task disappeared during release"))?;

        {
            let task = &mut self.tasks[task_idx];
            task.worktree = None;
            task.touch();
            persist::save_task(task)?;
        }
        self.sort_tasks();

        let task_idx = self
            .tasks
            .iter()
            .position(|t| t.file_stem == stem)
            .ok_or_else(|| color_eyre::eyre::eyre!("task disappeared after release"))?;

        self.release = None;
        if then_archive {
            self.finish_archive(task_idx)?;
            self.set_status(format!("Released worktree {wt_num} and archived task"));
        } else {
            self.view = View::TaskList;
            self.select_active_task_by_stem(&stem);
            self.set_status(format!("Released worktree {wt_num}"));
        }
        Ok(())
    }

    fn clamp_list_selection(&mut self) {
        let len = self.task_list_row_count();
        match self.list_state.selected() {
            Some(i) if i >= len => {
                if len == 0 {
                    self.list_state.select(None);
                } else {
                    self.list_state.select(Some(len - 1));
                }
            }
            None if len > 0 => self.list_state.select(Some(0)),
            _ => {}
        }
    }

    // --- Archive -----------------------------------------------------------

    fn handle_archive_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Esc => {
                self.view = View::TaskList;
                self.clear_status();
            }
            KeyCode::Char('u') | KeyCode::Char('U') => self.unarchive_selected()?,
            KeyCode::Down | KeyCode::Char('j') => self.select_next_archive(),
            KeyCode::Up | KeyCode::Char('k') => self.select_previous_archive(),
            _ => {}
        }
        Ok(())
    }

    fn tasks_index_for_archive_row(&self, row: usize) -> Option<usize> {
        self.archived_tasks().nth(row).map(|(i, _)| i)
    }

    pub fn selected_archive_task(&self) -> Option<&Task> {
        let row = self.archive_state.selected()?;
        let idx = self.tasks_index_for_archive_row(row)?;
        self.tasks.get(idx)
    }

    fn select_next_archive(&mut self) {
        let len = self.archived_count();
        if len == 0 {
            return;
        }
        let i = self.archive_state.selected().map_or(0, |i| (i + 1) % len);
        self.archive_state.select(Some(i));
    }

    fn select_previous_archive(&mut self) {
        let len = self.archived_count();
        if len == 0 {
            return;
        }
        let i = self
            .archive_state
            .selected()
            .map_or(0, |i| if i == 0 { len - 1 } else { i - 1 });
        self.archive_state.select(Some(i));
    }

    fn unarchive_selected(&mut self) -> color_eyre::Result<()> {
        let Some(row) = self.archive_state.selected() else {
            self.set_status("No archived task selected");
            return Ok(());
        };
        let Some(task_idx) = self.tasks_index_for_archive_row(row) else {
            return Ok(());
        };

        self.tasks[task_idx].archived = false;
        self.tasks[task_idx].touch();
        persist::save_task(&self.tasks[task_idx])?;
        self.sort_tasks();
        self.clamp_archive_selection();
        self.set_status("Task unarchived");
        Ok(())
    }

    fn clamp_archive_selection(&mut self) {
        let len = self.archived_count();
        match self.archive_state.selected() {
            Some(i) if i >= len => {
                if len == 0 {
                    self.archive_state.select(None);
                } else {
                    self.archive_state.select(Some(len - 1));
                }
            }
            None if len > 0 => self.archive_state.select(Some(0)),
            _ => {}
        }
    }

    // --- Edit --------------------------------------------------------------

    fn open_edit(&mut self, task_idx: usize, return_to: View) -> color_eyre::Result<()> {
        let available_modules = match env::current_dir() {
            Ok(cwd) => match task::available_modules(&cwd) {
                Ok(mods) => mods,
                Err(err) => {
                    self.set_status(format!("Module discovery failed: {err:#}"));
                    Vec::new()
                }
            },
            Err(err) => {
                self.set_status(format!("Could not read cwd: {err}"));
                Vec::new()
            }
        };

        self.edit = Some(EditState {
            task_idx,
            return_to,
            focus: EditFocus::Title,
            module_cursor: 0,
            available_modules,
        });
        self.view = View::Edit;
        self.clear_status();
        Ok(())
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        let focus = self.edit.as_ref().map(|e| e.focus);
        match (focus, key.code) {
            (_, KeyCode::Esc) => self.leave_edit(),
            (_, KeyCode::Tab) => self.edit_focus_next(),
            (_, KeyCode::BackTab) => self.edit_focus_prev(),
            (Some(EditFocus::Modules), KeyCode::Down | KeyCode::Char('j')) => {
                self.edit_module_move(1);
            }
            (Some(EditFocus::Modules), KeyCode::Up | KeyCode::Char('k')) => {
                self.edit_module_move(-1);
            }
            (_, KeyCode::Down) => self.edit_focus_next(),
            (_, KeyCode::Up) => self.edit_focus_prev(),
            (_, KeyCode::Enter) => self.edit_confirm_field()?,
            (Some(EditFocus::Modules), KeyCode::Char(' ')) => self.toggle_selected_module()?,
            (Some(EditFocus::Modules), KeyCode::Char('q') | KeyCode::Char('Q')) => {
                self.should_quit = true;
            }
            (Some(EditFocus::Title | EditFocus::Branch), KeyCode::Backspace) => {
                self.edit_backspace()?;
            }
            (Some(EditFocus::Title | EditFocus::Branch), KeyCode::Char(c)) => {
                // Letters (including q) go into the text field while focused here.
                self.edit_insert_char(c)?;
            }
            (_, KeyCode::Char('q') | KeyCode::Char('Q')) => self.should_quit = true,
            _ => {}
        }
        Ok(())
    }

    fn leave_edit(&mut self) {
        let return_to = self
            .edit
            .as_ref()
            .map(|e| e.return_to)
            .unwrap_or(View::TaskList);
        self.edit = None;
        self.view = return_to;
        self.clear_status();
    }

    fn edit_focus_next(&mut self) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        edit.focus = match edit.focus {
            EditFocus::Title => EditFocus::Branch,
            EditFocus::Branch => EditFocus::Modules,
            EditFocus::Modules => EditFocus::Title,
        };
        if edit.focus == EditFocus::Modules && !edit.available_modules.is_empty() {
            edit.module_cursor = edit
                .module_cursor
                .min(edit.available_modules.len().saturating_sub(1));
        }
    }

    fn edit_focus_prev(&mut self) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        edit.focus = match edit.focus {
            EditFocus::Title => EditFocus::Modules,
            EditFocus::Branch => EditFocus::Title,
            EditFocus::Modules => EditFocus::Branch,
        };
        if edit.focus == EditFocus::Modules && !edit.available_modules.is_empty() {
            edit.module_cursor = edit
                .module_cursor
                .min(edit.available_modules.len().saturating_sub(1));
        }
    }

    fn edit_module_move(&mut self, delta: i32) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let len = edit.available_modules.len();
        if len == 0 {
            return;
        }
        let cur = edit.module_cursor as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        edit.module_cursor = next;
    }

    fn edit_confirm_field(&mut self) -> color_eyre::Result<()> {
        let Some(edit) = self.edit.as_ref() else {
            return Ok(());
        };
        match edit.focus {
            EditFocus::Title | EditFocus::Branch => {
                // Text already persisted on each keystroke; Enter advances focus.
                self.edit_focus_next();
            }
            EditFocus::Modules => {
                // Enter on modules: no-op (Space toggles).
            }
        }
        Ok(())
    }

    fn edit_insert_char(&mut self, c: char) -> color_eyre::Result<()> {
        let (task_idx, focus) = {
            let Some(edit) = self.edit.as_ref() else {
                return Ok(());
            };
            (edit.task_idx, edit.focus)
        };
        let Some(task) = self.tasks.get_mut(task_idx) else {
            return Ok(());
        };
        match focus {
            EditFocus::Title => task.title.push(c),
            EditFocus::Branch => {
                let branch = task.branch.get_or_insert_with(String::new);
                branch.push(c);
            }
            EditFocus::Modules => return Ok(()),
        }
        task.touch();
        persist::save_task(task)?;
        self.sort_tasks_preserving_edit(task_idx)?;
        Ok(())
    }

    fn edit_backspace(&mut self) -> color_eyre::Result<()> {
        let (task_idx, focus) = {
            let Some(edit) = self.edit.as_ref() else {
                return Ok(());
            };
            (edit.task_idx, edit.focus)
        };
        if !matches!(focus, EditFocus::Title | EditFocus::Branch) {
            return Ok(());
        }
        let Some(task) = self.tasks.get_mut(task_idx) else {
            return Ok(());
        };
        match focus {
            EditFocus::Title => {
                task.title.pop();
            }
            EditFocus::Branch => {
                if let Some(branch) = task.branch.as_mut() {
                    branch.pop();
                    if branch.is_empty() {
                        task.branch = None;
                    }
                }
            }
            EditFocus::Modules => return Ok(()),
        }
        task.touch();
        persist::save_task(task)?;
        self.sort_tasks_preserving_edit(task_idx)?;
        Ok(())
    }

    fn toggle_selected_module(&mut self) -> color_eyre::Result<()> {
        let (task_idx, module_name) = {
            let Some(edit) = self.edit.as_ref() else {
                return Ok(());
            };
            let Some(name) = edit.available_modules.get(edit.module_cursor) else {
                return Ok(());
            };
            (edit.task_idx, name.clone())
        };
        let Some(task) = self.tasks.get_mut(task_idx) else {
            return Ok(());
        };
        if let Some(pos) = task.modules.iter().position(|m| m == &module_name) {
            task.modules.remove(pos);
        } else {
            task.modules.push(module_name);
        }
        task.touch();
        persist::save_task(task)?;
        self.sort_tasks_preserving_edit(task_idx)?;
        Ok(())
    }

    /// Re-sort by last_used and keep `edit.task_idx` pointing at the same task.
    fn sort_tasks_preserving_edit(&mut self, old_idx: usize) -> color_eyre::Result<()> {
        let stem = self
            .tasks
            .get(old_idx)
            .map(|t| t.file_stem.clone())
            .ok_or_else(|| color_eyre::eyre::eyre!("edit task index out of range"))?;
        self.sort_tasks();
        if let Some(edit) = self.edit.as_mut() {
            edit.task_idx = self
                .tasks
                .iter()
                .position(|t| t.file_stem == stem)
                .ok_or_else(|| color_eyre::eyre::eyre!("edited task disappeared after sort"))?;
        }
        Ok(())
    }

    // --- Create prompt -----------------------------------------------------

    fn handle_create_prompt_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.create_input.clear();
                self.pending_create_input = None;
                self.view = View::TaskList;
                self.clear_status();
            }
            KeyCode::Enter => {
                let input = self.create_input.clone();
                self.submit_create(&input)?;
            }
            KeyCode::Backspace => {
                self.create_input.pop();
            }
            KeyCode::Char(c) => {
                self.create_input.push(c);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_credential_prompt_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.credential_input.clear();
                self.pending_create_input = None;
                self.create_input.clear();
                self.view = View::TaskList;
                self.set_status("Create cancelled (no Linear API key)");
            }
            KeyCode::Enter => {
                let key_text = self.credential_input.trim().to_string();
                if key_text.is_empty() {
                    self.set_status("Linear API key cannot be empty");
                    return Ok(());
                }
                if let Err(err) = credentials::store_linear_api_key(&key_text) {
                    self.set_status(format!("Could not store API key: {err:#}"));
                    return Ok(());
                }
                self.credential_input.clear();
                let pending = self.pending_create_input.take();
                match pending {
                    Some(input) => {
                        self.create_input.clear();
                        self.submit_create(&input)?;
                    }
                    None => {
                        self.view = View::TaskList;
                        self.set_status("Linear API key saved");
                    }
                }
            }
            KeyCode::Backspace => {
                self.credential_input.pop();
            }
            KeyCode::Char(c) => {
                self.credential_input.push(c);
            }
            _ => {}
        }
        Ok(())
    }

    /// Parse input, optionally fetch Linear issue, persist task, return to list.
    fn submit_create(&mut self, input: &str) -> color_eyre::Result<()> {
        let parsed = match create::parse_create_input(input) {
            Ok(p) => p,
            Err(err) => {
                self.set_status(format!("Create failed: {err:#}"));
                // Stay on create prompt so the user can fix input.
                self.view = View::CreatePrompt;
                return Ok(());
            }
        };

        let (title, branch, issue_id) = match parsed {
            ParsedCreateInput::Title(title) => (title, None, None),
            ParsedCreateInput::Branch(name) => (name.clone(), Some(name), None),
            ParsedCreateInput::IssueId(id) => match self.resolve_linear_issue(&id)? {
                Some(issue) => (issue.title, None, Some(issue.identifier)),
                None => {
                    // Switched to credential prompt; wait for key then retry.
                    return Ok(());
                }
            },
        };

        self.finish_create_task(title, branch, issue_id)
    }

    /// Returns `None` when switching to the credential prompt (caller should return).
    fn resolve_linear_issue(
        &mut self,
        issue_id: &str,
    ) -> color_eyre::Result<Option<linear::LinearIssue>> {
        let api_key = match credentials::load_linear_api_key() {
            Ok(Some(key)) => key,
            Ok(None) => {
                self.pending_create_input = Some(issue_id.to_string());
                self.credential_input.clear();
                self.view = View::CredentialPrompt;
                self.set_status("Enter your Linear API key (stored in OS keyring)");
                return Ok(None);
            }
            Err(err) => {
                self.set_status(format!("Keyring error: {err:#}"));
                self.view = View::CreatePrompt;
                return Ok(None);
            }
        };

        match linear::fetch_issue_by_identifier(&api_key, issue_id) {
            Ok(issue) => Ok(Some(issue)),
            Err(err) => {
                self.set_status(format!("Linear lookup failed: {err:#}"));
                self.view = View::CreatePrompt;
                Ok(None)
            }
        }
    }

    fn finish_create_task(
        &mut self,
        title: String,
        branch: Option<String>,
        issue_id: Option<String>,
    ) -> color_eyre::Result<()> {
        let stem = persist::allocate_file_stem(&title)?;
        let mut task = Task::new(title.clone(), stem);
        task.branch = branch;
        task.issue_id = issue_id;
        task.touch();
        persist::save_task(&task)?;

        let stem = task.file_stem.clone();
        self.tasks.push(task);
        self.sort_tasks();
        self.select_active_task_by_stem(&stem);

        self.create_input.clear();
        self.pending_create_input = None;
        self.view = View::TaskList;
        self.set_status(format!("Created task: {title}"));
        Ok(())
    }

    fn select_active_task_by_stem(&mut self, stem: &str) {
        let row = self
            .active_tasks()
            .enumerate()
            .find(|(_, (_, task))| task.file_stem == stem)
            .map(|(row_offset, _)| row_offset + 1); // +1: row 0 is Create
        if let Some(row) = row {
            self.list_state.select(Some(row));
        }
    }

    // --- Switch --------------------------------------------------------------

    /// Enter on a task: ensure worktree (prompt / lease if needed), activate, open Cursor.
    fn start_switch(&mut self, task_idx: usize) -> color_eyre::Result<()> {
        self.switch_prep = None;
        self.clear_status();

        if self.tasks.get(task_idx).is_none() {
            self.set_status("Task missing");
            return Ok(());
        }

        if self.tasks[task_idx].worktree.is_some() {
            return self.finish_switch(task_idx);
        }

        // No worktree yet: ensure modules + branch, then lease.
        if self.tasks[task_idx].modules.is_empty() {
            return self.open_switch_modules_prompt(task_idx);
        }
        if self.tasks[task_idx]
            .branch
            .as_ref()
            .map(|b| b.trim().is_empty())
            .unwrap_or(true)
        {
            return self.open_switch_branch_prompt(task_idx);
        }

        self.finish_switch(task_idx)
    }

    fn open_switch_modules_prompt(&mut self, task_idx: usize) -> color_eyre::Result<()> {
        let available_modules = self.discover_modules_or_status();
        self.switch_prep = Some(SwitchPrepState {
            task_idx,
            module_cursor: 0,
            available_modules,
            branch_input: String::new(),
        });
        self.view = View::SwitchModules;
        self.set_status("Select modules for this task (Space toggle, Enter confirm)");
        Ok(())
    }

    fn open_switch_branch_prompt(&mut self, task_idx: usize) -> color_eyre::Result<()> {
        let available_modules = self
            .switch_prep
            .as_ref()
            .map(|s| s.available_modules.clone())
            .unwrap_or_else(|| self.discover_modules_or_status());
        self.switch_prep = Some(SwitchPrepState {
            task_idx,
            module_cursor: 0,
            available_modules,
            branch_input: self.tasks[task_idx].branch.clone().unwrap_or_default(),
        });
        self.view = View::SwitchBranch;
        self.set_status("Enter a branch name for this task");
        Ok(())
    }

    fn discover_modules_or_status(&mut self) -> Vec<String> {
        match env::current_dir() {
            Ok(cwd) => match task::available_modules(&cwd) {
                Ok(mods) => mods,
                Err(err) => {
                    self.set_status(format!("Module discovery failed: {err:#}"));
                    Vec::new()
                }
            },
            Err(err) => {
                self.set_status(format!("Could not read cwd: {err}"));
                Vec::new()
            }
        }
    }

    fn handle_switch_modules_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.switch_prep = None;
                self.view = View::TaskList;
                self.set_status("Switch cancelled");
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.switch_module_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.switch_module_move(-1),
            KeyCode::Char(' ') => self.switch_toggle_module()?,
            KeyCode::Enter => self.confirm_switch_modules()?,
            _ => {}
        }
        Ok(())
    }

    fn handle_switch_branch_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.switch_prep = None;
                self.view = View::TaskList;
                self.set_status("Switch cancelled");
            }
            KeyCode::Enter => self.confirm_switch_branch()?,
            KeyCode::Backspace => {
                if let Some(prep) = self.switch_prep.as_mut() {
                    prep.branch_input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(prep) = self.switch_prep.as_mut() {
                    prep.branch_input.push(c);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn switch_module_move(&mut self, delta: i32) {
        let Some(prep) = self.switch_prep.as_mut() else {
            return;
        };
        let len = prep.available_modules.len();
        if len == 0 {
            return;
        }
        let cur = prep.module_cursor as i32;
        prep.module_cursor = (cur + delta).rem_euclid(len as i32) as usize;
    }

    fn switch_toggle_module(&mut self) -> color_eyre::Result<()> {
        let (task_idx, module_name) = {
            let Some(prep) = self.switch_prep.as_ref() else {
                return Ok(());
            };
            let Some(name) = prep.available_modules.get(prep.module_cursor) else {
                return Ok(());
            };
            (prep.task_idx, name.clone())
        };
        let Some(task) = self.tasks.get_mut(task_idx) else {
            return Ok(());
        };
        if let Some(pos) = task.modules.iter().position(|m| m == &module_name) {
            task.modules.remove(pos);
        } else {
            task.modules.push(module_name);
        }
        task.touch();
        persist::save_task(task)?;
        self.sort_tasks_preserving_switch(task_idx)?;
        Ok(())
    }

    fn confirm_switch_modules(&mut self) -> color_eyre::Result<()> {
        let Some(task_idx) = self.switch_prep.as_ref().map(|p| p.task_idx) else {
            return Ok(());
        };
        if self.tasks[task_idx].modules.is_empty() {
            self.set_status("Select at least one module (Space), then Enter");
            return Ok(());
        }
        // Persist already happened on toggle; continue to branch or finish.
        if self.tasks[task_idx]
            .branch
            .as_ref()
            .map(|b| b.trim().is_empty())
            .unwrap_or(true)
        {
            return self.open_switch_branch_prompt(task_idx);
        }
        self.finish_switch(task_idx)
    }

    fn confirm_switch_branch(&mut self) -> color_eyre::Result<()> {
        let Some(prep) = self.switch_prep.as_ref() else {
            return Ok(());
        };
        let task_idx = prep.task_idx;
        let branch = prep.branch_input.trim().to_string();
        if branch.is_empty() {
            self.set_status("Branch name cannot be empty");
            return Ok(());
        }
        {
            let Some(task) = self.tasks.get_mut(task_idx) else {
                return Ok(());
            };
            task.branch = Some(branch);
            task.touch();
            persist::save_task(task)?;
        }
        self.sort_tasks_preserving_switch(task_idx)?;
        let task_idx = self
            .switch_prep
            .as_ref()
            .map(|p| p.task_idx)
            .unwrap_or(task_idx);
        self.finish_switch(task_idx)
    }

    /// Re-sort and keep `switch_prep.task_idx` pointing at the same task.
    fn sort_tasks_preserving_switch(&mut self, old_idx: usize) -> color_eyre::Result<()> {
        let stem = self
            .tasks
            .get(old_idx)
            .map(|t| t.file_stem.clone())
            .ok_or_else(|| color_eyre::eyre::eyre!("switch task index out of range"))?;
        self.sort_tasks();
        if let Some(prep) = self.switch_prep.as_mut() {
            prep.task_idx = self
                .tasks
                .iter()
                .position(|t| t.file_stem == stem)
                .ok_or_else(|| color_eyre::eyre::eyre!("switch task disappeared after sort"))?;
        }
        Ok(())
    }

    /// Lease if needed, activate branches, launch Cursor, touch + persist.
    fn finish_switch(&mut self, task_idx: usize) -> color_eyre::Result<()> {
        let stem = self
            .tasks
            .get(task_idx)
            .map(|t| t.file_stem.clone())
            .ok_or_else(|| color_eyre::eyre::eyre!("switch task missing"))?;

        // Lease when no worktree yet.
        if self.tasks[task_idx].worktree.is_none() {
            let cwd = env::current_dir().wrap_err("reading cwd for treehouse lease")?;
            match switch::lease_new_worktree(&cwd) {
                Ok(wt) => {
                    let task = &mut self.tasks[task_idx];
                    task.worktree = Some(wt);
                    task.touch();
                    persist::save_task(task)?;
                }
                Err(err) => {
                    self.switch_prep = None;
                    self.view = View::TaskList;
                    self.set_status(format!("Treehouse lease failed: {err:#}"));
                    return Ok(());
                }
            }
            self.sort_tasks();
        }

        let task_idx = self
            .tasks
            .iter()
            .position(|t| t.file_stem == stem)
            .ok_or_else(|| color_eyre::eyre::eyre!("task disappeared after lease"))?;

        let (worktree, modules, branch) = {
            let task = &self.tasks[task_idx];
            let worktree = task
                .worktree
                .clone()
                .ok_or_else(|| color_eyre::eyre::eyre!("worktree missing after lease"))?;
            let branch = task
                .branch
                .clone()
                .filter(|b| !b.trim().is_empty())
                .ok_or_else(|| color_eyre::eyre::eyre!("branch missing before activate"))?;
            (worktree, task.modules.clone(), branch)
        };

        if let Err(err) = switch::activate_worktree(&worktree, &modules, &branch) {
            self.switch_prep = None;
            self.view = View::TaskList;
            self.set_status(format!("Activate worktree failed: {err:#}"));
            return Ok(());
        }

        if let Err(err) = switch::launch_cursor(&worktree) {
            self.switch_prep = None;
            self.view = View::TaskList;
            self.set_status(format!("Opened worktree but Cursor launch failed: {err:#}"));
            // Still touch — switch mostly succeeded.
            self.tasks[task_idx].touch();
            persist::save_task(&self.tasks[task_idx])?;
            self.sort_tasks();
            self.select_active_task_by_stem(&stem);
            return Ok(());
        }

        {
            let task = &mut self.tasks[task_idx];
            task.touch();
            persist::save_task(task)?;
        }
        self.sort_tasks();
        self.select_active_task_by_stem(&stem);

        self.switch_prep = None;
        self.view = View::TaskList;
        let wt_num = self
            .tasks
            .iter()
            .find(|t| t.file_stem == stem)
            .and_then(|t| t.worktree.as_ref())
            .map(|w| w.number);
        let msg = match wt_num {
            Some(n) => format!("Switched to task (worktree {n}); opened Cursor"),
            None => "Switched to task; opened Cursor".to_string(),
        };
        self.set_status(msg);
        Ok(())
    }

    // --- Release / dirty warning -------------------------------------------

    fn handle_dirty_warning_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Esc => self.cancel_release(),
            KeyCode::Down | KeyCode::Char('j') => self.move_dirty_action(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_dirty_action(-1),
            KeyCode::Enter => self.confirm_dirty_action()?,
            KeyCode::Char(c) => {
                let lower = c.to_ascii_lowercase();
                if let Some(rel) = self.release.as_ref()
                    && let Some(idx) = rel.actions.iter().position(|a| a.shortcut() == lower)
                {
                    if let Some(rel) = self.release.as_mut() {
                        rel.action_cursor = idx;
                    }
                    self.confirm_dirty_action()?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn move_dirty_action(&mut self, delta: i32) {
        let Some(rel) = self.release.as_mut() else {
            return;
        };
        let len = rel.actions.len();
        if len == 0 {
            return;
        }
        let cur = rel.action_cursor as i32;
        rel.action_cursor = (cur + delta).rem_euclid(len as i32) as usize;
    }

    fn confirm_dirty_action(&mut self) -> color_eyre::Result<()> {
        let (action, task_idx, then_archive, path) = {
            let Some(rel) = self.release.as_ref() else {
                return Ok(());
            };
            let Some(action) = rel.actions.get(rel.action_cursor).copied() else {
                return Ok(());
            };
            let path = self
                .tasks
                .get(rel.task_idx)
                .and_then(|t| t.worktree.as_ref())
                .map(|w| w.path.clone());
            (action, rel.task_idx, rel.then_archive, path)
        };

        match action {
            DirtyAction::Cancel => {
                self.cancel_release();
            }
            DirtyAction::CheckAgain => {
                let Some(path) = path else {
                    self.cancel_release();
                    self.set_status("Worktree association missing; release cancelled");
                    return Ok(());
                };
                match dirty::inspect_worktree(&path) {
                    Ok(report) if report.is_clean() => {
                        self.complete_release(task_idx, then_archive)?;
                    }
                    Ok(report) => {
                        let actions = dirty::menu_actions(&report);
                        if let Some(rel) = self.release.as_mut() {
                            rel.report = report;
                            rel.actions = actions;
                            rel.action_cursor = 0;
                        }
                        self.set_status("Still dirty — fix leftovers or stash, then check again");
                    }
                    Err(err) => {
                        self.set_status(format!("Dirty check failed: {err:#}"));
                    }
                }
            }
            DirtyAction::StashChanges => {
                let Some(path) = path else {
                    self.cancel_release();
                    self.set_status("Worktree association missing; release cancelled");
                    return Ok(());
                };
                if let Err(err) = dirty::stash_local_changes(&path) {
                    self.set_status(format!("Stash failed: {err:#}"));
                    return Ok(());
                }
                match dirty::inspect_worktree(&path) {
                    Ok(report) if report.is_clean() => {
                        self.complete_release(task_idx, then_archive)?;
                    }
                    Ok(report) => {
                        let msg = if report.has_remote_divergence() && !report.has_local_changes() {
                            "Stashed local changes; remote divergence still blocks release"
                                .to_string()
                        } else {
                            "Stashed; still dirty — check again after fixing".to_string()
                        };
                        let actions = dirty::menu_actions(&report);
                        if let Some(rel) = self.release.as_mut() {
                            rel.report = report;
                            rel.actions = actions;
                            rel.action_cursor = 0;
                        }
                        self.set_status(msg);
                    }
                    Err(err) => {
                        self.set_status(format!("Re-check after stash failed: {err:#}"));
                    }
                }
            }
        }
        Ok(())
    }

    fn cancel_release(&mut self) {
        let then_archive = self
            .release
            .as_ref()
            .map(|r| r.then_archive)
            .unwrap_or(false);
        self.release = None;
        self.view = View::TaskList;
        if then_archive {
            self.set_status("Archive cancelled (worktree not released)");
        } else {
            self.set_status("Release cancelled");
        }
    }

    // --- Shared helpers ----------------------------------------------------

    fn sort_tasks(&mut self) {
        self.tasks.sort_by_key(|b| std::cmp::Reverse(b.last_used));
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    fn clear_status(&mut self) {
        self.status = None;
    }
}
