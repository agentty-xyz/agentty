//! Centered popup for choosing the type of session to create.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::style::palette;
use crate::ui::{Component, overlay, style};

/// Minimum popup height that leaves room for title, options, and hints
/// without adding unused vertical space.
const MIN_OVERLAY_HEIGHT: u16 = 10;
/// Minimum popup width sized for the longest option row plus shared overlay
/// chrome.
const MIN_OVERLAY_WIDTH: u16 = 45;
/// Fixed description width so option labels share one left edge.
const OPTION_DETAIL_WIDTH: usize = 27;
/// Fixed label width for the longest session-type label.
const OPTION_LABEL_WIDTH: usize = 7;
/// Detail text for every row when no agent can run a session.
const AGENT_CLI_REQUIRED_DETAIL: &str = "Install an agent CLI";
/// Popup dimensions for the compact session selector.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(30, 22, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);
/// Last selector row, which creates a draft session.
pub(crate) const LAST_OPTION_INDEX: usize = 1;

/// Centered popup used to choose the type of session to create.
pub struct SessionCreationOverlay {
    /// Whether an agent CLI or the in-process harness can run new sessions.
    can_start_sessions: bool,
    /// Currently highlighted option row in the selector.
    selected_option_index: usize,
}

impl SessionCreationOverlay {
    /// Creates a session creation selector with the provided highlighted row.
    pub fn new(selected_option_index: usize, can_start_sessions: bool) -> Self {
        Self {
            can_start_sessions,
            selected_option_index,
        }
    }

    /// Returns all render lines for this popup.
    ///
    /// The header and bottom help hint rows are centered, while option rows
    /// keep a fixed text width so labels align while the option block stays
    /// visually centered.
    fn lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::from(vec![Span::styled(
                "Select session type",
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center),
            Line::from(""),
            self.option_line(0, "Regular", "Start immediately"),
            self.option_line(LAST_OPTION_INDEX, "Draft", "Stage locally first"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "j/k: move | Enter: select | q: close",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        ]
    }

    /// Builds one option row and applies the active selection style, or a
    /// disabled row with an install hint when no agent can run sessions.
    fn option_line(
        &self,
        option_index: usize,
        label: &'static str,
        detail: &'static str,
    ) -> Line<'static> {
        if !self.can_start_sessions {
            let muted = Style::default().fg(palette::text_muted());

            return Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{label:<OPTION_LABEL_WIDTH$}"), muted),
                Span::styled("  ", Style::default().fg(palette::text_subtle())),
                Span::styled(
                    format!("{AGENT_CLI_REQUIRED_DETAIL:<OPTION_DETAIL_WIDTH$}"),
                    muted,
                ),
                Span::styled(" ", Style::default().fg(palette::text_subtle())),
            ])
            .alignment(Alignment::Center);
        }

        if self.selected_option_index == option_index {
            let row_text =
                format!(" {label:<OPTION_LABEL_WIDTH$}  {detail:<OPTION_DETAIL_WIDTH$} ");

            return Line::from(vec![Span::styled(row_text, style::selection_style())])
                .alignment(Alignment::Center);
        }

        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{label:<OPTION_LABEL_WIDTH$}"),
                Style::default()
                    .fg(palette::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default().fg(palette::text_subtle())),
            Span::styled(
                format!("{detail:<OPTION_DETAIL_WIDTH$}"),
                Style::default().fg(palette::text_muted()),
            ),
            Span::styled(" ", Style::default().fg(palette::text_subtle())),
        ])
        .alignment(Alignment::Center)
    }
}

impl Component for SessionCreationOverlay {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);
        let paragraph = Paragraph::new(self.lines())
            .alignment(Alignment::Left)
            .block(overlay::overlay_block("New Session", palette::accent()));

        overlay::clear_popup_area(frame, popup_area);
        frame.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "session_creation_overlay_test.rs"]
mod tests;
