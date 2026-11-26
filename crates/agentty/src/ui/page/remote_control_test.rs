//! Remote-control section rendering tests: pairing requests, the relay link
//! and its QR code, outcomes, paired devices, and the section layout.

use ag_remote::RelayStatus;
use ratatui::text::Line;

use super::{
    ENLARGE_FOR_QR_CODE, PageContent, QrPlacement, device_lines, hint_lines, outcome_lines,
    pairing_request_lines, relay_lines, relay_qr_code, render_section,
};
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemotePairingRequestView, RemoteRelayView,
};
use crate::ui::style;
use crate::ui::style::palette;

/// Lays out the remote-control sections of `view` for content `width`
/// columns wide and `height` rows high, as the Platform tab stacks them.
fn content(view: &RemoteControlView, width: usize, height: usize) -> PageContent<'_> {
    PageContent::new(
        relay_qr_code(view),
        width,
        height,
        |text_width, placement| {
            let mut lines = pairing_request_lines(view);
            if let Some(relay) = &view.relay {
                lines.extend(relay_lines(relay, placement));
            }
            lines.extend(outcome_lines(view));
            lines.extend(device_lines(view, text_width));

            lines
        },
    )
}

/// Renders the sections of `view` in one titled section on a `width` by
/// `height` terminal and returns its rows.
fn rendered_rows(view: &RemoteControlView, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            render_section(frame, area, "Section", |width, height| {
                content(view, width, height)
            });
        })
        .expect("failed to draw");
    let buffer = terminal.backend().buffer();

    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}

/// Renders the sections of `view` on a 100x40 terminal and returns its
/// text.
fn rendered_text(view: &RemoteControlView) -> String {
    rendered_rows(view, 100, 40).concat()
}

/// Builds two paired devices, the second one connected.
fn paired_devices() -> Vec<RemoteDeviceView> {
    vec![
        RemoteDeviceView {
            id: "phone".to_string(),
            is_connected: false,
            name: "Phone".to_string(),
        },
        RemoteDeviceView {
            id: "tablet".to_string(),
            is_connected: true,
            name: "Tablet".to_string(),
        },
    ]
}

/// Builds running remote control whose relay is in `status`, with
/// `notice` from the latest link action.
fn relay_view(status: RelayStatus, notice: Option<&str>) -> RemoteControlView {
    RemoteControlView {
        notice: notice.map(str::to_string),
        relay: Some(RemoteRelayView::new(
            "https://platform.example/sessions/#code=ABCD".to_string(),
            status,
        )),
        ..RemoteControlView::default()
    }
}

/// Returns the QR code rows of the relay `view` links to.
fn qr_code(view: &RemoteControlView) -> Vec<String> {
    view.relay
        .as_ref()
        .map(|relay| relay.qr_code.clone())
        .expect("relay should have a QR code")
}

