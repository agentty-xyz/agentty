use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};

use crate::model::{Agent, AppMode};

pub fn render(f: &mut Frame, mode: &AppMode, agents: &[Agent], table_state: &mut TableState) {
    let area = f.area();
    match mode {
        AppMode::List => {
            let rects = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .margin(2)
                .split(area);

            if agents.is_empty() {
                let vertical_chunks = Layout::default()
                    .constraints([
                        Constraint::Percentage(40),
                        Constraint::Length(5),
                        Constraint::Percentage(40),
                    ])
                    .split(rects[0]);

                let horizontal_chunks = Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(20),
                        Constraint::Percentage(60),
                        Constraint::Percentage(20),
                    ])
                    .split(vertical_chunks[1]);

                let hint = ratatui::widgets::Paragraph::new(vec![
                    ratatui::text::Line::from("Welcome to Agent Manager!"),
                    ratatui::text::Line::from(""),
                    ratatui::text::Line::from(ratatui::text::Span::styled(
                        "Press 'a' to initiate your first agent with a prompt",
                        Style::default().fg(Color::Cyan),
                    )),
                ])
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(" Getting Started "),
                );
                f.render_widget(hint, horizontal_chunks[1]);
            } else {
                let selected_style = Style::default().bg(Color::DarkGray);
                let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
                let header_cells = ["Agent Name", "Status"].iter().map(|h| Cell::from(*h));
                let header = Row::new(header_cells)
                    .style(normal_style)
                    .height(1)
                    .bottom_margin(1);
                let rows = agents.iter().map(|agent| {
                    let cells = vec![
                        Cell::from(agent.name.as_str()),
                        Cell::from(agent.status.icon())
                            .style(Style::default().fg(agent.status.color())),
                    ];
                    Row::new(cells).height(1)
                });
                let t = Table::new(
                    rows,
                    [Constraint::Percentage(50), Constraint::Percentage(50)],
                )
                .header(header)
                .block(Block::default().borders(Borders::ALL).title("Agents"))
                .row_highlight_style(selected_style)
                .highlight_symbol(">> ");

                f.render_stateful_widget(t, rects[0], table_state);
            }

            let help_message = ratatui::widgets::Paragraph::new(
                "Press 'a' to create agent with prompt, 'j'/'k' to navigate, 'q' to quit",
            )
            .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, rects[1]);
        }
        AppMode::Prompt { input } => {
            let rects = Layout::default()
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .margin(2)
                .split(area);

            let input_widget = ratatui::widgets::Paragraph::new(input.as_str())
                .block(Block::default().borders(Borders::ALL).title("Prompt"));
            f.render_widget(input_widget, rects[0]);

            let help_message = ratatui::widgets::Paragraph::new(
                "Enter the initial prompt for the agent. Press 'Enter' to confirm, 'Esc' to cancel",
            )
            .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, rects[2]);
        }
    }
}
