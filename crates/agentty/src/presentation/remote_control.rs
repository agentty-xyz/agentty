//! Remote-control state projected for the Remote tab.

use qrcode::{Color as QrColor, EcLevel, QrCode};

/// Remote-control listener and paired-device state for one frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteControlView {
    /// Paired devices in pairing order, with their live connection state.
    pub devices: Vec<RemoteDeviceView>,
    /// Failure from the latest remote-control action, when any.
    pub error: Option<String>,
    /// Running listener, or `None` while remote control is off.
    pub listener: Option<RemoteListenerView>,
    /// Confirmation of the latest link action, such as a copy.
    pub notice: Option<String>,
    /// Devices waiting for approval in arrival order.
    pub pairing_requests: Vec<RemotePairingRequestView>,
    /// Highlighted row in `devices`.
    pub selected_device_index: usize,
}

impl RemoteControlView {
    /// Returns how many paired devices are connected now.
    pub fn connected_device_count(&self) -> usize {
        self.devices
            .iter()
            .filter(|device| device.is_connected)
            .count()
    }

    /// Returns the Remote tab's header label, which counts the connected
    /// devices while the listener runs.
    pub fn tab_label(&self) -> String {
        match self.listener {
            Some(_) => format!("Remote ({} online)", self.connected_device_count()),
            None => "Remote".to_string(),
        }
    }
}

/// One running remote-control listener.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteListenerView {
    /// One-time code a new device enters to pair.
    pub pairing_code: String,
    /// Relay link, when remote control registers with a relay.
    pub relay: Option<RemoteRelayView>,
    /// Loopback URL serving the web client.
    pub url: String,
}

/// The relay a running listener registers with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRelayView {
    /// Whether the relay currently accepts devices for this instance.
    pub is_connected: bool,
    /// Pairing link a device opens, or scans as a QR code, to pair through
    /// the relay. It carries the one-time code, so it works once.
    pub link: String,
    /// The link as QR code rows of half-block characters, encoded once per
    /// link; empty when the link cannot be encoded.
    pub qr_code: Vec<String>,
}

impl RemoteRelayView {
    /// Projects `link` and its QR code.
    pub fn new(link: String, is_connected: bool) -> Self {
        Self {
            is_connected,
            qr_code: qr_rows(&link),
            link,
        }
    }
}

/// Light modules around a QR code; scanners need a margin.
const QR_QUIET_ZONE: usize = 2;
/// Cell of two light modules: a no-break space, because wrapping text
/// renderers trim ordinary leading spaces and would shift the rows.
pub const LIGHT_CELL: char = '\u{a0}';

/// Encodes `link` as QR code rows. Each character covers two module rows:
/// `█` both dark, `▀` top dark, `▄` bottom dark, and [`LIGHT_CELL`] both light,
/// with a two-module quiet zone. Returns no rows when the link is too long
/// to encode.
fn qr_rows(link: &str) -> Vec<String> {
    let Ok(code) = QrCode::with_error_correction_level(link.as_bytes(), EcLevel::L) else {
        return Vec::new();
    };
    let modules = code.width();
    let colors = code.to_colors();
    let size = modules + QR_QUIET_ZONE * 2;
    let is_dark = |column: usize, row: usize| {
        let column = column.wrapping_sub(QR_QUIET_ZONE);
        let row = row.wrapping_sub(QR_QUIET_ZONE);

        column < modules && row < modules && colors[row * modules + column] == QrColor::Dark
    };

    (0..size)
        .step_by(2)
        .map(|row| {
            (0..size)
                .map(
                    |column| match (is_dark(column, row), is_dark(column, row + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => LIGHT_CELL,
                    },
                )
                .collect()
        })
        .collect()
}

/// One paired device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteDeviceView {
    /// Stable device identifier used for revocation.
    pub id: String,
    /// Whether the device has an authenticated connection now.
    pub is_connected: bool,
    /// Name the device gave when it paired.
    pub name: String,
}

/// One device that entered the pairing code and waits for approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePairingRequestView {
    /// Code the device also shows, so the user can match them.
    pub confirmation: String,
    /// Name the device gave.
    pub device_name: String,
    /// Request identifier used to approve or deny it.
    pub id: u64,
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