/// Returns the text of `lines`, one string per line.
fn line_texts(lines: &[Line<'_>]) -> Vec<String> {
    lines.iter().map(ToString::to_string).collect()
}

#[test]
fn devices_list_every_paired_device_and_mark_the_connected_ones() {
    // Arrange
    let view = RemoteControlView {
        devices: paired_devices(),
        selected_device_index: 1,
        ..RemoteControlView::default()
    };

    // Act
    let text = rendered_text(&view);

    // Assert
    assert!(text.contains("Paired devices"));
    assert!(text.contains("Phone"));
    assert!(text.contains("connected"));
    assert!(!text.contains("Pairing request"));
}

#[test]
fn pending_pairing_shows_the_oldest_device_its_confirmation_and_how_many_wait() {
    // Arrange
    let request = |id: u64| RemotePairingRequestView {
        confirmation: format!("K7Q{id}"),
        device_name: format!("Laptop {id}"),
        id,
    };
    let view = RemoteControlView {
        pairing_requests: vec![request(1), request(2)],
        ..RemoteControlView::default()
    };

    // Act
    let text = rendered_text(&view);

    // Assert
    assert!(text.contains("Pairing request"));
    assert!(text.contains("Laptop 1"));
    assert!(text.contains("K7Q1"));
    assert!(!text.contains("Laptop 2"));
    assert!(text.contains("1 more waiting"));
}

#[test]
fn single_pending_pairing_omits_the_waiting_count() {
    // Arrange
    let view = RemoteControlView {
        pairing_requests: vec![RemotePairingRequestView {
            confirmation: "K7QX".to_string(),
            device_name: "Laptop".to_string(),
            id: 1,
        }],
        ..RemoteControlView::default()
    };

    // Act
    let text = rendered_text(&view);

    // Assert
    assert!(text.contains("K7QX"));
    assert!(!text.contains("more waiting"));
}

#[test]
fn selected_device_row_uses_the_selection_style() {
    // Arrange
    let view = RemoteControlView {
        devices: paired_devices(),
        selected_device_index: 1,
        ..RemoteControlView::default()
    };

    // Act
    let lines = content(&view, 40, 40).lines;

    // Assert
    let tablet_line = lines
        .iter()
        .find(|line| line.to_string().contains("Tablet"))
        .expect("tablet row should render");
    let phone_line = lines
        .iter()
        .find(|line| line.to_string().contains("Phone"))
        .expect("phone row should render");
    assert_eq!(tablet_line.spans[0].style, style::selection_style());
    assert_ne!(phone_line.spans[0].style, style::selection_style());
    assert_eq!(tablet_line.width(), 40);
}

#[test]
fn relay_puts_the_qr_code_beside_the_text_when_there_is_room() {
    // Arrange
    let view = relay_view(RelayStatus::Connected, Some("Link copied. It works once."));
    let qr_code = qr_code(&view);
    let qr_width = qr_code[0].chars().count();

    // Act
    let content = content(&view, 80, 60);
    let text = line_texts(&content.lines);

    // Assert
    assert_eq!(content.placement, QrPlacement::Beside);
    assert_eq!(content.qr_code, qr_code.as_slice());
    assert_eq!(content.text_width, 80 - qr_width - 2);
    assert!(!text.contains(&qr_code[0]));
    assert!(
        text.iter()
            .any(|line| line.starts_with("Relay   Connected"))
    );
    assert!(
        text.iter()
            .any(|line| line.contains("https://platform.example/sessions/#code=ABCD"))
    );
    assert!(
        text.iter()
            .any(|line| line.starts_with("Scan the code or open the link"))
    );
    assert!(!text.iter().any(|line| line == ENLARGE_FOR_QR_CODE));
    assert!(
        text.iter()
            .any(|line| line == "Link copied. It works once.")
    );
}

#[test]
fn relay_puts_the_qr_code_below_the_relay_when_too_narrow_beside() {
    // Arrange
    let view = relay_view(RelayStatus::Connected, None);
    let qr_code = qr_code(&view);

    // Act
    let content = content(&view, 60, 60);
    let text = line_texts(&content.lines);

    // Assert
    let relay_index = text
        .iter()
        .position(|line| line.starts_with("Relay   Connected"))
        .expect("relay row should render");
    assert_eq!(content.placement, QrPlacement::Below);
    assert_eq!(content.text_width, 60);
    assert_eq!(text[relay_index + 1], qr_code[0]);
    assert_eq!(
        content.lines[relay_index + 1].spans[0].style,
        style::scannable_code_style()
    );
    assert!(text[relay_index + 1 + qr_code.len()].starts_with("Link"));
    assert!(
        text.iter()
            .any(|line| line.starts_with("Scan the code or open the link"))
    );
}

#[test]
fn rendered_qr_code_keeps_every_row_intact_and_aligned() {
    // Arrange
    let view = relay_view(RelayStatus::Connected, None);
    let qr_code = qr_code(&view);

    for (width, is_beside) in [(160, true), (60, false)] {
        // Act
        let rows = rendered_rows(&view, width, 80);

        // Assert
        let first_row = rows
            .iter()
            .position(|row| row.contains(&qr_code[0]))
            .expect("first QR row should render");
        let column = rows[first_row].find(&qr_code[0]);
        for (offset, qr_row) in qr_code.iter().enumerate() {
            assert_eq!(rows[first_row + offset].find(qr_row.as_str()), column);
        }
        // Beside the text, the code starts on the first content row.
        assert_eq!(first_row == 1, is_beside);
    }
}

#[test]
fn relay_qr_code_is_left_out_when_it_does_not_fit() {
    // Arrange
    let view = relay_view(RelayStatus::Connecting, None);
    let qr_height = qr_code(&view).len();

    // Act
    let too_short = content(&view, 80, qr_height - 1);
    let too_narrow = content(&view, 10, 200);

    // Assert
    for content in [too_short, too_narrow] {
        let text = line_texts(&content.lines);
        let relay_index = text
            .iter()
            .position(|line| line.starts_with("Relay   Connecting"))
            .expect("relay row should render");
        assert_eq!(content.placement, QrPlacement::Hidden);
        assert!(text[relay_index + 1].starts_with("Link"));
        assert!(text.iter().any(|line| line.starts_with("Open the link")));
        assert!(text.iter().any(|line| line == ENLARGE_FOR_QR_CODE));
    }
}

#[test]
fn relay_link_without_a_qr_code_asks_for_no_larger_terminal() {
    // Arrange
    let view = RemoteControlView {
        relay: Some(RemoteRelayView {
            link: "https://platform.example/sessions/#code=ABCD".to_string(),
            qr_code: Vec::new(),
            status: RelayStatus::Connected,
        }),
        ..RemoteControlView::default()
    };

    // Act
    let content = content(&view, 200, 200);
    let text = line_texts(&content.lines);

    // Assert
    assert_eq!(content.placement, QrPlacement::Hidden);
    assert!(text.iter().any(|line| line.starts_with("Open the link")));
    assert!(!text.iter().any(|line| line == ENLARGE_FOR_QR_CODE));
}

/// Builds a page text of one 200-column line, or, below the relay, that
/// line under `qr_code`'s rows.
fn sample_lines(qr_code: &[String], placement: QrPlacement) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if placement == QrPlacement::Below {
        lines.extend(qr_code.iter().map(|row| Line::from(row.clone())));
    }
    lines.push(Line::from("word ".repeat(40)));

    lines
}

