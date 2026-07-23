use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;
use ratatui::widgets::ListState;

use crate::task::{Task, TaskStatus};
use crate::ui;

#[derive(Debug)]
pub struct App {
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        let tasks = vec![
            Task::new("example-feature", "~/dev/example", TaskStatus::Active),
            Task::new("bugfix-login", "~/dev/example-wt-login", TaskStatus::Paused),
            Task::new("docs-pass", "~/dev/example-wt-docs", TaskStatus::Done),
        ];
        let mut list_state = ListState::default();
        if !tasks.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            tasks,
            list_state,
            should_quit: false,
        }
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
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key.code),
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
            KeyCode::Home => self.list_state.select_first(),
            KeyCode::End => {
                if !self.tasks.is_empty() {
                    self.list_state.select(Some(self.tasks.len() - 1));
                }
            }
            _ => {}
        }
    }

    fn select_next(&mut self) {
        let len = self.tasks.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().map_or(0, |i| (i + 1) % len);
        self.list_state.select(Some(i));
    }

    fn select_previous(&mut self) {
        let len = self.tasks.len();
        if len == 0 {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| if i == 0 { len - 1 } else { i - 1 });
        self.list_state.select(Some(i));
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.list_state.selected().and_then(|i| self.tasks.get(i))
    }
}
