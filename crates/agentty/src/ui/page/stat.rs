use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::domain::agent::AgentKind;
use crate::domain::agent_usage::{
    AgentRateLimit, AgentUsageDetails, AgentUsageSnapshot, AgentUsageStatus,
};
use crate::domain::session::Session;
use crate::ui::page::session_list::{model_column_width, project_column_width};
use crate::ui::state::help_action;
use crate::ui::util::{format_token_count, inline_text};
use crate::ui::{Page, layout, style};

/// Fixed height reserved for provider usage on the Stats page.
const USAGE_PANEL_HEIGHT: u16 = 11;
/// Horizontal spacing between token-stat table columns.
const TABLE_COLUMN_SPACING: u16 = 2;

/// Stats dashboard showing provider usage and per-session token statistics.
pub struct StatsPage<'a> {
    agent_usage_snapshot: &'a AgentUsageSnapshot,
    sessions: &'a [Session],
}

impl<'a> StatsPage<'a> {
    /// Creates a stats page renderer from live sessions and provider usage
    /// state.
    pub fn new(sessions: &'a [Session], agent_usage_snapshot: &'a AgentUsageSnapshot) -> Self {
        Self {
            agent_usage_snapshot,
            sessions,
        }
    }
}

impl Page for StatsPage<'_> {
    /// Renders the dashboard with provider usage, token table, footer, and
    /// compact tab-page spacing.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(USAGE_PANEL_HEIGHT), Constraint::Min(0)])
            .split(areas.main_area);

        self.render_agent_usage(f, main_chunks[0]);
        self.render_table(f, main_chunks[1]);
        self.render_footer(f, areas.footer_area);
    }
}

impl StatsPage<'_> {
    /// Renders provider subscription and quota usage above token totals.
    fn render_agent_usage(&self, f: &mut Frame, area: Rect) {
        let usage = Paragraph::new(self.build_agent_usage_lines()).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Subscription Usage")
                .border_style(style::border_style()),
        );

        f.render_widget(usage, area);
    }

    /// Renders per-session token statistics using the shared table palette.
    fn render_table(&self, f: &mut Frame, area: Rect) {
        let header_style = Style::default()
            .bg(style::palette::surface())
            .fg(style::palette::text_muted())
            .add_modifier(Modifier::BOLD);
        let header_cells = ["Session", "Project", "Model", "Input", "Output"]
            .iter()
            .map(|header| Cell::from(*header));
        let header = Row::new(header_cells)
            .style(header_style)
            .height(1)
            .bottom_margin(1);

        let rows = self.sessions.iter().map(|session| {
            let cells = vec![
                Cell::from(inline_text(session.display_title())),
                Cell::from(session.project_name.clone()),
                Cell::from(session.model.as_str()),
                Cell::from(format_token_count(session.stats.input_tokens)),
                Cell::from(format_token_count(session.stats.output_tokens)),
            ];

            Row::new(cells)
                .style(Style::default().fg(style::palette::text()))
                .height(1)
        });

        let table = Table::new(
            rows,
            [
                Constraint::Min(0),
                project_column_width(self.sessions),
                model_column_width(self.sessions),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Token Stats")
                .border_style(style::border_style()),
        );

        f.render_widget(table, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let footer_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Min(0)])
            .split(area);

        let help = Paragraph::new(help_action::footer_line(
            &help_action::stats_footer_actions(),
        ));
        f.render_widget(help, footer_chunks[0]);

        let total_input: u64 = self
            .sessions
            .iter()
            .map(|session| session.stats.input_tokens)
            .sum();
        let total_output: u64 = self
            .sessions
            .iter()
            .map(|session| session.stats.output_tokens)
            .sum();

        let input_display = format_token_count(total_input);
        let output_display = format_token_count(total_output);
        let summary = format!(
            "Sessions: {} | Input: {} | Output: {}",
            self.sessions.len(),
            input_display,
            output_display
        );
        let stats = Paragraph::new(summary)
            .style(Style::default().fg(style::palette::text_muted()))
            .alignment(Alignment::Right);
        f.render_widget(stats, footer_chunks[1]);
    }

    /// Builds provider usage lines for the subscription panel.
    fn build_agent_usage_lines(&self) -> Vec<Line<'static>> {
        if self.agent_usage_snapshot.rows.is_empty() {
            return vec![Line::from(Span::styled(
                "Loading provider usage...",
                Style::default().fg(style::palette::text_muted()),
            ))];
        }

        let mut lines = Vec::new();
        for row in &self.agent_usage_snapshot.rows {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }

            lines.push(Line::from(Span::styled(
                agent_kind_label(row.kind),
                Style::default()
                    .fg(style::palette::text())
                    .add_modifier(Modifier::BOLD),
            )));
            lines.extend(agent_usage_status_lines(&row.status));
        }

        lines
    }
}

