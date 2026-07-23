use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::app::{App, EditFocus, View};
use crate::dirty;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(4),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    match app.view {
        View::TaskList => draw_task_list(frame, body, app),
        View::Archive => draw_archive_list(frame, body, app),
        View::Edit => draw_edit(frame, body, app),
        View::CreatePrompt => draw_create_prompt(frame, body, app),
        View::CredentialPrompt => draw_credential_prompt(frame, body, app),
        View::SwitchModules => draw_switch_modules(frame, body, app),
        View::SwitchBranch => draw_switch_branch(frame, body, app),
        View::DirtyWarning => draw_dirty_warning(frame, body, app),
    }
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let subtitle = match app.view {
        View::TaskList => "task manager",
        View::Archive => "archive",
        View::Edit => "edit task",
        View::CreatePrompt => "create task",
        View::CredentialPrompt => "credentials",
        View::SwitchModules => "switch — modules",
        View::SwitchBranch => "switch — branch",
        View::DirtyWarning => "dirty worktree",
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled("tod", Style::default().bold().fg(Color::Cyan)),
        Span::raw(format!(" — {subtitle}")),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, area);
}

fn format_task_row(title: &str, branch: Option<&str>, wt_num: Option<i32>) -> Line<'static> {
    let mut spans = vec![Span::raw(title.to_string())];
    if let Some(branch) = branch {
        spans.push(Span::styled(
            format!("  {branch}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(number) = wt_num {
        spans.push(Span::styled(
            format!("  [{number}]"),
            Style::default().fg(Color::Cyan),
        ));
    }
    Line::from(spans)
}

fn draw_task_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(Span::styled(
        "+ Create new task",
        Style::default().fg(Color::Green),
    )))];

    for (_, task) in app.active_tasks() {
        items.push(ListItem::new(format_task_row(
            &task.title,
            task.branch.as_deref(),
            task.worktree.as_ref().map(|wt| wt.number),
        )));
    }

    let list = List::new(items)
        .block(Block::default().title("Tasks").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_archive_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .archived_tasks()
        .map(|(_, task)| {
            ListItem::new(format_task_row(
                &task.title,
                task.branch.as_deref(),
                task.worktree.as_ref().map(|wt| wt.number),
            ))
        })
        .collect();

    let title = if items.is_empty() {
        "Archive (empty)"
    } else {
        "Archive"
    };

    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.archive_state);
}

