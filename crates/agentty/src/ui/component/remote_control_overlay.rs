//! Centered popup showing the remote-control listener, its pairing code,
//! and the paired devices.

use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::presentation::remote_control::RemoteControlView;
use crate::ui::style::palette;
use crate::ui::{Component, overlay, style};

const MIN_OVERLAY_HEIGHT: u16 = 22;
const MIN_OVERLAY_WIDTH: u16 = 56;
/// Popup dimensions for the remote-control status.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(64, 70, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);
/// Width of the left-hand field labels.
const LABEL_WIDTH: usize = 8;

/// Centered popup for `/remote-control`.
pub struct RemoteControlOverlay<'a> {
    selected_device_index: usize,
    view: &'a RemoteControlView,
}

impl<'a> RemoteControlOverlay<'a> {
    /// Creates the popup for one frame's remote-control state.
    pub fn new(view: &'a RemoteControlView) -> Self {
        Self {
            selected_device_index: 0,
            view,
        }
    }

    /// Sets which paired-device row is highlighted.
    #[must_use]
    pub fn selected_device_index(mut self, selected_device_index: usize) -> Self {
        self.selected_device_index = selected_device_index;

        self
    }

    /// Returns all render lines for content `width` columns wide.
    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if let Some(request) = self.view.pairing_requests.first() {
            lines.push(Line::from(Span::styled(
                "Pairing request",
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(field_line("Device", &request.device_name, palette::text()));
            lines.push(field_line(
                "Confirm",
                &request.confirmation,
                palette::warning(),
            ));
            lines.push(muted_line(
                "Approve only if the device shows the same confirmation code.",
            ));
            let waiting = self.view.pairing_requests.len() - 1;
            if waiting > 0 {
                lines.push(muted_line(&format!("{waiting} more waiting")));
            }
            lines.push(Line::from(""));
        }
        match &self.view.listener {
            Some(listener) => {
                lines.push(field_line("Status", "On", palette::success()));
                lines.push(field_line("Open", &listener.url, palette::text()));
                lines.push(field_line(
                    "Code",
                    &listener.pairing_code,
                    palette::warning(),
                ));
                lines.push(Line::from(""));
                lines.push(muted_line(
                    "Open the address on another device through a tunnel that forwards this port, \
                     then enter the code. The code works once.",
                ));
            }
            None => {
                lines.push(field_line("Status", "Off", palette::text_muted()));
            }
        }
        if let Some(error) = &self.view.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(palette::danger()),
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Paired devices",
            Style::default()
                .fg(palette::text())
                .add_modifier(Modifier::BOLD),
        )));
        if self.view.devices.is_empty() {
            lines.push(muted_line("None yet"));
        }
        for (index, device) in self.view.devices.iter().enumerate() {
            let state = if device.is_connected { "connected" } else { "" };
            let name_width = width.saturating_sub(state.len() + 2).max(1);
            let name = truncate_with_ellipsis(&device.name, name_width);
            let row = format!(" {name:<name_width$}{state}");
            if index == self.selected_device_index {
                lines.push(Line::from(Span::styled(
                    format!("{row:<width$}"),
                    style::selection_style(),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    row,
                    Style::default().fg(palette::text()),
                )));
            }
        }

        lines.push(Line::from(""));
        lines.push(
            Line::from(Span::styled(
                self.help_text(),
                Style::default().fg(palette::text_muted()),
            ))
            .alignment(Alignment::Center),
        );

        lines
    }

    /// Returns the key hints that apply to the current state.
    fn help_text(&self) -> String {
        let mut hints = Vec::new();
        if !self.view.pairing_requests.is_empty() {
            hints.push("y: approve");
            hints.push("x: deny");
        }
        if self.view.listener.is_some() {
            hints.push("n: new code");
            hints.push("s: stop");
        } else {
            hints.push("s: start");
        }
        if !self.view.devices.is_empty() {
            hints.push("j/k: move");
            hints.push("d: remove");
        }
        hints.push("Esc: close");

        hints.join(" | ")
    }
}

impl Component for RemoteControlOverlay<'_> {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);
        let content_width = overlay::overlay_content_width(popup_area.width).max(1);
        let paragraph = Paragraph::new(self.lines(content_width))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block("Remote Control", palette::accent()));

        overlay::clear_popup_area(frame, popup_area);
        frame.render_widget(paragraph, popup_area);
    }
}

/// Builds one `label  value` row with the value in `color`.
fn field_line(label: &str, value: &str, color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<LABEL_WIDTH$}"),
            Style::default().fg(palette::text_muted()),
        ),
        Span::styled(
            value.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

/// Builds one muted explanatory row.
fn muted_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(palette::text_muted()),
    ))
}

#[cfg(test)]
#[path = "remote_control_overlay_test.rs"]
mod tests;
