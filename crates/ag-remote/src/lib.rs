//! Remote control server for driving Agentty sessions from another device.
//!
//! [`start`] binds an HTTP listener that serves an embedded web client and one
//! WebSocket endpoint at `/ws`, answering only requests whose `Host` is
//! loopback or explicitly allowed. Clients authenticate with a device token,
//! or pair with a one-time code from [`RemoteServerHandle::issue_pairing_code`]
//! that the host must then approve through
//! [`RemoteServerHandle::approve_pairing`]. Authenticated clients exchange
//! [`protocol`] frames that the server translates into
//! [`ag_session::SessionService`] calls. The host keeps device persistence
//! behind [`DeviceStore`] and signals session changes through a revision
//! counter.
//!
//! With [`RemoteServerConfig::relay`] set, the server also registers with a
//! relay and serves devices that connect through it. Such a device needs no
//! route to the listener. It runs the device handshake of `ag-noise` against
//! [`RemoteRelayConfig::instance_key`], so the relay carries only ciphertext,
//! and it pairs with the same one-time code and host approval. Its own key,
//! not a token, authenticates it afterwards. [`RemoteServerStatus::relay`]
//! reports whether the relay link is up.

mod connection;
mod device;
mod encrypted;
mod entropy;
mod pairing;
pub mod protocol;
mod relay;
mod server;
mod transport;

pub use ag_noise::{PublicKey, StaticKeypair};
pub use device::{DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteDevice};
pub use pairing::PairingCode;
pub use relay::{RelayStatus, RemoteRelayConfig, pairing_link};
pub use server::{
    PairingRequest, RemoteConnection, RemoteError, RemoteHost, RemoteServerConfig,
    RemoteServerHandle, RemoteServerStatus, start,
};

/// Version of the frame protocol that this server speaks.
///
/// Clients send it in their first frame; the server rejects any other value.
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
#[path = "test_support_test.rs"]
mod test_support;
