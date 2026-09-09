//! Board-first orchestration campaign page.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ui::page::session_chat::{SessionChatPage, SessionChatPageInput};
use crate::ui::{Page, style};

/// Controller page that keeps campaign state above a compact conversation
/// pane.
pub struct OrchestrationPage<'a> {
    can_open_worktree: bool,
    chat_input: SessionChatPageInput<'a>,
}

impl<'a> OrchestrationPage<'a> {
    /// Creates a campaign page from the same immutable inputs used by the
    /// controller chat pane.
    pub fn new(chat_input: SessionChatPageInput<'a>) -> Self {
        Self {
            can_open_worktree: false,
            chat_input,
        }
    }

    /// Sets whether the controller worktree can be opened.
    #[must_use]
    pub fn can_open_worktree(mut self, can_open_worktree: bool) -> Self {
        self.can_open_worktree = can_open_worktree;

        self
    }

    fn render_board(&self, frame: &mut Frame, area: Rect) {
        let session = self.chat_input.sessions.get(self.chat_input.session_index);
        let progress = session
            .and_then(|session| session.orchestration_progress.as_deref())
            .unwrap_or("Phase: Planning\nDiscuss the goal with the controller to produce a plan.");
        let mut lines = progress
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let style = if index == 0 {
                    Style::default()
                        .fg(style::palette::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(style::palette::text())
                };

                Line::from(Span::styled(line.to_string(), style))
            })
            .collect::<Vec<_>>();
        if progress.contains("AwaitingApproval") || progress == "Awaiting approval" {
            lines.push(Line::from(Span::styled(
                "a approve  Enter discuss/revise",
                Style::default().fg(style::palette::text_muted()),
            )));
        } else if progress.contains("AwaitingIntegration") {
            lines.push(Line::from(Span::styled(
                "a approve integration",
                Style::default().fg(style::palette::text_muted()),
            )));
        }
        let title = session.map_or("Orchestration Campaign", |session| session.display_title());
        let board = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(style::palette::border()))
                    .title(format!(" Campaign: {title} ")),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(board, area);
    }
}

impl Page for OrchestrationPage<'_> {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let progress = self
            .chat_input
            .sessions
            .get(self.chat_input.session_index)
            .and_then(|session| session.orchestration_progress.as_deref());
        let [board_area, chat_area] = campaign_page_areas(area, progress);

        self.render_board(frame, board_area);
        SessionChatPage::new(self.chat_input)
            .can_open_worktree(self.can_open_worktree)
            .render(frame, chat_area);
    }
}

/// Splits a controller page into its campaign board and chat areas.
///
/// Runtime scroll metrics use the same chat area so line-step bounds match
/// the compact transcript viewport painted below the campaign board.
pub(crate) fn campaign_page_areas(area: Rect, progress: Option<&str>) -> [Rect; 2] {
    let board_height = campaign_board_height(progress);

    Layout::vertical([Constraint::Length(board_height), Constraint::Min(8)]).areas(area)
}

/// Calculates the bounded campaign board height from the current snapshot.
fn campaign_board_height(progress: Option<&str>) -> u16 {
    let progress_lines = progress.map_or(1, |progress| progress.lines().count());

    u16::try_from(progress_lines.saturating_add(4))
        .unwrap_or(12)
        .clamp(6, 12)
}

#[cfg(test)]
#[path = "orchestration_test.rs"]
mod tests;
