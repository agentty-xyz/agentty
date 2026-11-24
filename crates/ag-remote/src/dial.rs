//! Connections to a host that may publish several addresses.
//!
//! [`connect`] resolves the host and staggers connection attempts across its
//! addresses in the style of RFC 8305 (Happy Eyeballs): it alternates
//! address families, starts the next attempt when any running attempt
//! fails or [`ATTEMPT_DELAY`] after the latest one started, and keeps the
//! first connection that opens. An address that never answers, such as one
//! behind an IPv6 route that drops packets, therefore cannot keep the dial from
//! reaching another address of the same host.
//!
//! On a secure connection an attempt includes the TLS handshake, not only
//! the TCP connect: a route can accept the TCP connection and then stall,
//! for example when it drops the server's large certificate flight, and
//! such an address must lose to one that completes TLS.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::MaybeTlsStream;

/// Wait after one connection attempt starts before the next one starts,
/// unless a running attempt fails sooner. RFC 8305 recommends 250 ms.
pub(crate) const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Name resolution and connection attempts: the network access a dial
/// needs.
pub(crate) trait NetworkClient {
    /// Connection that a successful attempt yields.
    type Stream;

    /// Resolves `host` to the addresses to try at `port`, in the resolver's
    /// order.
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;

    /// Runs one connection attempt to `address`, which succeeds only once
    /// the connection is ready to carry data.
    async fn connect(&self, address: SocketAddr) -> io::Result<Self::Stream>;
}

/// Network access through the system resolver and sockets. Each attempt
/// opens a TCP connection with `TCP_NODELAY` and, for a secure connection,
/// completes the TLS handshake on it.
pub(crate) struct RealNetworkClient {
    /// TLS each attempt completes before it succeeds; `None` for plain TCP.
    pub(crate) tls: Option<TlsTarget>,
}

impl NetworkClient for RealNetworkClient {
    type Stream = MaybeTlsStream<TcpStream>;

    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(tokio::net::lookup_host((host, port)).await?.collect())
    }

    async fn connect(&self, address: SocketAddr) -> io::Result<Self::Stream> {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        let Some(tls) = &self.tls else {
            return Ok(MaybeTlsStream::Plain(stream));
        };
        let connector = TlsConnector::from(Arc::clone(&tls.config));

        connector
            .connect(tls.server_name.clone(), stream)
            .await
            .map(MaybeTlsStream::Rustls)
    }
}

/// TLS settings and the server name to verify for a secure connection.
pub(crate) struct TlsTarget {
    pub(crate) config: Arc<ClientConfig>,
    pub(crate) server_name: ServerName<'static>,
}

/// Connects to `host` at `port` through `network`, trying its addresses as
/// the module describes. Attempts still running when one connects are
/// dropped.
///
/// Returns `None` when resolution fails or every address fails. There is no
/// overall time limit: callers bound the whole dial.
pub(crate) async fn connect<Network: NetworkClient>(
    network: &Network,
    host: &str,
    port: u16,
) -> Option<Network::Stream> {
    let addresses = network.resolve(host, port).await.ok()?;
    let mut remaining = interleave_families(addresses).into_iter().peekable();
    let mut attempts = FuturesUnordered::new();

    loop {
        if let Some(address) = remaining.next() {
            attempts.push(network.connect(address));
        }
        let finished = if remaining.peek().is_some() {
            tokio::select! {
                biased;
                finished = attempts.next() => finished,
                () = sleep(ATTEMPT_DELAY) => continue,
            }
        } else {
            attempts.next().await
        };
        match finished {
            Some(Ok(stream)) => return Some(stream),
            Some(Err(_)) => {}
            None => return None,
        }
    }
}

/// Orders `addresses` by alternating address families, starting with the
/// family of the first one and keeping the resolver's order within each
/// family.
fn interleave_families(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let first_is_ipv6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let address_count = addresses.len();
    let (preferred, other): (Vec<_>, Vec<_>) = addresses
        .into_iter()
        .partition(|address| address.is_ipv6() == first_is_ipv6);
    let mut preferred = preferred.into_iter();
    let mut other = other.into_iter();
    let mut ordered = Vec::with_capacity(address_count);

    while ordered.len() < address_count {
        ordered.extend(preferred.next());
        ordered.extend(other.next());
    }

    ordered
}

#[cfg(test)]
#[path = "dial_test.rs"]
mod tests;