fn draw_edit(frame: &mut Frame, area: Rect, app: &App) {
    let Some(edit) = app.edit.as_ref() else {
        frame.render_widget(
            Paragraph::new("No task selected").block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    };
    let Some(task) = app.tasks.get(edit.task_idx) else {
        frame.render_widget(
            Paragraph::new("Task missing").block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    };

    let [fields, modules, readonly] = Layout::vertical([
        Constraint::Length(7),
        Constraint::Fill(1),
        Constraint::Length(4),
    ])
    .areas(area);

    let focus_style = |focused: bool| {
        if focused {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };

    let title_line = Line::from(vec![
        Span::styled("Title:    ", focus_style(edit.focus == EditFocus::Title)),
        Span::raw(task.title.clone()),
        if edit.focus == EditFocus::Title {
            Span::styled("▌", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
    ]);
    let branch_val = task.branch.as_deref().unwrap_or("");
    let branch_line = Line::from(vec![
        Span::styled("Branch:   ", focus_style(edit.focus == EditFocus::Branch)),
        Span::raw(branch_val.to_string()),
        if edit.focus == EditFocus::Branch {
            Span::styled("▌", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
    ]);
    let issue_val = task.issue_id.as_deref().unwrap_or("");
    let issue_line = Line::from(vec![
        Span::styled("Issue ID: ", focus_style(edit.focus == EditFocus::IssueId)),
        Span::raw(issue_val.to_string()),
        if edit.focus == EditFocus::IssueId {
            Span::styled("▌", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
    ]);

    let fields_widget = Paragraph::new(vec![
        title_line,
        Line::from(""),
        branch_line,
        Line::from(""),
        issue_line,
    ])
    .block(Block::default().title("Editable").borders(Borders::ALL));
    frame.render_widget(fields_widget, fields);

    let module_items: Vec<ListItem> = edit
        .available_modules
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let selected = task.modules.iter().any(|m| m == name);
            let mark = if selected { "[x]" } else { "[ ]" };
            let cursor = if edit.focus == EditFocus::Modules && i == edit.module_cursor {
                "> "
            } else {
                "  "
            };
            let style = if edit.focus == EditFocus::Modules && i == edit.module_cursor {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{cursor}{mark} {name}"),
                style,
            )))
        })
        .collect();

    let modules_title = if edit.focus == EditFocus::Modules {
        "Modules (focused)"
    } else {
        "Modules"
    };
    let modules_list = List::new(module_items).block(
        Block::default()
            .title(modules_title)
            .borders(Borders::ALL)
            .border_style(focus_style(edit.focus == EditFocus::Modules)),
    );
    frame.render_widget(modules_list, modules);

    let wt_text = match &task.worktree {
        Some(wt) => format!("Worktree: {} ({})", wt.number, wt.path.display()),
        None => "Worktree: (none)".to_string(),
    };
    let readonly_widget = Paragraph::new(vec![Line::from(Span::styled(
        wt_text,
        Style::default().fg(Color::DarkGray),
    ))])
    .block(Block::default().title("Read-only").borders(Borders::ALL));
    frame.render_widget(readonly_widget, readonly);
}

fn draw_create_prompt(frame: &mut Frame, area: Rect, app: &App) {
    let body = Paragraph::new(vec![
        Line::from("Enter a title, branch (prefix/name), or issue ID (ABC-123):"),
        Line::from(""),
        Line::from(vec![
            Span::raw("> "),
            Span::raw(app.create_input.clone()),
            Span::styled("▌", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Issue IDs look up Linear (may prompt for API key). Esc cancels.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .title("Create new task")
            .borders(Borders::ALL),
    );
    frame.render_widget(body, area);
}

fn draw_credential_prompt(frame: &mut Frame, area: Rect, app: &App) {
    let masked: String = "*".repeat(app.credential_input.chars().count());
    let body = Paragraph::new(vec![
        Line::from("Linear API key not found in the OS keyring."),
        Line::from("Paste your key below; it will be stored as service `tod`, account `linear`."),
        Line::from(""),
        Line::from(vec![
            Span::raw("> "),
            Span::raw(masked),
            Span::styled("▌", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter saves and continues create. Esc cancels.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .title("Linear credentials")
            .borders(Borders::ALL),
    );
    frame.render_widget(body, area);
}

fn draw_switch_modules(frame: &mut Frame, area: Rect, app: &App) {
    let Some(prep) = app.switch_prep.as_ref() else {
        frame.render_widget(
            Paragraph::new("No switch in progress").block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    };
    let Some(task) = app.tasks.get(prep.task_idx) else {
        frame.render_widget(
            Paragraph::new("Task missing").block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    };

    let [intro, modules] =
        Layout::vertical([Constraint::Length(4), Constraint::Fill(1)]).areas(area);

    let intro_widget = Paragraph::new(vec![
        Line::from(format!("Task: {}", task.title)),
        Line::from(Span::styled(
            "Select which modules use the task branch (Space toggles).",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .title("Switch — modules")
            .borders(Borders::ALL),
    );
    frame.render_widget(intro_widget, intro);

    let module_items: Vec<ListItem> = prep
        .available_modules
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let selected = task.modules.iter().any(|m| m == name);
            let mark = if selected { "[x]" } else { "[ ]" };
            let cursor = if i == prep.module_cursor { "> " } else { "  " };
            let style = if i == prep.module_cursor {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{cursor}{mark} {name}"),
                style,
            )))
        })
        .collect();

    let modules_list = List::new(module_items).block(
        Block::default()
            .title("Modules")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(modules_list, modules);
}

fn draw_switch_branch(frame: &mut Frame, area: Rect, app: &App) {
    let (title, input) = match app.switch_prep.as_ref() {
        Some(prep) => {
            let title = app
                .tasks
                .get(prep.task_idx)
                .map(|t| t.title.as_str())
                .unwrap_or("(missing)");
            (title.to_string(), prep.branch_input.clone())
        }
        None => ("(none)".to_string(), String::new()),
    };

    let body = Paragraph::new(vec![
        Line::from(format!("Task: {title}")),
        Line::from("Enter the git branch name for this task:"),
        Line::from(""),
        Line::from(vec![
            Span::raw("> "),
            Span::raw(input),
            Span::styled("▌", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter continues switch (lease / activate / Cursor). Esc cancels.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .title("Switch — branch")
            .borders(Borders::ALL),
    );
    frame.render_widget(body, area);
}

fn draw_dirty_warning(frame: &mut Frame, area: Rect, app: &App) {
    let Some(rel) = app.release.as_ref() else {
        frame.render_widget(
            Paragraph::new("No release in progress").block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    };

    let task_title = app
        .tasks
        .get(rel.task_idx)
        .map(|t| t.title.as_str())
        .unwrap_or("(missing)");
    let context = if rel.then_archive {
        "Releasing worktree before archive"
    } else {
        "Releasing worktree"
    };

    let mut report_lines: Vec<Line> = vec![
        Line::from(format!("Task: {task_title}")),
        Line::from(Span::styled(context, Style::default().fg(Color::DarkGray))),
        Line::from(""),
    ];
    for line in dirty::format_report_lines(&rel.report) {
        let style = if line.starts_with('[') {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if line.contains("blocked") {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        report_lines.push(Line::from(Span::styled(line, style)));
    }

    let action_items: Vec<ListItem> = rel
        .actions
        .iter()
        .enumerate()
        .map(|(i, action)| {
            let cursor = if i == rel.action_cursor { "> " } else { "  " };
            let shortcut = action.shortcut().to_ascii_uppercase();
            let style = if i == rel.action_cursor {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{cursor}[{shortcut}] {}", action.label()),
                style,
            )))
        })
        .collect();

    let [report_area, actions_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(6)]).areas(area);

    let report_widget = Paragraph::new(report_lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title("Dirty worktree warning")
                .borders(Borders::ALL),
        );
    frame.render_widget(report_widget, report_area);

    let actions_list = List::new(action_items).block(
        Block::default()
            .title("Options")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(actions_list, actions_area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let help = match app.view {
        View::TaskList => {
            let sel = match app.list_state.selected() {
                Some(0) => "Create new task".to_string(),
                Some(_) => app
                    .selected_list_task()
                    .map(|t| format!("Selected: {}", t.title))
                    .unwrap_or_default(),
                None => "No selection".to_string(),
            };
            format!(
                "{sel}  |  ↑/↓ move  Enter open  E edit  R release  A archive  Shift+A archive view  Q quit"
            )
        }
        View::Archive => {
            let sel = app
                .selected_archive_task()
                .map(|t| format!("Selected: {}", t.title))
                .unwrap_or_else(|| "Archive empty".to_string());
            format!("{sel}  |  ↑/↓ move  U unarchive  Esc back  Q quit")
        }
        View::Edit => {
            "Tab/↑/↓ fields  Space toggle module  Enter confirm field  Esc back  Q quit".to_string()
        }
        View::CreatePrompt => "Type input  Enter create  Esc cancel".to_string(),
        View::CredentialPrompt => "Type API key  Enter save  Esc cancel".to_string(),
        View::SwitchModules => {
            "↑/↓ move  Space toggle  Enter confirm  Esc cancel  Q quit".to_string()
        }
        View::SwitchBranch => "Type branch  Enter continue  Esc cancel".to_string(),
        View::DirtyWarning => {
            "↑/↓ move  Enter choose  C check again  S stash  X/Esc cancel  Q quit".to_string()
        }
    };

    let text = if let Some(status) = &app.status {
        format!("{status}  ‖  {help}")
    } else {
        help
    };

    let footer = Paragraph::new(text)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}