/// Returns the display label for one provider family.
fn agent_kind_label(agent_kind: AgentKind) -> &'static str {
    match agent_kind {
        AgentKind::Gemini => "Gemini",
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
    }
}

/// Converts one provider usage status into muted detail lines.
fn agent_usage_status_lines(status: &AgentUsageStatus) -> Vec<Line<'static>> {
    match status {
        AgentUsageStatus::Available(details) => agent_usage_details_lines(details),
        AgentUsageStatus::MissingCli => vec![muted_line("CLI not found on PATH")],
        AgentUsageStatus::Unavailable { message }
        | AgentUsageStatus::NotImplemented { message } => {
            vec![muted_line(message.clone())]
        }
        AgentUsageStatus::Error { message } => vec![error_line(message.clone())],
    }
}

/// Converts structured usage data into compact detail lines.
fn agent_usage_details_lines(details: &AgentUsageDetails) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let plan = details.plan.as_deref().unwrap_or("Unknown");
    lines.push(muted_line(format!("Plan: {plan}")));

    if details.rate_limits.is_empty() {
        lines.push(muted_line("Usage: not reported"));
    } else {
        lines.extend(details.rate_limits.iter().map(format_rate_limit_line));
    }

    if let Some(reached_type) = &details.reached_type {
        lines.push(error_line(format!("Limit reached: {reached_type}")));
    }

    lines
}

/// Formats one rate-limit bucket for display.
fn format_rate_limit_line(rate_limit: &AgentRateLimit) -> Line<'static> {
    let used_percent = rate_limit.used_percent.as_deref().unwrap_or("unknown");
    let reset = rate_limit.resets_at_unix_seconds.map_or_else(
        || "reset unknown".to_string(),
        |timestamp| format_reset_deadline(timestamp, current_unix_timestamp_seconds()),
    );
    let window = rate_limit.window_duration_mins.map_or_else(
        || "window unknown".to_string(),
        |minutes| format!("{minutes}m window"),
    );

    muted_line(format!(
        "{}: {} used, {}, {}",
        rate_limit.label, used_percent, reset, window
    ))
}

/// Formats a quota reset timestamp as a compact relative deadline.
fn format_reset_deadline(reset_timestamp_seconds: i64, now_timestamp_seconds: i64) -> String {
    let seconds_until_reset = reset_timestamp_seconds.saturating_sub(now_timestamp_seconds);
    if seconds_until_reset <= 0 {
        return "resets now".to_string();
    }

    format!(
        "resets in {}",
        format_relative_duration(seconds_until_reset)
    )
}

/// Formats a positive duration for provider quota reset deadlines.
fn format_relative_duration(seconds: i64) -> String {
    let total_minutes = seconds.saturating_add(59) / 60;
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes % (24 * 60)) / 60;
    let minutes = total_minutes % 60;

    if days > 0 {
        if hours > 0 {
            return format!("{days}d {hours}h");
        }

        return format!("{days}d");
    }

    if hours > 0 {
        if minutes > 0 {
            return format!("{hours}h {minutes}m");
        }

        return format!("{hours}h");
    }

    format!("{minutes}m")
}

/// Returns the current Unix timestamp in seconds for relative usage
/// deadlines.
fn current_unix_timestamp_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

/// Builds one muted line for secondary panel details.
fn muted_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(style::palette::text_muted()),
    ))
}

