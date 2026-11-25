use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{future, io};

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep, timeout};
use tokio_tungstenite::MaybeTlsStream;

use super::interleave_families;
use crate::dial::{ATTEMPT_DELAY, NetworkClient, RealNetworkClient, TlsTarget, connect};
use crate::test_support::{
    LOOPBACK_GUARD, TLS_FIXTURE_NAMES, TLS_HANDSHAKE_RECORD, TlsFixture, loopback_listener,
    untrusting_tls_config,
};

/// How long an attempt that never completes takes to fail, as a system
/// connect timeout would: an unanswered TCP connect, or a TLS handshake
/// that stalls after the TCP connect.
const UNANSWERED: Duration = Duration::from_secs(75);

/// Network whose resolver returns fixed addresses and whose attempts are
/// the system's own.
struct FixedResolution {
    addresses: Vec<SocketAddr>,
    network: RealNetworkClient,
}

impl NetworkClient for FixedResolution {
    type Stream = MaybeTlsStream<TcpStream>;

    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> {
        future::ready(Ok(self.addresses.clone()))
    }

    fn connect(&self, address: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> {
        self.network.connect(address)
    }
}

/// How a scripted address answers once an attempt to it starts.
#[derive(Clone, Copy)]
enum Answer {
    ConnectAfter(Duration),
    FailAfter(Duration),
}

/// Network whose resolver returns scripted addresses that answer as
/// scripted. A connection is the address it reached.
struct ScriptedNetwork {
    /// Addresses in resolver order with their answers; `None` fails the
    /// resolution.
    answers: Option<Vec<(SocketAddr, Answer)>>,
    origin: Instant,
    /// Attempts whose futures have neither completed nor been dropped.
    running: Mutex<usize>,
    /// Each started attempt with its start time since `origin`.
    started: Mutex<Vec<(SocketAddr, Duration)>>,
}

impl ScriptedNetwork {
    fn new(answers: Option<Vec<(SocketAddr, Answer)>>) -> Self {
        Self {
            answers,
            origin: Instant::now(),
            running: Mutex::new(0),
            started: Mutex::new(Vec::new()),
        }
    }

    fn running(&self) -> usize {
        *self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn started(&self) -> Vec<(SocketAddr, Duration)> {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl NetworkClient for ScriptedNetwork {
    type Stream = SocketAddr;

    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> {
        let addresses = self
            .answers
            .as_ref()
            .map(|answers| answers.iter().map(|(address, _)| *address).collect())
            .ok_or_else(|| io::Error::other("no such host"));

        future::ready(addresses)
    }

    async fn connect(&self, address: SocketAddr) -> io::Result<SocketAddr> {
        let _running = RunningAttempt::start(&self.running);
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((address, self.origin.elapsed()));
        let answer = self
            .answers
            .iter()
            .flatten()
            .find_map(|(scripted, answer)| (*scripted == address).then_some(*answer))
            .expect("only resolved addresses should be dialed");
        match answer {
            Answer::ConnectAfter(delay) => {
                sleep(delay).await;

                Ok(address)
            }
            Answer::FailAfter(delay) => {
                sleep(delay).await;

                Err(io::Error::other("connection failed"))
            }
        }
    }
}

/// Counts one attempt as running from its start until its future completes
/// or is dropped.
struct RunningAttempt<'count>(&'count Mutex<usize>);

impl<'count> RunningAttempt<'count> {
    fn start(count: &'count Mutex<usize>) -> Self {
        *count.lock().unwrap_or_else(PoisonError::into_inner) += 1;

        Self(count)
    }
}

impl Drop for RunningAttempt<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
    }
}

fn v4(last: u8) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::new(192, 0, 2, last), 443))
}

fn v6(last: u16) -> SocketAddr {
    SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, last), 443))
}

fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn tls_target(config: Arc<ClientConfig>, server_name: &str) -> TlsTarget {
    TlsTarget {
        config,
        server_name: ServerName::try_from(server_name)
            .expect("name should be valid")
            .to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn connect_staggers_attempts_and_keeps_the_first_connection() {
    struct Case {
        answers: Vec<(SocketAddr, Answer)>,
        expected: Option<SocketAddr>,
        expected_elapsed: Duration,
        expected_started: Vec<(SocketAddr, Duration)>,
        name: &'static str,
    }
    let cases = [
        Case {
            answers: vec![
                (v6(1), Answer::FailAfter(UNANSWERED)),
                (v4(1), Answer::ConnectAfter(Duration::ZERO)),
            ],
            expected: Some(v4(1)),
            expected_elapsed: ATTEMPT_DELAY,
            expected_started: vec![(v6(1), Duration::ZERO), (v4(1), ATTEMPT_DELAY)],
            name: "an unanswered or stalled first attempt starts the second after the delay",
        },
        Case {
            answers: vec![
                (v6(1), Answer::FailAfter(millis(10))),
                (v4(1), Answer::ConnectAfter(millis(20))),
            ],
            expected: Some(v4(1)),
            expected_elapsed: millis(30),
            expected_started: vec![(v6(1), Duration::ZERO), (v4(1), millis(10))],
            name: "a failed first address starts the second at once",
        },
        Case {
            answers: vec![
                (v6(1), Answer::ConnectAfter(millis(100))),
                (v4(1), Answer::ConnectAfter(Duration::ZERO)),
            ],
            expected: Some(v6(1)),
            expected_elapsed: millis(100),
            expected_started: vec![(v6(1), Duration::ZERO)],
            name: "a first address that connects within the delay is the only attempt",
        },
        Case {
            answers: vec![
                (v6(1), Answer::ConnectAfter(millis(400))),
                (v4(1), Answer::FailAfter(UNANSWERED)),
            ],
            expected: Some(v6(1)),
            expected_elapsed: millis(400),
            expected_started: vec![(v6(1), Duration::ZERO), (v4(1), ATTEMPT_DELAY)],
            name: "an earlier attempt keeps running and still wins",
        },
        Case {
            answers: vec![
                (v6(1), Answer::FailAfter(UNANSWERED)),
                (v6(2), Answer::ConnectAfter(Duration::ZERO)),
                (v4(1), Answer::FailAfter(UNANSWERED)),
            ],
            expected: Some(v6(2)),
            expected_elapsed: ATTEMPT_DELAY * 2,
            expected_started: vec![
                (v6(1), Duration::ZERO),
                (v4(1), ATTEMPT_DELAY),
                (v6(2), ATTEMPT_DELAY * 2),
            ],
            name: "attempts alternate families and start one delay apart",
        },
        Case {
            answers: vec![
                (v6(1), Answer::FailAfter(millis(300))),
                (v4(1), Answer::FailAfter(millis(100))),
            ],
            expected: None,
            expected_elapsed: millis(350),
            expected_started: vec![(v6(1), Duration::ZERO), (v4(1), ATTEMPT_DELAY)],
            name: "the dial fails once every address has failed",
        },
    ];

    for case in cases {
        // Arrange
        let network = ScriptedNetwork::new(Some(case.answers));

        // Act
        let connection = connect(&network, "relay.example", 443).await;

        // Assert
        assert_eq!(connection, case.expected, "{}", case.name);
        assert_eq!(network.started(), case.expected_started, "{}", case.name);
        assert_eq!(
            network.origin.elapsed(),
            case.expected_elapsed,
            "{}",
            case.name
        );
        assert_eq!(
            network.running(),
            0,
            "{}: losing attempts are dropped",
            case.name
        );
    }
}

#[tokio::test]
async fn connect_fails_without_attempts_when_the_host_has_no_address() {
    for answers in [None, Some(Vec::new())] {
        // Arrange
        let network = ScriptedNetwork::new(answers);

        // Act
        let connection = connect(&network, "relay.example", 443).await;

        // Assert
        assert_eq!(connection, None);
        assert_eq!(network.started(), Vec::new());
    }
}

#[tokio::test]
async fn connect_tries_the_next_address_while_tls_stalls_on_an_accepted_connection() {
    // Arrange
    let (stalled, stalled_address) = loopback_listener().await;
    let (next, next_address) = loopback_listener().await;
    let network = FixedResolution {
        addresses: vec![stalled_address, next_address],
        network: RealNetworkClient {
            tls: Some(tls_target(untrusting_tls_config(), "relay.example")),
        },
    };
    let dial = tokio::spawn(async move { connect(&network, "relay.example", 443).await.is_some() });
    let (mut stalled_connection, _) = timeout(LOOPBACK_GUARD, stalled.accept())
        .await
        .expect("dial should reach the first address")
        .expect("dial should connect");
    let mut first_byte = [0; 1];
    stalled_connection
        .read_exact(&mut first_byte)
        .await
        .expect("dial should start TLS");

    // Act
    let next_connection = timeout(LOOPBACK_GUARD, next.accept()).await;
    let is_next_tried = matches!(next_connection, Ok(Ok(_)));
    drop(stalled_connection);
    drop(next_connection);
    let is_connected = timeout(LOOPBACK_GUARD, dial)
        .await
        .expect("dial should finish")
        .expect("dial task should not panic");

    // Assert
    assert_eq!(first_byte, [TLS_HANDSHAKE_RECORD]);
    assert!(is_next_tried);
    assert!(!is_connected);
}

#[tokio::test]
async fn real_network_client_plain_attempt_is_tcp_with_nodelay() {
    // Arrange
    let (_listener, address) = loopback_listener().await;
    let network = RealNetworkClient { tls: None };

    // Act
    let connection = timeout(LOOPBACK_GUARD, network.connect(address))
        .await
        .expect("attempt should finish");

    // Assert
    let nodelay = match &connection {
        Ok(MaybeTlsStream::Plain(stream)) => stream.nodelay().ok(),
        _ => None,
    };
    assert_eq!(nodelay, Some(true));
}

#[tokio::test]
async fn real_network_client_secure_attempt_completes_tls_only_for_the_certified_name() {
    let fixture = TlsFixture::new();
    // Each certified name, a host name or an IP literal, completes TLS over
    // TCP with nodelay; any other name fails verification.
    for (server_name, expected_nodelay) in [
        (TLS_FIXTURE_NAMES[0], Some(true)),
        (TLS_FIXTURE_NAMES[1], Some(true)),
        ("other.example", None),
    ] {
        // Arrange
        let (listener, address) = loopback_listener().await;
        let acceptor = fixture.acceptor.clone();
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await?;

            acceptor.accept(connection).await
        });
        let network = RealNetworkClient {
            tls: Some(tls_target(Arc::clone(&fixture.client), server_name)),
        };

        // Act
        let connection = timeout(LOOPBACK_GUARD, network.connect(address))
            .await
            .expect("attempt should finish");
        let nodelay = match &connection {
            Ok(MaybeTlsStream::Rustls(stream)) => stream.get_ref().0.nodelay().ok(),
            _ => None,
        };
        drop(connection);
        let is_accepted = timeout(LOOPBACK_GUARD, server)
            .await
            .expect("server should finish")
            .expect("server task should not panic")
            .is_ok();

        // Assert
        assert_eq!(nodelay, expected_nodelay, "{server_name}");
        assert_eq!(is_accepted, expected_nodelay.is_some(), "{server_name}");
    }
}

#[test]
fn interleave_families_alternates_from_the_first_family_in_resolver_order() {
    let cases = [
        ("no addresses", vec![], vec![]),
        ("one address", vec![v4(1)], vec![v4(1)]),
        ("one family", vec![v4(1), v4(2)], vec![v4(1), v4(2)]),
        (
            "starts with IPv6",
            vec![v6(1), v6(2), v4(1)],
            vec![v6(1), v4(1), v6(2)],
        ),
        (
            "starts with IPv4 and has more IPv4",
            vec![v4(1), v6(1), v4(2), v4(3), v6(2)],
            vec![v4(1), v6(1), v4(2), v6(2), v4(3)],
        ),
    ];

    for (name, addresses, expected) in cases {
        // Act
        let ordered = interleave_families(addresses);

        // Assert
        assert_eq!(ordered, expected, "{name}");
    }
}
