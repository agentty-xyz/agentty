//! Remote-control popup listener, device, error, and hint rendering tests.

use super::RemoteControlOverlay;
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView, RemotePairingRequestView,
    RemoteRelayView,
};
use crate::ui::render::Component;
use crate::ui::style;

/// Renders `overlay` on a 100x40 terminal and returns its text.
fn rendered_text(overlay: &RemoteControlOverlay<'_>) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
        })
        .expect("failed to draw");

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
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

#[test]
fn running_listener_shows_address_code_devices_and_hints() {
    // Arrange
    let view = RemoteControlView {
        devices: paired_devices(),
        error: None,
        listener: Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
            relay: None,
            url: "http://127.0.0.1:4646/".to_string(),
        }),
        notice: None,
        pairing_requests: Vec::new(),
    };
    let overlay = RemoteControlOverlay::new(&view).selected_device_index(1);

    // Act
    let text = rendered_text(&overlay);

    // Assert
    assert!(text.contains("Remote Control"));
    assert!(text.contains("Status  On"));
    assert!(text.contains("http://127.0.0.1:4646/"));
    assert!(text.contains("ABCD-EFGH"));
    assert!(text.contains("Phone"));
    assert!(text.contains("connected"));
    assert!(text.contains("n: new code | s: stop | j/k: move | d: remove | Esc: close"));
    assert!(!text.contains("Pairing request"));
}

#[test]
fn pending_pairing_shows_device_confirmation_and_approval_hints() {
    // Arrange
    let request = |id: u64| RemotePairingRequestView {
        confirmation: format!("K7Q{id}"),
        device_name: format!("Laptop {id}"),
        id,
    };
    let view = RemoteControlView {
        devices: Vec::new(),
        error: None,
        listener: Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
            relay: None,
            url: "http://127.0.0.1:4646/".to_string(),
        }),
        notice: None,
        pairing_requests: vec![request(1), request(2)],
    };
    let overlay = RemoteControlOverlay::new(&view);

    // Act
    let text = rendered_text(&overlay);

    // Assert
    assert!(text.contains("Pairing request"));
    assert!(text.contains("Laptop 1"));
    assert!(text.contains("K7Q1"));
    assert!(!text.contains("Laptop 2"));
    assert!(text.contains("1 more waiting"));
    assert!(text.contains("y: approve | x: deny | n: new code | s: stop | Esc: close"));
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
    let overlay = RemoteControlOverlay::new(&view);

    // Act
    let text = rendered_text(&overlay);

    // Assert
    assert!(text.contains("K7QX"));
    assert!(!text.contains("more waiting"));
}

#[test]
fn selected_device_row_uses_the_selection_style() {
    // Arrange
    let view = RemoteControlView {
        devices: paired_devices(),
        ..RemoteControlView::default()
    };
    let overlay = RemoteControlOverlay::new(&view).selected_device_index(1);

    // Act
    let lines = overlay.lines(40, 40);

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

/// Builds a running listener linked to a relay.
fn relay_view(is_connected: bool, notice: Option<&str>) -> RemoteControlView {
    RemoteControlView {
        listener: Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
            relay: Some(RemoteRelayView::new(
                "https://relay.example/#code=ABCD".to_string(),
                is_connected,
            )),
            url: "http://127.0.0.1:4646/".to_string(),
        }),
        notice: notice.map(str::to_string),
        ..RemoteControlView::default()
    }
}

#[test]
fn relay_listener_shows_status_link_qr_code_and_link_hints() {
    // Arrange
    let view = relay_view(true, Some("Link copied. It works once."));
    let overlay = RemoteControlOverlay::new(&view);

    // Act
    let lines = overlay.lines(80, 60);
    let text: Vec<String> = lines.iter().map(ToString::to_string).collect();

    // Assert
    let relay_index = text
        .iter()
        .position(|line| line.starts_with("Relay   Connected"))
        .expect("relay row should render");
    let qr_rows = view
        .listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map(|relay| relay.qr_code.clone())
        .expect("relay should have a QR code");
    assert_eq!(text[relay_index + 1], qr_rows[0]);
    assert_eq!(
        lines[relay_index + 1].spans[0].style,
        style::scannable_code_style()
    );
    assert!(
        text.iter()
            .any(|line| line.contains("https://relay.example/#code=ABCD"))
    );
    assert!(
        text.iter()
            .any(|line| line == "Link copied. It works once.")
    );
    assert!(
        text.iter()
            .any(|line| line.contains("o: open link | c: copy link"))
    );
}

