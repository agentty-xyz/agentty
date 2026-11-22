//! Centered popup showing the remote-control listener, its pairing code,
//! and the paired devices.

use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::presentation::remote_control::{RemoteControlView, RemoteRelayView};
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

    /// Returns all render lines for content `width` columns wide and
    /// `height` rows high. The relay QR code is included only when it fits
    /// beside everything else.
    fn lines(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let lines = self.content_lines(width, &[]);
        let qr_code = self
            .view
            .listener
            .as_ref()
            .and_then(|listener| listener.relay.as_ref())
            .map_or(&[][..], |relay| relay.qr_code.as_slice());
        let qr_width = qr_code.first().map_or(0, |row| row.chars().count());
        let fits = qr_width <= width && wrapped_row_count(&lines, width) + qr_code.len() <= height;
        if qr_code.is_empty() || !fits {
            return lines;
        }

        self.content_lines(width, qr_code)
    }

    /// Returns the render lines with `qr_code` rows below the relay status.
    fn content_lines(&self, width: usize, qr_code: &[String]) -> Vec<Line<'static>> {
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
                if let Some(relay) = &listener.relay {
                    lines.extend(relay_lines(relay, qr_code));
                }
                if let Some(notice) = &self.view.notice {
                    lines.push(Line::from(Span::styled(
                        notice.clone(),
                        Style::default().fg(palette::success()),
                    )));
                }
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
        if let Some(listener) = &self.view.listener {
            hints.push("n: new code");
            if listener.relay.is_some() {
                hints.push("o: open link");
                hints.push("c: copy link");
            }
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
        let content_height = usize::from(
            popup_area
                .height
                .saturating_sub(overlay::overlay_required_height(0)),
        );
        let paragraph = Paragraph::new(self.lines(content_width, content_height))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block("Remote Control", palette::accent()));

        overlay::clear_popup_area(frame, popup_area);
        frame.render_widget(paragraph, popup_area);
    }
}

/// Builds the relay status, `qr_code` rows in dark-on-light colors, the
/// pairing link, and how to use them.
fn relay_lines(relay: &RemoteRelayView, qr_code: &[String]) -> Vec<Line<'static>> {
    let (state, color) = if relay.is_connected {
        ("Connected", palette::success())
    } else {
        ("Connecting", palette::warning())
    };
    let qr_style = style::scannable_code_style();
    let mut lines = vec![Line::from(""), field_line("Relay", state, color)];
    lines.extend(
        qr_code
            .iter()
            .map(|row| Line::from(Span::styled(row.clone(), qr_style))),
    );
    lines.push(field_line("Link", &relay.link, palette::text()));
    lines.push(muted_line(
        "Or scan the code or open the link on the other device. Traffic through the relay is \
         encrypted end to end, and the link works once.",
    ));

    lines
}

/// Estimates the rows `lines` take once wrapped to `width` columns. Word
/// wrapping can break before a full row, so each wrapped line counts one
/// extra row to stay on the safe side.
fn wrapped_row_count(lines: &[Line<'_>], width: usize) -> usize {
    lines
        .iter()
        .map(|line| {
            let rows = line.width().div_ceil(width).max(1);

            rows + usize::from(rows > 1)
        })
        .sum()
}

/// Builds one `label  value` row with the value in `color`.
fn field_line(label: &str, value: &str, color: Color) -> Line<'static> {
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
