//! Remote tab page showing the remote-control listener, its pairing code,
//! and the paired devices, which reach every project the terminal lists.

use ag_remote::RelayStatus;
use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::presentation::help_action::remote_control_actions;
use crate::presentation::remote_control::{RemoteControlView, RemoteRelayView};
use crate::ui::style::palette;
use crate::ui::{Page, layout, style};

/// Width of the left-hand field labels.
const LABEL_WIDTH: usize = 8;

/// Fewest text columns a QR code beside the text leaves.
const MIN_TEXT_WIDTH: usize = 40;

/// Columns between the text and a QR code beside it.
const QR_GAP: u16 = 2;

/// Separates the key hints on one row.
const HINT_SEPARATOR: &str = " | ";

/// Explains how to show a QR code that does not fit.
const ENLARGE_FOR_QR_CODE: &str = "Enlarge the terminal to show the link as a QR code.";

/// Title of the Remote tab, which names no project: paired devices pick
/// their own among the projects the terminal lists.
const TITLE: &str = "Remote control";

/// Renders the Remote tab: remote-control status, pairing, and devices.
pub struct RemoteControlPage<'a> {
    view: &'a RemoteControlView,
}

impl<'a> RemoteControlPage<'a> {
    /// Creates the page for one frame's remote-control state.
    pub(crate) fn new(view: &'a RemoteControlView) -> Self {
        Self { view }
    }

    /// Lays the page out for content `width` columns wide and `height` rows
    /// high, with the relay QR code wherever it fits.
    fn content(&self, width: usize, height: usize) -> PageContent<'a> {
        PageContent::new(
            relay_qr_code(self.view),
            width,
            height,
            |text_width, placement| self.lines(text_width, placement),
        )
    }

    /// Returns the render lines for text `width` columns wide, with the
    /// relay QR code at `placement`.
    fn lines(&self, width: usize, placement: QrPlacement) -> Vec<Line<'static>> {
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
                lines.extend(relay_lines(relay, placement));
            }
        } else {
            lines.push(field_line("Status", "Off", palette::text_muted()));
            lines.push(Line::from(""));
            lines.push(muted_line(
                "Start remote control to follow and steer the sessions of your projects from a \
                 paired phone or browser.",
            ));
        }
        lines.extend(outcome_lines(self.view));
        lines.extend(device_lines(self.view, width));

        lines.push(Line::from(""));
        lines.extend(hint_lines(&self.hints(), width));

        lines
    }

    /// Returns the key hints that apply to the current state.
    fn hints(&self) -> Vec<String> {
        remote_control_actions(self.view)
            .iter()
            .map(|action| format!("{}: {}", action.key, action.footer_label))
            .collect()
    }
}

impl Page for RemoteControlPage<'_> {
    /// Renders the remote-control state in one bordered section that fills
    /// the tab page.
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        render_section(frame, area, TITLE, |width, height| {
            self.content(width, height)
        });
    }
}

/// Where a page puts the relay QR code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QrPlacement {
    /// Right of the text, which narrows to make room.
    Beside,
    /// Below the relay status, among the text rows.
    Below,
    /// Left out, because it fits nowhere or the link has none.
    Hidden,
}

/// One frame's page layout: the text and where the relay QR code goes.
pub(super) struct PageContent<'view> {
    /// Text rows, wrapped to `text_width` columns.
    pub(super) lines: Vec<Line<'static>>,
    /// Where the relay QR code goes.
    pub(super) placement: QrPlacement,
    /// Relay QR code rows, drawn right of the text while placed beside it.
    pub(super) qr_code: &'view [String],
    /// Columns the text wraps to.
    pub(super) text_width: usize,
}

