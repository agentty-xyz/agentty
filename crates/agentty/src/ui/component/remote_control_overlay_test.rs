//! Remote-control popup listener, device, error, and hint rendering tests.

use super::RemoteControlOverlay;
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView,
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
            url: "http://127.0.0.1:4646/".to_string(),
        }),
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
    let lines = overlay.lines(40);

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
fn stopped_listener_shows_error_empty_devices_and_start_hint() {
    // Arrange
    let view = RemoteControlView {
        devices: Vec::new(),
        error: Some("Port 4646 is in use".to_string()),
        listener: None,
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
