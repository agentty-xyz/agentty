use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::style::palette;
use crate::ui::{Component, overlay};

/// Minimum popup height that leaves room for title, options, and hints
/// without adding unused vertical space.
const MIN_OVERLAY_HEIGHT: u16 = 13;
/// Minimum popup width sized for the longest option row plus shared overlay
/// chrome.
const MIN_OVERLAY_WIDTH: u16 = 53;
/// Fixed description width so option labels share one left edge.
const OPTION_DETAIL_WIDTH: usize = 27;
/// Fixed label width for the longest session-type label.
const OPTION_LABEL_WIDTH: usize = 15;
/// Detail text for the experimental orchestrator session creation path.
const ORCHESTRATOR_SESSION_PREVIEW_DETAIL: &str = "[Preview] Plan workers";
/// Detail text for the experimental append-to-stack action when enabled.
const APPEND_TO_STACK_PREVIEW_DETAIL: &str = "[Preview] Move under parent";
/// Detail text for the experimental append-to-stack action when disabled.
const APPEND_TO_STACK_DISABLED_PREVIEW_DETAIL: &str = "[Preview] Review only";
/// Detail text for the stacked session creation path.
const STACKED_SESSION_DETAIL: &str = "Stack on selected";
/// Popup dimensions for the compact session selector.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(30, 22, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);

/// Centered popup used to choose the type of session to create.
pub struct SessionCreationOverlay {
    /// Whether the highlighted session can be moved into an existing stack.
    can_append_to_stack: bool,
    /// Whether the highlighted list session can parent a new stacked draft.
    can_create_stacked_session: bool,
    /// Currently highlighted option row in the selector.
    selected_option_index: usize,
}

impl SessionCreationOverlay {
    /// Creates a session creation selector with the provided highlighted row.
    pub fn new(
        selected_option_index: usize,
        can_create_stacked_session: bool,
        can_append_to_stack: bool,
    ) -> Self {
        Self {
            can_append_to_stack,
            can_create_stacked_session,
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
            self.option_line(0, "Regular", "Start immediately", false),
            self.option_line(1, "Draft", "Stage locally first", false),
            self.option_line(
                2,
                "Orchestrator",
                ORCHESTRATOR_SESSION_PREVIEW_DETAIL,
                false,
            ),
            self.option_line(
                3,
                "Stacked",
                if self.can_create_stacked_session {
                    STACKED_SESSION_DETAIL
                } else {
                    "Select parent first"
                },
                !self.can_create_stacked_session,
            ),
            self.option_line(
                4,
                "Append to stack",
                if self.can_append_to_stack {
                    APPEND_TO_STACK_PREVIEW_DETAIL
                } else {
                    APPEND_TO_STACK_DISABLED_PREVIEW_DETAIL
                },
                !self.can_append_to_stack,
            ),
            Line::from(""),
            Line::from(vec![Span::styled(
                "j/k: move | Enter: select | q: close",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        ]
    }

    /// Builds one option row and applies the active selection style.
    fn option_line(
        &self,
        option_index: usize,
        label: &'static str,
        detail: &'static str,
        is_disabled: bool,
    ) -> Line<'static> {
        let is_selected = self.selected_option_index == option_index && !is_disabled;
        let row_text = format!(" {label:<OPTION_LABEL_WIDTH$}  {detail:<OPTION_DETAIL_WIDTH$} ");

        if is_selected {
            return Line::from(vec![Span::styled(
                row_text,
                Style::default()
                    .fg(palette::surface_overlay())
                    .bg(palette::accent())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center);
        }

        let (label_style, detail_style) = if is_disabled {
            (
                Style::default().fg(palette::text_subtle()),
                Style::default().fg(palette::text_subtle()),
            )
        } else {
            (
                Style::default()
                    .fg(palette::accent())
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(palette::text_muted()),
            )
        };

        Line::from(vec![
            Span::styled(" ", Style::default().fg(palette::text_subtle())),
            Span::styled(format!("{label:<OPTION_LABEL_WIDTH$}"), label_style),
            Span::styled("  ", Style::default().fg(palette::text_subtle())),
            Span::styled(format!("{detail:<OPTION_DETAIL_WIDTH$}"), detail_style),
            Span::styled(" ", Style::default().fg(palette::text_subtle())),
        ])
        .alignment(Alignment::Center)
    }
}

impl Component for SessionCreationOverlay {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);
        let paragraph = Paragraph::new(self.lines())
            .alignment(Alignment::Left)
            .block(overlay::overlay_block("New Session", palette::accent()));

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "session_creation_overlay_test.rs"]
mod tests;