impl<'view> PageContent<'view> {
    /// Lays out a page for content `width` columns wide and `height` rows
    /// high, where `build_lines` returns the text for a text width and QR
    /// placement. The relay `qr_code` goes beside the text when both fit,
    /// else below the relay status when everything still fits, else
    /// nowhere, so it never pushes text out of view.
    pub(super) fn new(
        qr_code: &'view [String],
        width: usize,
        height: usize,
        build_lines: impl Fn(usize, QrPlacement) -> Vec<Line<'static>>,
    ) -> Self {
        let qr_width = qr_code.first().map_or(0, |row| row.chars().count());
        let beside_width = width.saturating_sub(qr_width + usize::from(QR_GAP));
        let candidates = [
            (
                QrPlacement::Beside,
                beside_width,
                beside_width >= MIN_TEXT_WIDTH && qr_code.len() <= height,
            ),
            (QrPlacement::Below, width, qr_width <= width),
        ];
        if !qr_code.is_empty() {
            for (placement, text_width, qr_fits) in candidates {
                if !qr_fits {
                    continue;
                }
                let lines = build_lines(text_width, placement);
                if wrapped_row_count(&lines, text_width) <= height {
                    return Self {
                        lines,
                        placement,
                        qr_code,
                        text_width,
                    };
                }
            }
        }

        Self {
            lines: build_lines(width, QrPlacement::Hidden),
            placement: QrPlacement::Hidden,
            qr_code,
            text_width: width,
        }
    }
}

/// Renders one bordered section titled `title` that fills the tab page in
/// `area`. `layout_content` lays the page out for the section's inner
/// width and height; a QR code placed beside the text is drawn right of
/// it, top-aligned.
pub(super) fn render_section<'view>(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    layout_content: impl FnOnce(usize, usize) -> PageContent<'view>,
) {
    let main_area = layout::tab_page_area(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style::border_style())
        .padding(Padding::horizontal(1));
    let content_area = block.inner(main_area);
    let content = layout_content(
        usize::from(content_area.width).max(1),
        usize::from(content_area.height),
    );
    let text = Paragraph::new(content.lines).wrap(Wrap { trim: true });
    frame.render_widget(block, main_area);
    if content.placement != QrPlacement::Beside {
        frame.render_widget(text, content_area);

        return;
    }

    let text_width = u16::try_from(content.text_width).unwrap_or(content_area.width);
    let [text_area, qr_area] =
        Layout::horizontal([Constraint::Length(text_width), Constraint::Fill(1)])
            .spacing(QR_GAP)
            .areas(content_area);
    let qr_style = style::scannable_code_style();
    let qr_code: Vec<Line<'_>> = content
        .qr_code
        .iter()
        .map(|row| Line::from(Span::styled(row.as_str(), qr_style)))
        .collect();
    frame.render_widget(text, text_area);
    frame.render_widget(Paragraph::new(qr_code), qr_area);
}

/// Returns the relay QR code rows of `view`, or none without a relay.
pub(super) fn relay_qr_code(view: &RemoteControlView) -> &[String] {
    view.listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map_or(&[][..], |relay| relay.qr_code.as_slice())
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

/// Builds the relay status, the QR code rows in dark-on-light colors while
/// `placement` puts them below it, the pairing link, and how to use them.
pub(super) fn relay_lines(relay: &RemoteRelayView, placement: QrPlacement) -> Vec<Line<'static>> {
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
    if placement == QrPlacement::Below {
        lines.extend(
            relay
                .qr_code
                .iter()
                .map(|row| Line::from(Span::styled(row.clone(), qr_style))),
        );
    }
    lines.push(field_line("Link", &relay.link, palette::text()));
    let is_hidden = placement == QrPlacement::Hidden;
    let how_to_pair = if is_hidden {
        "Open the link on the other device."
    } else {
        "Scan the code or open the link on the other device."
    };
    lines.push(muted_line(&format!(
        "{how_to_pair} Traffic through the relay is encrypted end to end, and the link works once."
    )));
    if is_hidden && !relay.qr_code.is_empty() {
        lines.push(muted_line(ENLARGE_FOR_QR_CODE));
    }

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

/// Builds the key `hints` as muted rows of at most `width` columns that
/// break only between hints, so no hint splits across rows.
pub(super) fn hint_lines(hints: &[String], width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<String> = Vec::new();
    for hint in hints {
        match rows.last_mut() {
            Some(row) if row.width() + HINT_SEPARATOR.len() + hint.width() <= width => {
                row.push_str(HINT_SEPARATOR);
                row.push_str(hint);
            }
            _ => rows.push(hint.clone()),
        }
    }

    rows.iter().map(|row| muted_line(row)).collect()
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
