//! Remote-control state projected for the overlay and the status bar.

/// Remote-control listener and paired-device state for one frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteControlView {
    /// Paired devices in pairing order, with their live connection state.
    pub devices: Vec<RemoteDeviceView>,
    /// Failure from the latest remote-control action, when any.
    pub error: Option<String>,
    /// Running listener, or `None` while remote control is off.
    pub listener: Option<RemoteListenerView>,
}

impl RemoteControlView {
    /// Returns how many paired devices are connected now.
    pub fn connected_device_count(&self) -> usize {
        self.devices
            .iter()
            .filter(|device| device.is_connected)
            .count()
    }

    /// Returns the connected device count while the listener runs, for the
    /// status-bar notice, or `None` while remote control is off.
    pub fn status_bar_connections(&self) -> Option<usize> {
        self.listener
            .as_ref()
            .map(|_| self.connected_device_count())
    }
}

/// One running remote-control listener.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteListenerView {
    /// One-time code a new device enters to pair.
    pub pairing_code: String,
    /// Loopback URL serving the web client.
    pub url: String,
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

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
