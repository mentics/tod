use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::app::App;
use crate::task::TaskStatus;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    draw_header(frame, header);
    draw_task_list(frame, body, app);
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("taskstui", Style::default().bold().fg(Color::Cyan)),
        Span::raw(" — task manager"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, area);
}

fn draw_task_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|task| {
            let status_style = match task.status {
                TaskStatus::Active => Style::default().fg(Color::Green),
                TaskStatus::Paused => Style::default().fg(Color::Yellow),
                TaskStatus::Done => Style::default().fg(Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{}] ", task.status.label()), status_style),
                Span::raw(&task.name),
                Span::styled(
                    format!("  {}", task.path),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

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

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let help = match app.selected_task() {
        Some(task) => format!(
            "Selected: {} ({})  |  j/k or ↑/↓ move  |  q quit",
            task.name,
            task.status.label()
        ),
        None => "No tasks  |  q quit".to_string(),
    };

    let footer = Paragraph::new(help)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}
