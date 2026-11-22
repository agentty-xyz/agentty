//! Relay that joins remote-control devices to Agentty instances.
//!
//! An instance keeps one outbound control connection to the relay and dials
//! one more connection for each device, so it never accepts inbound traffic.
//! Devices and instances encrypt end to end; the relay copies bytes it cannot
//! read and keeps no state beyond the open connections.
//!
//! [`start`] serves these WebSocket endpoints:
//!
//! - [`REGISTER_PATH`]: an instance proves its key with the relay handshake of
//!   `ag-noise`, sent as three binary messages (instance, relay, instance). The
//!   relay answers with the text message [`REGISTERED_MESSAGE`], then sends one
//!   text message of [`STREAM_MESSAGE_PREFIX`] followed by a stream identifier
//!   for each device that connects. The instance identifier is the fingerprint
//!   of the proven key. Registering again replaces the earlier control
//!   connection.
//! - [`CONNECT_PATH_PREFIX`] followed by an instance identifier: a device opens
//!   a stream. The upgrade is refused with `404` when no such instance is
//!   registered.
//! - [`ACCEPT_PATH_PREFIX`] followed by a stream identifier: the instance
//!   answers a stream. The upgrade is refused with `404` for an unknown or
//!   already answered stream.
//!
//! Once a stream is answered the relay forwards binary messages in both
//! directions until either side leaves, and closes both sides with the same
//! reason. `GET /healthz` answers `ok` for liveness probes.
//!
//! [`RelayLimits`] bounds what clients hold once a request reaches the
//! relay. It does not cap plain TCP connections or slow request headers, and
//! the relay speaks unencrypted HTTP: run it behind a reverse proxy that
//! terminates TLS and limits connections.

mod close;
mod control;
mod limit;
mod peer;
mod registry;
mod server;
mod stream;

pub use limit::RelayLimits;
pub use server::{
    ACCEPT_PATH_PREFIX, CONNECT_PATH_PREFIX, REGISTER_PATH, REGISTERED_MESSAGE, RelayConfig,
    RelayError, RelayHandle, STREAM_MESSAGE_PREFIX, start,
};
