use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use tachyonfx::EffectManager;

use crate::agent::AgentKind;
use crate::icon::Icon;
use crate::model::{AppMode, PromptSlashStage, Session, Status};
use crate::ui::components::chat_input::{ChatInput, SlashMenu};
use crate::ui::components::eye::Eye;
use crate::ui::util::{calculate_input_height, compute_input_layout, wrap_lines};
use crate::ui::{Component, Page};

pub struct SessionChatPage<'a> {
    pub effect_manager: &'a mut EffectManager<&'static str>,
    pub frame_delta: Duration,
    pub mode: &'a AppMode,
    pub scroll_offset: Option<u16>,
    pub session_index: usize,
    pub sessions: &'a [Session],
}

impl<'a> SessionChatPage<'a> {
    pub fn new(
        sessions: &'a [Session],
        session_index: usize,
        scroll_offset: Option<u16>,
        mode: &'a AppMode,
        effect_manager: &'a mut EffectManager<&'static str>,
        frame_delta: Duration,
    ) -> Self {
        Self {
            effect_manager,
            frame_delta,
            mode,
            scroll_offset,
            session_index,
            sessions,
        }
    }

    fn build_slash_menu(
        input: &str,
        stage: PromptSlashStage,
        selected_agent: Option<AgentKind>,
        session: &Session,
    ) -> Option<SlashMenu<'static>> {
        if !input.starts_with('/') {
            return None;
        }

        let (title, options): (&'static str, Vec<String>) = match stage {
            PromptSlashStage::Command => {
                let lowered = input.to_lowercase();
                let commands = ["/model"]
                    .iter()
                    .copied()
                    .filter(|command| command.starts_with(&lowered))
                    .map(str::to_string)
                    .collect::<Vec<_>>();

                ("", commands)
            }
            PromptSlashStage::Agent => (
                "/model > agent",
                AgentKind::ALL
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            ),
            PromptSlashStage::Model => {
                let session_agent = selected_agent.unwrap_or_else(|| {
                    session
                        .agent
                        .parse::<AgentKind>()
                        .unwrap_or(AgentKind::Gemini)
                });
                let models = session_agent
                    .models()
                    .iter()
                    .map(|model| model.as_str().to_string())
                    .collect::<Vec<_>>();

                ("/model > model", models)
            }
        };
        if options.is_empty() {
            return None;
        }

