//! Remote-control state projected for the Platform tab.

use ag_remote::RelayStatus;
use qrcode::{Color as QrColor, EcLevel, QrCode};

/// Remote-control pairing and paired-device state for one frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteControlView {
    /// Paired devices in pairing order, with their live connection state.
    pub devices: Vec<RemoteDeviceView>,
    /// Failure from the latest remote-control action, when any.
    pub error: Option<String>,
    /// Confirmation of the latest link action, such as a copy.
    pub notice: Option<String>,
    /// Devices waiting for approval in arrival order.
    pub pairing_requests: Vec<RemotePairingRequestView>,
    /// The relay link and its pairing link while remote control runs, or
    /// `None` while it is off.
    pub relay: Option<RemoteRelayView>,
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
}

/// The relay that running remote control registers with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRelayView {
    /// Pairing link a device opens, or scans as a QR code, to pair through
    /// the relay. It carries the one-time code, so it works once.
    pub link: String,
    /// The link as QR code rows of half-block characters, encoded once per
    /// link; empty when the link cannot be encoded.
    pub qr_code: Vec<String>,
    /// Whether the relay accepts devices for this instance now, is being
    /// connected to, or refused to admit this instance.
    pub status: RelayStatus,
}

impl RemoteRelayView {
    /// Projects `link` and its QR code.
    pub fn new(link: String, status: RelayStatus) -> Self {
        Self {
            qr_code: qr_rows(&link),
            link,
            status,
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