/// Builds one error-colored line for provider usage failures.
fn error_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(style::palette::danger()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::SessionStats;
    use crate::domain::session::tests::SessionFixtureBuilder;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_token_stats_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }

    #[test]
    fn test_render_uses_palette_border_for_stats_panels() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let sessions = vec![session_fixture()];
        let usage_snapshot = AgentUsageSnapshot::default();
        let mut page = StatsPage::new(&sessions, &usage_snapshot);
        let backend = ratatui::backend::TestBackend::new(160, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw stats page");

        // Assert
        let border_cell_count = foreground_symbol_cell_count(terminal.backend().buffer(), "┌");
        assert!(
            border_cell_count >= 2,
            "expected subscription usage and token table panels to use palette border color"
        );
    }

    fn session_fixture() -> Session {
        session_fixture_with(
            "session-id",
            "Stats Session",
            AgentModel::Gemini3FlashPreview,
            1_500,
            700,
            0,
            1_800,
        )
    }

    fn session_fixture_with(
        session_id: &str,
        title: &str,
        model: AgentModel,
        input_tokens: u64,
        output_tokens: u64,
        created_at: i64,
        updated_at: i64,
    ) -> Session {
        SessionFixtureBuilder::new()
            .id(session_id)
            .title(Some(title.to_string()))
            .model(model)
            .stats(SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                input_tokens,
                output_tokens,
            })
            .created_at(created_at)
            .updated_at(updated_at)
            .build()
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn find_text_start_cell<'a>(
        buffer: &'a ratatui::buffer::Buffer,
        needle: &str,
    ) -> Option<&'a ratatui::buffer::Cell> {
        let (cell_index, _) = find_text_start_position(buffer, needle)?;

        buffer.content().get(cell_index)
    }

    fn find_text_start_position(
        buffer: &ratatui::buffer::Buffer,
        needle: &str,
    ) -> Option<(usize, u16)> {
        let width = usize::from(buffer.area.width.max(1));
        let needle_symbols = needle
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        let content = buffer.content();

        for row_start in (0..content.len()).step_by(width) {
            let row_end = row_start + width.min(content.len().saturating_sub(row_start));
            let row = &content[row_start..row_end];

            for (index, window) in row.windows(needle_symbols.len()).enumerate() {
                let window_matches = window
                    .iter()
                    .zip(&needle_symbols)
                    .all(|(cell, symbol)| cell.symbol() == symbol);

                if window_matches {
                    let cell_index = row_start + index;
                    let x = u16::try_from(index).ok()?;

                    return Some((cell_index, x));
                }
            }
        }

        None
    }

    /// Counts cells matching a rendered symbol and the active palette border
    /// color.
    fn foreground_symbol_cell_count(buffer: &ratatui::buffer::Buffer, symbol: &str) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == symbol && cell.fg == style::palette::border())
            .count()
    }

    #[test]
    fn test_render_shows_subscription_usage_above_tokens() {
        // Arrange
        let sessions = vec![session_fixture()];
        let usage_snapshot = AgentUsageSnapshot::new(vec![
            crate::domain::agent_usage::AgentUsageRow::new(
                AgentKind::Codex,
                AgentUsageStatus::Available(AgentUsageDetails {
                    plan: Some("pro".to_string()),
                    rate_limits: vec![AgentRateLimit {
                        label: "Primary".to_string(),
                        used_percent: Some("25%".to_string()),
                        resets_at_unix_seconds: Some(1_730_947_200),
                        window_duration_mins: Some(15),
                    }],
                    reached_type: None,
                }),
            ),
            crate::domain::agent_usage::AgentUsageRow::new(
                AgentKind::Claude,
                AgentUsageStatus::NotImplemented {
                    message: "Claude placeholder".to_string(),
                },
            ),
        ]);
        let mut page = StatsPage::new(&sessions, &usage_snapshot);
        let backend = ratatui::backend::TestBackend::new(180, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw stats page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Subscription Usage"));
        assert!(text.contains("Codex"));
        assert!(text.contains("Plan: pro"));
        assert!(text.contains("Primary: 25% used"));
        assert!(text.contains("resets"));
        assert!(!text.contains("1730947200"));
        assert!(text.contains("Claude placeholder"));
    }

    #[test]
    fn test_format_reset_deadline_uses_relative_duration() {
        // Arrange
        let now = 1_000;
        let reset_at = now + 3_900;

        // Act
        let reset = format_reset_deadline(reset_at, now);

        // Assert
        assert_eq!(reset, "resets in 1h 5m");
    }

    #[test]
    fn test_render_shows_session_token_table() {
        // Arrange
        let sessions = vec![
            session_fixture_with(
                "session-1",
                "Longest Session",
                AgentModel::Gpt54,
                1_000,
                500,
                0,
                7_200,
            ),
            session_fixture_with(
                "session-2",
                "Second Session",
                AgentModel::Gpt54,
                2_000,
                700,
                10,
                20,
            ),
            session_fixture_with(
                "session-3",
                "Claude Session",
                AgentModel::ClaudeOpus47,
                300,
                200,
                30,
                90,
            ),
        ];
        let usage_snapshot = AgentUsageSnapshot::default();
        let mut page = StatsPage::new(&sessions, &usage_snapshot);
        let backend = ratatui::backend::TestBackend::new(220, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw stats page");

        // Assert
        let buffer = terminal.backend().buffer();
        let fallback_cell = &buffer.content()[0];
        let title_cell = find_text_start_cell(buffer, "Longest Session").unwrap_or(fallback_cell);
        let text = buffer_text(buffer);
        assert!(text.contains("Token Stats"));
        assert!(text.contains("gpt-5.4"));
        assert!(text.contains("claude-opus-4-7"));
        assert_eq!(title_cell.fg, style::palette::text());
    }

    #[test]
    fn test_render_does_not_show_session_summary_panel() {
        // Arrange
        let sessions = vec![session_fixture()];
        let usage_snapshot = AgentUsageSnapshot::default();
        let mut page = StatsPage::new(&sessions, &usage_snapshot);
        let backend = ratatui::backend::TestBackend::new(220, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw stats page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("Session Stats"));
        assert!(!text.contains("Favorite model:"));
        assert!(!text.contains("Longest Agentty session:"));
        assert!(!text.contains("Model stats (All time)"));
    }
}
