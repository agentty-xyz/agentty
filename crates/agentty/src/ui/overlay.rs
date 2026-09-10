use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding};

use crate::ui::style::palette;

const OVERLAY_HORIZONTAL_PADDING: u16 = 2;
const OVERLAY_VERTICAL_PADDING: u16 = 1;

/// Percentage and minimum-size constraints for a centered overlay popup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayDimensions {
    height_percent: u16,
    min_height: u16,
    min_width: u16,
    width_percent: u16,
}

impl OverlayDimensions {
    /// Creates reusable dimensions for one popup family.
    pub const fn new(
        width_percent: u16,
        height_percent: u16,
        min_width: u16,
        min_height: u16,
    ) -> Self {
        Self {
            height_percent,
            min_height,
            min_width,
            width_percent,
        }
    }

    /// Computes a centered popup rectangle within `area`.
    pub fn centered_popup_area(self, area: Rect) -> Rect {
        centered_popup_area(
            area,
            self.width_percent,
            self.height_percent,
            self.min_width,
            self.min_height,
        )
    }
}

/// Composes sync popup body with optional project and branch context.
pub(crate) fn sync_popup_message(
    default_branch: Option<&str>,
    detail_message: &str,
    project_name: Option<&str>,
) -> String {
    match (project_name, default_branch) {
        (Some(project_name), Some(default_branch)) => format!(
            "Project `{project_name}` on main branch `{default_branch}`.\n\n{detail_message}"
        ),
        (Some(project_name), None) => format!("Project `{project_name}`.\n\n{detail_message}"),
        (None, Some(default_branch)) => {
            format!("Main branch `{default_branch}`.\n\n{detail_message}")
        }
        (None, None) => detail_message.to_string(),
    }
}

/// Clears popup-local cells and immediately reapplies the overlay surface
/// style so modal content never falls back to terminal-default colors.
pub(crate) fn clear_popup_area(f: &mut Frame, area: Rect) {
    let popup_style = Style::default()
        .fg(palette::text())
        .bg(palette::surface_overlay());

    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(popup_style), area);
}

/// Returns a centered popup rectangle constrained by bounds and minimum size.
pub(crate) fn centered_popup_area(
    area: Rect,
    width_percent: u16,
    height_percent: u16,
    min_width: u16,
    min_height: u16,
) -> Rect {
    let popup_width = (area.width * width_percent / 100)
        .max(min_width)
        .min(area.width);
    let popup_height = (area.height * height_percent / 100)
        .max(min_height)
        .min(area.height);

    Rect::new(
        area.x + (area.width.saturating_sub(popup_width)) / 2,
        area.y + (area.height.saturating_sub(popup_height)) / 2,
        popup_width,
        popup_height,
    )
}

/// Returns the inner text width for overlay content based on shared frame
/// chrome.
pub(crate) fn overlay_content_width(popup_width: u16) -> usize {
    let horizontal_chrome = 2 + (OVERLAY_HORIZONTAL_PADDING * 2);

    usize::from(popup_width.saturating_sub(horizontal_chrome).max(1))
}

/// Returns the total popup height required to render a given number of body
/// lines inside the shared overlay frame.
pub(crate) fn overlay_required_height(inner_line_count: usize) -> u16 {
    let vertical_chrome = 2 + (OVERLAY_VERTICAL_PADDING * 2);

    u16::try_from(inner_line_count.saturating_add(usize::from(vertical_chrome))).unwrap_or(u16::MAX)
}

/// Builds a shared rounded overlay frame block with centered styled title and
/// default body padding.
pub(crate) fn overlay_block(title: &str, border_color: Color) -> Block<'static> {
    let title_text = format!(" {title} ");

    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::new(
            OVERLAY_HORIZONTAL_PADDING,
            OVERLAY_HORIZONTAL_PADDING,
            OVERLAY_VERTICAL_PADDING,
            OVERLAY_VERTICAL_PADDING,
        ))
        .title(Span::styled(title_text, overlay_title_style(border_color)))
        .title_alignment(Alignment::Center)
}

/// Returns the shared title text style for overlay frame headers.
fn overlay_title_style(border_color: Color) -> Style {
    Style::default()
        .fg(border_color)
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
#[path = "overlay_test.rs"]
mod tests;
