//! Remote control server for driving Agentty sessions from another device
//! through a relay.
//!
//! [`start`] registers with the relay that [`RemoteServerConfig::relay`]
//! names and serves the devices that connect through it; it opens no
//! listener of its own. Each device runs the device handshake of `ag-noise`
//! against [`RemoteRelayConfig::instance_key`], so the relay carries only
//! ciphertext, and its own key authenticates it. A new device pairs with a
//! one-time code from [`RemoteServerHandle::issue_pairing_code`], which the
//! host must then approve through [`RemoteServerHandle::approve_pairing`]
//! after comparing the confirmation code both sides show. Authenticated
//! devices exchange [`protocol`] frames that the server translates into
//! [`ag_session::SessionService`] calls. The host keeps device persistence
//! behind [`DeviceStore`] and signals session changes through a revision
//! counter. [`RemoteServerStatus::relay`] reports whether the relay link is
//! up, or whether the relay refused to admit this instance.
//!
//! The web client a device runs is served elsewhere, by a program that
//! embeds the relay; [`pairing_link_on_page`] builds the link that opens it.
//!
//! The `test-utils` feature adds the `test_support` module: an in-memory
//! [`DeviceStore`] and a seeded session backend, so a program that embeds the
//! crate can run a real server in its own tests.

mod connection;
mod device;
mod dial;
mod encrypted;
mod entropy;
mod pairing;
pub mod protocol;
mod relay;
mod request;
mod server;
mod transport;

pub use ag_noise::{PublicKey, StaticKeypair};
pub use device::{DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteDevice};
pub use pairing::PairingCode;
pub use relay::{RelayStatus, RemoteRelayConfig, pairing_link_on_page};
pub use server::{
    PairingRequest, RemoteConnection, RemoteError, RemoteHost, RemoteServerConfig,
    RemoteServerHandle, RemoteServerStatus, start,
};

/// Version of the frame protocol that this server speaks.
///
/// Clients send it in their first frame; the server rejects any other value.
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(any(test, feature = "test-utils"))]
#[path = "host_support_test.rs"]
pub mod test_support;

#[cfg(test)]
#[path = "source_support_test.rs"]
mod source_support;
