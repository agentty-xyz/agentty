use ag_tui_text::text_util::wrap_lines_breaking_long_words;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::style::palette;
use crate::ui::{Component, overlay, style};

const MAX_OVERLAY_HEIGHT_PERCENT: u16 = 90;
const MIN_OVERLAY_HEIGHT: u16 = 8;
const MIN_OVERLAY_WIDTH: u16 = 40;
/// Blank separator row plus the `Yes`/`No` row below the preview.
const OPTION_ROW_COUNT: usize = 2;
const OVERLAY_TITLE: &str = "Submit diff comments?";
const OVERLAY_WIDTH_PERCENT: u16 = 70;

/// Leave-diff popup that previews pending diff comments above `Yes`/`No`.
///
/// The preview shows the exact text submitted to the agent and the popup grows
/// to fit it. When it still exceeds the screen, its last visible row reports
/// how many rows are hidden so the choices always remain visible.
pub struct DiffCommentSubmitOverlay<'a> {
    preview: &'a str,
    selected_yes: bool,
}

impl<'a> DiffCommentSubmitOverlay<'a> {
    /// Creates the popup for the submitted comment `preview` text.
    pub fn new(preview: &'a str, selected_yes: bool) -> Self {
        Self {
            preview,
            selected_yes,
        }
    }

    /// Returns a popup tall enough for every wrapped preview row, capped at
    /// `MAX_OVERLAY_HEIGHT_PERCENT` of `area`, plus the wrapped rows.
    fn popup_area_and_preview(&self, area: Rect) -> (Rect, Vec<Line<'a>>) {
        let popup_width =
            overlay::centered_popup_area(area, OVERLAY_WIDTH_PERCENT, 0, MIN_OVERLAY_WIDTH, 0)
                .width;
        let preview_lines = wrap_lines_breaking_long_words(
            self.preview,
            overlay::overlay_content_width(popup_width),
        );
        let max_popup_height = area.height * MAX_OVERLAY_HEIGHT_PERCENT / 100;
        let popup_height =
            overlay::overlay_required_height(preview_lines.len().saturating_add(OPTION_ROW_COUNT))
                .min(max_popup_height)
                .max(MIN_OVERLAY_HEIGHT);
        let popup_area = overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            0,
            MIN_OVERLAY_WIDTH,
            popup_height,
        );

        (popup_area, preview_lines)
    }

    /// Replaces preview rows beyond `height` with a hidden-row count.
    fn fit_preview_lines(mut lines: Vec<Line<'a>>, height: usize) -> Vec<Line<'a>> {
        if lines.len() > height {
            let visible_count = height.saturating_sub(1);
            let hidden_count = lines.len() - visible_count;
            lines.truncate(visible_count);
            lines.push(Line::from(Span::styled(
                format!("... {hidden_count} more lines"),
                Style::default().fg(palette::text_muted()),
            )));
        }

        lines
    }

    /// Builds the highlighted `Yes`/`No` choice row.
    fn option_line(&self) -> Line<'static> {
        let selected_option_style = style::selection_style();
        let unselected_option_style = Style::default().fg(palette::text());
        let (yes_style, no_style) = if self.selected_yes {
            (selected_option_style, unselected_option_style)
        } else {
            (unselected_option_style, selected_option_style)
        };

        Line::from(vec![
            Span::styled(" Yes ", yes_style),
            Span::raw("   "),
            Span::styled(" No ", no_style),
        ])
        .alignment(Alignment::Center)
    }
}

impl Component for DiffCommentSubmitOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let (popup_area, preview_lines) = self.popup_area_and_preview(area);
        let block = overlay::overlay_block(OVERLAY_TITLE, palette::warning());
        let inner_area = block.inner(popup_area);
        let [preview_area, _, option_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner_area);
        let preview_lines =
            Self::fit_preview_lines(preview_lines, usize::from(preview_area.height));
        let preview_paragraph =
            Paragraph::new(preview_lines).style(Style::default().fg(palette::text()));

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(block, popup_area);
        f.render_widget(preview_paragraph, preview_area);
        f.render_widget(Paragraph::new(self.option_line()), option_area);
    }
}

#[cfg(test)]
#[path = "diff_comment_submit_overlay_test.rs"]
mod tests;