#[test]
fn page_content_places_the_qr_code_where_it_fits_first() {
    // Arrange
    // A 10-column, 5-row code and one 200-column line, which takes 6 rows
    // at 40 columns and 5 at 51 or 52, counting one spare row once wrapped.
    let qr_code = vec!["x".repeat(10); 5];
    let cases = [
        (52, 6, QrPlacement::Beside, 40),
        (52, 5, QrPlacement::Hidden, 52),
        (51, 10, QrPlacement::Below, 51),
        (51, 9, QrPlacement::Hidden, 51),
        (9, 100, QrPlacement::Hidden, 9),
    ];

    for (width, height, expected_placement, expected_text_width) in cases {
        // Act
        let content = PageContent::new(&qr_code, width, height, |_, placement| {
            sample_lines(&qr_code, placement)
        });

        // Assert
        assert_eq!(
            (content.placement, content.text_width),
            (expected_placement, expected_text_width),
            "{width}x{height}"
        );
    }
}

#[test]
fn page_content_without_a_qr_code_leaves_it_out() {
    // Arrange
    let qr_code: Vec<String> = Vec::new();

    // Act
    let content = PageContent::new(&qr_code, 200, 200, |_, placement| {
        sample_lines(&qr_code, placement)
    });

    // Assert
    assert_eq!(content.placement, QrPlacement::Hidden);
    assert_eq!(content.text_width, 200);
}

#[test]
fn relay_that_does_not_admit_this_agentty_says_so_in_danger_color() {
    // Arrange
    let view = relay_view(RelayStatus::NotAdmitted, None);

    // Act
    let lines = content(&view, 200, 80).lines;

    // Assert
    let relay_index = lines
        .iter()
        .position(|line| line.to_string().starts_with("Relay   Not admitted"))
        .expect("relay row should render");
    assert_eq!(
        lines[relay_index].spans[1].style.fg,
        Some(palette::danger())
    );
    assert!(
        lines[relay_index + 1]
            .to_string()
            .starts_with("The relay refused this Agentty")
    );
}

#[test]
fn an_error_shows_after_the_notice_and_no_devices_read_none_yet() {
    // Arrange
    let view = RemoteControlView {
        error: Some("Could not load paired devices: disk full".to_string()),
        notice: Some("Link copied. It works once.".to_string()),
        ..RemoteControlView::default()
    };

    // Act
    let text = rendered_text(&view);

    // Assert
    let notice = text.find("Link copied").expect("notice should render");
    let error = text
        .find("Could not load paired devices: disk full")
        .expect("error should render");
    assert!(notice < error);
    assert!(text.contains("None yet"));
}

#[test]
fn hint_lines_break_only_between_hints() {
    // Arrange
    let hints = ["a: keep on", "l: sign out", "Enter: stop"].map(String::from);

    // Act
    let one_row = line_texts(&hint_lines(&hints, 200));
    let wrapped = line_texts(&hint_lines(&hints, 25));
    let too_narrow = line_texts(&hint_lines(&hints, 5));

    // Assert
    assert_eq!(one_row, ["a: keep on | l: sign out | Enter: stop"]);
    assert_eq!(wrapped, ["a: keep on | l: sign out", "Enter: stop"]);
    assert_eq!(too_narrow, ["a: keep on", "l: sign out", "Enter: stop"]);
}
