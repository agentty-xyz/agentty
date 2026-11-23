//! Remote tab page showing the remote-control listener, its pairing code,
//! and the paired devices of the active project.

use ag_remote::RelayStatus;
use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::presentation::help_action::remote_control_actions;
use crate::presentation::remote_control::{RemoteControlView, RemoteRelayView};
use crate::ui::style::palette;
use crate::ui::{Page, layout, style};

/// Width of the left-hand field labels.
const LABEL_WIDTH: usize = 8;

/// Renders the Remote tab: remote-control status, pairing, and devices.
pub struct RemoteControlPage<'a> {
    project_name: Option<String>,
    view: &'a RemoteControlView,
}

impl<'a> RemoteControlPage<'a> {
    /// Creates the page for one frame's remote-control state. Paired
    /// devices follow the sessions of the project named `project_name`.
    pub(crate) fn new(view: &'a RemoteControlView, project_name: Option<String>) -> Self {
        Self { project_name, view }
    }

    /// Returns the title naming the project that paired devices follow.
    fn title(&self) -> String {
        match self.project_name.as_deref() {
            Some(name) => format!("'{name}' remote control"),
            None => "Remote control".to_string(),
        }
    }

    /// Returns all render lines for content `width` columns wide and
    /// `height` rows high. The relay QR code is included only when it fits
    /// beside everything else.
    fn lines(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let lines = self.content_lines(width, &[]);
        let qr_code = fitting_qr_code(self.view, &lines, width, height);
        if qr_code.is_empty() {
            return lines;
        }

        self.content_lines(width, qr_code)
    }

    /// Returns the render lines with `qr_code` rows below the relay status.
    fn content_lines(&self, width: usize, qr_code: &[String]) -> Vec<Line<'static>> {
        let mut lines = pairing_request_lines(self.view);
        if let Some(listener) = &self.view.listener {
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
        } else {
            lines.push(field_line("Status", "Off", palette::text_muted()));
            lines.push(Line::from(""));
            lines.push(muted_line(
                "Start remote control to follow and steer this project's sessions from a paired \
                 phone or browser.",
            ));
        }
        lines.extend(outcome_lines(self.view));
        lines.extend(device_lines(self.view, width));

        lines.push(Line::from(""));
        lines.push(muted_line(&self.help_text()));

        lines
    }

    /// Returns the key hints that apply to the current state.
    fn help_text(&self) -> String {
        let hints: Vec<String> = remote_control_actions(self.view)
            .iter()
            .map(|action| format!("{}: {}", action.key, action.footer_label))
            .collect();

        hints.join(" | ")
    }
}

impl Page for RemoteControlPage<'_> {
    /// Renders the remote-control state in one bordered section that fills
    /// the tab page.
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let main_area = layout::tab_page_area(area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(self.title())
            .border_style(style::border_style())
            .padding(Padding::horizontal(1));
        let content_area = block.inner(main_area);
        let lines = self.lines(
            usize::from(content_area.width).max(1),
            usize::from(content_area.height),
        );
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true }).block(block);

        frame.render_widget(paragraph, main_area);
    }
}

/// Returns the relay QR code of `view` when it fits in `width` columns and
/// `height` rows beside `lines`, or no rows.
pub(super) fn fitting_qr_code<'view>(
    view: &'view RemoteControlView,
    lines: &[Line<'_>],
    width: usize,
    height: usize,
) -> &'view [String] {
    let qr_code = view
        .listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map_or(&[][..], |relay| relay.qr_code.as_slice());
    let qr_width = qr_code.first().map_or(0, |row| row.chars().count());
    let fits = qr_width <= width && wrapped_row_count(lines, width) + qr_code.len() <= height;
    if !fits {
        return &[];
    }

    qr_code
}

/// Builds the oldest pairing request, with its confirmation code and how
/// many more wait, or no rows while none waits.
pub(super) fn pairing_request_lines(view: &RemoteControlView) -> Vec<Line<'static>> {
    let Some(request) = view.pairing_requests.first() else {
        return Vec::new();
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "Pairing request",
            Style::default()
                .fg(palette::warning())
                .add_modifier(Modifier::BOLD),
        )),
        field_line("Device", &request.device_name, palette::text()),
        field_line("Confirm", &request.confirmation, palette::warning()),
        muted_line("Approve only if the device shows the same confirmation code."),
    ];
    let waiting = view.pairing_requests.len() - 1;
    if waiting > 0 {
        lines.push(muted_line(&format!("{waiting} more waiting")));
    }
    lines.push(Line::from(""));

    lines
}

/// Builds the outcome of the latest remote-control action: a link notice,
/// then any error.
pub(super) fn outcome_lines(view: &RemoteControlView) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(notice) = &view.notice {
        lines.push(Line::from(Span::styled(
            notice.clone(),
            Style::default().fg(palette::success()),
        )));
    }
    if let Some(error) = &view.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(palette::danger()),
        )));
    }

    lines
}

/// Builds the paired-device list for content `width` columns wide, with
/// the highlighted device selected and live devices marked.
pub(super) fn device_lines(view: &RemoteControlView, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Paired devices",
            Style::default()
                .fg(palette::text())
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if view.devices.is_empty() {
        lines.push(muted_line("None yet"));
    }
    for (index, device) in view.devices.iter().enumerate() {
        let state = if device.is_connected { "connected" } else { "" };
        let name_width = width.saturating_sub(state.len() + 2).max(1);
        let name = truncate_with_ellipsis(&device.name, name_width);
        let row = format!(" {name:<name_width$}{state}");
        if index == view.selected_device_index {
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

    lines
}

/// Builds the relay status, `qr_code` rows in dark-on-light colors, the
/// pairing link, and how to use them.
pub(super) fn relay_lines(relay: &RemoteRelayView, qr_code: &[String]) -> Vec<Line<'static>> {
    let (state, color) = match relay.status {
        RelayStatus::Connected => ("Connected", palette::success()),
        RelayStatus::Connecting => ("Connecting", palette::warning()),
        RelayStatus::NotAdmitted => ("Not admitted", palette::danger()),
    };
    let qr_style = style::scannable_code_style();
    let mut lines = vec![Line::from(""), field_line("Relay", state, color)];
    if relay.status == RelayStatus::NotAdmitted {
        lines.push(Line::from(Span::styled(
            "The relay refused this Agentty: it serves only the instances it admits. Devices \
             cannot reach this Agentty through it until it does; Agentty keeps trying.",
            Style::default().fg(palette::danger()),
        )));
    }
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
pub(super) fn field_line(label: &str, value: &str, color: Color) -> Line<'static> {
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
pub(super) fn muted_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(palette::text_muted()),
    ))
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
