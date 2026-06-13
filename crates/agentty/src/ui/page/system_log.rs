//! Process-local system log page rendering.

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use time::{OffsetDateTime, UtcOffset};

use crate::domain::system_log::{
    SYSTEM_LOG_LIMIT, SystemLogBuffer, SystemLogEntry, SystemLogLevel,
};
use crate::ui::state::help_action;
use crate::ui::text_util::inline_text;
use crate::ui::{Page, layout, style};

/// Renders the process-local system log page.
pub struct SystemLogPage<'a> {
    logs: &'a SystemLogBuffer,
    tail_offset: u16,
}

impl<'a> SystemLogPage<'a> {
    /// Creates a page renderer for the process-local log buffer.
    pub fn new(logs: &'a SystemLogBuffer, tail_offset: u16) -> Self {
        Self { logs, tail_offset }
    }
}

impl Page for SystemLogPage<'_> {
    /// Renders retained system log entries with severity and source
    /// highlighting plus tail-oriented scroll state.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);
        let block = system_log_block(self.logs.len());
        let inner_height = block.inner(areas.main_area).height;
        let lines = system_log_lines(self.logs.entries());
        let scroll_offset = top_scroll_offset(lines.len(), inner_height, self.tail_offset);
        let paragraph = Paragraph::new(lines)
            .block(block)
            .scroll((scroll_offset, 0))
            .style(Style::default().fg(style::palette::text()));

        f.render_widget(paragraph, areas.main_area);
        f.render_widget(Paragraph::new(system_log_footer_line()), areas.footer_area);
    }
}

/// Builds the bordered system log panel title.
fn system_log_block(log_count: usize) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(format!("Logs ({log_count}/{SYSTEM_LOG_LIMIT})"))
        .border_style(style::border_style())
}

/// Converts retained log entries into styled render lines.
fn system_log_lines(entries: &VecDeque<SystemLogEntry>) -> Vec<Line<'static>> {
    if entries.is_empty() {
        return vec![Line::styled(
            "No system logs yet.",
            Style::default().fg(style::palette::text_muted()),
        )];
    }

    entries.iter().map(system_log_line).collect()
}

/// Converts one system log entry into a highlighted line.
fn system_log_line(entry: &SystemLogEntry) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{} ", timestamp_label(entry.timestamp_unix_seconds)),
            Style::default().fg(style::palette::text_subtle()),
        ),
        Span::styled(
            format!("{:<4} ", entry.level.label()),
            level_style(entry.level).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<7} ", entry.category.label()),
            Style::default().fg(style::palette::accent_soft()),
        ),
        Span::styled(
            inline_text(&entry.message),
            Style::default().fg(style::palette::text()),
        ),
    ];

    if let Some(detail) = entry.detail.as_ref().filter(|detail| !detail.is_empty()) {
        spans.push(Span::styled(
            "  ",
            Style::default().fg(style::palette::text_subtle()),
        ));
        spans.push(Span::styled(
            inline_text(detail),
            Style::default().fg(style::palette::text_muted()),
        ));
    }

    Line::from(spans)
}

/// Returns the style used for a log severity badge.
fn level_style(level: SystemLogLevel) -> Style {
    match level {
        SystemLogLevel::Info => Style::default().fg(style::palette::info()),
        SystemLogLevel::Success => Style::default().fg(style::palette::success()),
        SystemLogLevel::Warning => Style::default().fg(style::palette::warning()),
        SystemLogLevel::Error => Style::default().fg(style::palette::danger()),
    }
}

/// Returns a local `HH:MM:SS` timestamp label for one Unix timestamp.
fn timestamp_label(timestamp_unix_seconds: i64) -> String {
    let Ok(utc_time) = OffsetDateTime::from_unix_timestamp(timestamp_unix_seconds) else {
        return "--:--:--".to_string();
    };
    let display_time =
        UtcOffset::local_offset_at(utc_time).map_or(utc_time, |offset| utc_time.to_offset(offset));

    format!(
        "{:02}:{:02}:{:02}",
        display_time.hour(),
        display_time.minute(),
        display_time.second()
    )
}

/// Converts a tail-relative log offset into Ratatui's top-relative scroll
/// offset for the rendered line list.
fn top_scroll_offset(line_count: usize, viewport_height: u16, tail_offset: u16) -> u16 {
    let hidden_line_count = line_count.saturating_sub(usize::from(viewport_height));
    let tail_offset = usize::from(tail_offset).min(hidden_line_count);
    let top_offset = hidden_line_count.saturating_sub(tail_offset);

    u16::try_from(top_offset).unwrap_or(u16::MAX)
}

/// Builds the footer help content for the logs page.
fn system_log_footer_line() -> Line<'static> {
    let mut line = help_action::footer_line(&help_action::system_log_footer_actions());
    line.spans.push(help_action::footer_separator_span());
    line.spans.push(help_action::footer_muted_span(format!(
        "keeps last {SYSTEM_LOG_LIMIT} entries in memory"
    )));

    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::system_log::{SystemLogCategory, SystemLogEvent};

    /// Verifies the default tail view starts at the newest render lines.
    #[test]
    fn top_scroll_offset_tails_newest_lines_by_default() {
        // Arrange
        let line_count = 20;
        let viewport_height = 5;
        let tail_offset = 0;

        // Act
        let scroll_offset = top_scroll_offset(line_count, viewport_height, tail_offset);

        // Assert
        assert_eq!(scroll_offset, 15);
    }

    /// Verifies a large tail offset clamps to the oldest retained line.
    #[test]
    fn top_scroll_offset_clamps_to_oldest_lines() {
        // Arrange
        let line_count = 20;
        let viewport_height = 5;
        let tail_offset = u16::MAX;

        // Act
        let scroll_offset = top_scroll_offset(line_count, viewport_height, tail_offset);

        // Assert
        assert_eq!(scroll_offset, 0);
    }

    /// Verifies warning entries render with the warning severity style.
    #[test]
    fn system_log_line_highlights_warning_level() {
        // Arrange
        let mut buffer = SystemLogBuffer::default();
        buffer.push(
            0,
            SystemLogEvent::new(
                SystemLogLevel::Warning,
                SystemLogCategory::Forge,
                "Review sync failed",
            )
            .with_detail("network unavailable"),
        );
        let entry = buffer
            .entries()
            .front()
            .expect("expected log entry for render test");

        // Act
        let line = system_log_line(entry);

        // Assert
        assert_eq!(line.spans[1].content.as_ref(), "WARN ");
        assert_eq!(line.spans[1].style.fg, Some(style::palette::warning()));
        assert!(line.to_string().contains("network unavailable"));
    }

    /// Verifies invalid timestamps render as an unknown time placeholder.
    #[test]
    fn timestamp_label_returns_unknown_for_invalid_timestamp() {
        // Arrange
        let timestamp = i64::MAX;

        // Act
        let label = timestamp_label(timestamp);

        // Assert
        assert_eq!(label, "--:--:--");
    }
}
