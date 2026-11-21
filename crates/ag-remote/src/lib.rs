//! Remote control server for driving Agentty sessions from another device.
//!
//! [`start`] binds an HTTP listener that serves an embedded web client and one
//! WebSocket endpoint at `/ws`. Clients authenticate with a device token, or
//! pair with a one-time code from [`RemoteServerHandle::issue_pairing_code`],
//! then exchange [`protocol`] frames that the server translates into
//! [`ag_session::SessionService`] calls. The host keeps device persistence
//! behind [`DeviceStore`] and signals session changes through a revision
//! counter.

mod connection;
mod device;
mod entropy;
mod pairing;
pub mod protocol;
mod server;

pub use device::{DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteDevice};
pub use pairing::PairingCode;
pub use server::{
    RemoteConnection, RemoteError, RemoteHost, RemoteServerConfig, RemoteServerHandle,
    RemoteServerStatus, start,
};

/// Version of the frame protocol that this server speaks.
///
/// Clients send it in their first frame; the server rejects any other value.
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
#[path = "test_support_test.rs"]
mod test_support;