        Some(SlashMenu {
            options,
            selected_index: 0,
            title,
        })
    }

    fn render_session(&mut self, f: &mut Frame, area: Rect, session: &Session) {
        let bottom_height = self.bottom_height(area, session);
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
            .margin(1)
            .split(area);

        self.render_output_panel(f, chunks[0], session);
        self.render_bottom_panel(f, chunks[1], session);
    }

    fn bottom_height(&self, area: Rect, session: &Session) -> u16 {
        if let AppMode::Prompt {
            input, slash_state, ..
        } = self.mode
        {
            let slash_option_count = Self::build_slash_menu(
                input.text(),
                slash_state.stage,
                slash_state.selected_agent,
                session,
            )
            .map_or(0, |menu| {
                menu.options.len() + usize::from(!menu.title.is_empty())
            });

            return calculate_input_height(area.width.saturating_sub(2), input.text())
                .saturating_add(u16::try_from(slash_option_count).unwrap_or(u16::MAX));
        }

        1
    }

    fn render_output_panel(&mut self, f: &mut Frame, output_area: Rect, session: &Session) {
        let status = session.status();
        let commit_count = session.commit_count();
        let commits_label = if commit_count == 1 {
            "commit"
        } else {
            "commits"
        };
        let title = format!(
            " {} — {status} - {commit_count} {commits_label} [{}:{}] ",
            session.display_title(),
            session.agent,
            session.model
        );
        let lines = Self::output_lines(session, output_area, status);
        let final_scroll = self.final_scroll_offset(output_area, lines.len());

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(status.color()))
                    .title(Span::styled(title, Style::default().fg(status.color()))),
            )
            .scroll((final_scroll, 0));

        f.render_widget(paragraph, output_area);

        if Self::should_show_eye(session, status) {
            let inner_area = output_area.inner(ratatui::layout::Margin::new(1, 1));
            let pupil_offset = self.pupil_offset(output_area.width);
            Eye::new(pupil_offset).render(f, inner_area);

            self.effect_manager.process_effects(
                self.frame_delta.into(),
                f.buffer_mut(),
                inner_area,
            );
        }
    }

    /// Returns `true` when the eye should be rendered in the output panel.
    fn should_show_eye(session: &Session, status: Status) -> bool {
        status == Status::New
            && session
                .output
                .lock()
                .map_or(true, |buffer| buffer.is_empty())
    }

    /// Computes pupil offset `[-1.0, 1.0]` from the cursor's visual X
    /// position within the panel.
    fn pupil_offset(&self, panel_width: u16) -> f64 {
        if let AppMode::Prompt { input, .. } = self.mode {
            let inner_width = panel_width.saturating_sub(2);
            if inner_width == 0 {
                return 0.0;
            }

            let visual_cursor_x = if input.text().is_empty() {
                // Empty input: cursor sits right after the " › " prefix.
                3
            } else {
                let (_lines, cursor_x, _cursor_y) =
                    compute_input_layout(input.text(), panel_width, input.cursor);
                cursor_x
            };

            // Map visual X [0, inner_width] to [-1.0, 1.0].
            let ratio = f64::from(visual_cursor_x) / f64::from(inner_width);

            return ratio * 2.0 - 1.0;
        }

        0.0
    }

    fn output_lines(session: &Session, output_area: Rect, status: Status) -> Vec<Line<'static>> {
        let output_text = session
            .output
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default();
        let inner_width = output_area.width.saturating_sub(2) as usize;
        let mut lines = wrap_lines(&output_text, inner_width)
            .into_iter()
            .map(|line| Line::from(line.to_string()))
            .collect::<Vec<_>>();

        if matches!(
            status,
            Status::InProgress | Status::PullRequest | Status::CreatingPullRequest
        ) {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "{} {}",
                    Icon::current_spinner(),
                    Self::status_message(status)
                ),
                Style::default().fg(status.color()),
            )]));
        }

        lines
    }

    fn status_message(status: Status) -> &'static str {
        match status {
            Status::InProgress => "Thinking...",
            Status::CreatingPullRequest => "Creating PR...",
            Status::PullRequest => "Waiting for PR merge...",
            Status::New | Status::Review | Status::Done | Status::Canceled => "",
        }
    }

    fn final_scroll_offset(&self, output_area: Rect, line_count: usize) -> u16 {
        if let Some(scroll_offset) = self.scroll_offset {
            return scroll_offset;
        }

        let inner_height = output_area.height.saturating_sub(2) as usize;

        u16::try_from(line_count.saturating_sub(inner_height)).unwrap_or(u16::MAX)
    }

    fn render_bottom_panel(&self, f: &mut Frame, bottom_area: Rect, session: &Session) {
        if let AppMode::Prompt {
            input, slash_state, ..
        } = self.mode
        {
            let title = if session.prompt.is_empty() {
                " New Chat "
            } else {
                " Reply "
            };
            let slash_menu = Self::build_slash_menu(
                input.text(),
                slash_state.stage,
                slash_state.selected_agent,
                session,
            )
            .map(|mut menu| {
                let max_index = menu.options.len().saturating_sub(1);
                menu.selected_index = slash_state.selected_index.min(max_index);
                menu
            });

            ChatInput::new(
                title,
                input.text(),
                input.cursor,
                "Type your message",
                slash_menu,
            )
            .render(f, bottom_area);

            return;
        }

        let help_message =
            Paragraph::new("q: back | enter: reply | d: diff | p: pr | m: merge | j/k: scroll")
                .style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, bottom_area);
    }
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            self.render_session(f, area, session);
        }
    }
}