/// Renders `overlay` on a `width` by `height` terminal and returns its rows.
fn rendered_rows(overlay: &RemoteControlOverlay<'_>, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
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

#[test]
fn rendered_qr_code_keeps_every_row_intact_and_aligned() {
    // Arrange
    let view = relay_view(true, None);
    let overlay = RemoteControlOverlay::new(&view);
    let qr_code = view
        .listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map(|relay| relay.qr_code.clone())
        .expect("relay should have a QR code");

    // Act
    let rows = rendered_rows(&overlay, 160, 80);

    // Assert
    let first_row = rows
        .iter()
        .position(|row| row.contains(&qr_code[0]))
        .expect("first QR row should render");
    let column = rows[first_row].find(&qr_code[0]);
    for (offset, qr_row) in qr_code.iter().enumerate() {
        assert_eq!(rows[first_row + offset].find(qr_row.as_str()), column);
    }
}

#[test]
fn rendered_popup_keeps_its_hints_when_the_qr_code_barely_fits() {
    // Arrange
    let view = relay_view(true, None);
    let overlay = RemoteControlOverlay::new(&view);
    let first_qr_row = view
        .listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map(|relay| relay.qr_code[0].clone())
        .expect("relay should have a QR code");

    // Act
    let frames: Vec<(bool, bool)> = (30..90)
        .map(|height| {
            let rows = rendered_rows(&overlay, 100, height);
            let has_qr_code = rows.iter().any(|row| row.contains(&first_qr_row));
            let last_text_row = rows
                .iter()
                .rev()
                .find(|row| {
                    row.contains('│')
                        && row
                            .chars()
                            .any(|cell| cell.is_alphanumeric() || cell == ':')
                })
                .cloned()
                .unwrap_or_default();
            let has_hints = rows.iter().any(|row| row.contains("n: new code"))
                && last_text_row.contains("close");

            (has_qr_code, has_hints)
        })
        .collect();

    // Assert
    assert!(frames.iter().any(|(has_qr_code, _)| *has_qr_code));
    assert!(
        frames
            .iter()
            .all(|(has_qr_code, has_hints)| !has_qr_code || *has_hints)
    );
}

#[test]
fn relay_qr_code_is_left_out_when_it_does_not_fit() {
    // Arrange
    let view = relay_view(false, None);
    let overlay = RemoteControlOverlay::new(&view);
    let qr_height = view
        .listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map_or(0, |relay| relay.qr_code.len());

    // Act
    let too_short = overlay.lines(80, qr_height);
    let too_narrow = overlay.lines(10, 200);

    // Assert
    for lines in [too_short, too_narrow] {
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
        let relay_index = text
            .iter()
            .position(|line| line.starts_with("Relay   Connecting"))
            .expect("relay row should render");
        assert!(text[relay_index + 1].starts_with("Link"));
    }
}

#[test]
fn stopped_listener_shows_error_empty_devices_and_start_hint() {
    // Arrange
    let view = RemoteControlView {
        devices: Vec::new(),
        error: Some("Port 4646 is in use".to_string()),
        listener: None,
        notice: None,
        pairing_requests: Vec::new(),
    };
    let overlay = RemoteControlOverlay::new(&view);

    // Act
    let text = rendered_text(&overlay);

    // Assert
    assert!(text.contains("Status  Off"));
    assert!(text.contains("Port 4646 is in use"));
    assert!(text.contains("None yet"));
    assert!(text.contains("s: start | Esc: close"));
    assert!(!text.contains("Code"));
}
