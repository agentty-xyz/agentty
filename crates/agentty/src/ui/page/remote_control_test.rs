//! Remote tab page listener, device, error, and hint rendering tests.

use ag_remote::RelayStatus;

use super::RemoteControlPage;
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView, RemotePairingRequestView,
    RemoteRelayView,
};
use crate::ui::render::Page;
use crate::ui::style;
use crate::ui::style::palette;

/// Renders the page for `view` on a `width` by `height` terminal and
/// returns its rows.
fn rendered_rows(view: &RemoteControlView, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut page = RemoteControlPage::new(view, Some("agentty".to_string()));
    terminal
        .draw(|frame| {
            let area = frame.area();
            page.render(frame, area);
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

/// Renders the page for `view` on a 100x40 terminal and returns its text.
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

/// Builds a running listener without a relay.
fn listener() -> RemoteListenerView {
    RemoteListenerView {
        pairing_code: "ABCD-EFGH".to_string(),
        relay: None,
        url: "http://127.0.0.1:4646/".to_string(),
    }
}

#[test]
fn running_listener_shows_address_code_devices_and_hints() {
    // Arrange
    let view = RemoteControlView {
        devices: paired_devices(),
        listener: Some(listener()),
        selected_device_index: 1,
        ..RemoteControlView::default()
    };

    // Act
    let text = rendered_text(&view);

    // Assert
    assert!(text.contains("'agentty' remote control"));
    assert!(text.contains("Status  On"));
    assert!(text.contains("http://127.0.0.1:4646/"));
    assert!(text.contains("ABCD-EFGH"));
    assert!(text.contains("Phone"));
    assert!(text.contains("connected"));
    assert!(text.contains("n: new code | Enter: stop | j/k: move | d: remove"));
    assert!(!text.contains("Pairing request"));
}

#[test]
fn page_without_a_project_name_uses_the_plain_title() {
    // Arrange
    let view = RemoteControlView::default();
    let page = RemoteControlPage::new(&view, None);

    // Act
    let title = page.title();

    // Assert
    assert_eq!(title, "Remote control");
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
        listener: Some(listener()),
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
    assert!(text.contains("y: approve | x: deny | n: new code | Enter: stop"));
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
    let page = RemoteControlPage::new(&view, None);

    // Act
    let lines = page.lines(40, 40);

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
fn relay_view(status: RelayStatus, notice: Option<&str>) -> RemoteControlView {
    RemoteControlView {
        listener: Some(RemoteListenerView {
            relay: Some(RemoteRelayView::new(
                "https://relay.example/#code=ABCD".to_string(),
                status,
            )),
            ..listener()
        }),
        notice: notice.map(str::to_string),
        ..RemoteControlView::default()
    }
}

/// Returns the QR code rows of the relay `view` links to.
fn qr_code(view: &RemoteControlView) -> Vec<String> {
    view.listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map(|relay| relay.qr_code.clone())
        .expect("relay should have a QR code")
}

#[test]
fn relay_listener_shows_status_link_qr_code_and_link_hints() {
    // Arrange
    let view = relay_view(RelayStatus::Connected, Some("Link copied. It works once."));
    let page = RemoteControlPage::new(&view, None);

    // Act
    let lines = page.lines(80, 60);
    let text: Vec<String> = lines.iter().map(ToString::to_string).collect();

    // Assert
    let relay_index = text
        .iter()
        .position(|line| line.starts_with("Relay   Connected"))
        .expect("relay row should render");
    assert_eq!(text[relay_index + 1], qr_code(&view)[0]);
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

#[test]
fn rendered_qr_code_keeps_every_row_intact_and_aligned() {
    // Arrange
    let view = relay_view(RelayStatus::Connected, None);
    let qr_code = qr_code(&view);

    // Act
    let rows = rendered_rows(&view, 160, 80);

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
fn rendered_page_keeps_its_hints_when_the_qr_code_barely_fits() {
    // Arrange
    let view = relay_view(RelayStatus::Connected, None);
    let first_qr_row = qr_code(&view).swap_remove(0);

    // Act
    let frames: Vec<(bool, bool)> = (20..90)
        .map(|height| {
            let rows = rendered_rows(&view, 100, height);
            let has_qr_code = rows.iter().any(|row| row.contains(&first_qr_row));
            let has_hints = rows.iter().any(|row| row.contains("Enter: stop"));

            (has_qr_code, has_hints)
        })
        .collect();

    // Assert
    assert!(frames.iter().any(|(has_qr_code, _)| *has_qr_code));
    assert!(frames.iter().any(|(has_qr_code, _)| !*has_qr_code));
    assert!(
        frames
            .iter()
            .all(|(has_qr_code, has_hints)| !has_qr_code || *has_hints)
    );
}

#[test]
fn relay_qr_code_is_left_out_when_it_does_not_fit() {
    // Arrange
    let view = relay_view(RelayStatus::Connecting, None);
    let page = RemoteControlPage::new(&view, None);
    let qr_height = qr_code(&view).len();

    // Act
    let too_short = page.lines(80, qr_height);
    let too_narrow = page.lines(10, 200);

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
fn relay_that_does_not_admit_this_agentty_says_so_in_danger_color() {
    // Arrange
    let view = relay_view(RelayStatus::NotAdmitted, None);
    let page = RemoteControlPage::new(&view, None);

    // Act
    let lines = page.lines(200, 80);

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
fn stopped_listener_shows_error_empty_devices_and_start_hint() {
    // Arrange
    let view = RemoteControlView {
        error: Some("Port 4646 is in use".to_string()),
        ..RemoteControlView::default()
    };

    // Act
    let text = rendered_text(&view);

    // Assert
    assert!(text.contains("Status  Off"));
    assert!(text.contains("Start remote control to follow and steer"));
    assert!(text.contains("Port 4646 is in use"));
    assert!(text.contains("None yet"));
    assert!(text.contains("Enter: start"));
    assert!(!text.contains("Enter: stop"));
    assert!(!text.contains("Code"));
}
