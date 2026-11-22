//! Running-relay fixture with WebSocket clients for instances and devices.

use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use ag_noise::{Handshake, StaticKeypair};
use ag_relay::{
    ACCEPT_PATH_PREFIX, CONNECT_PATH_PREFIX, REGISTER_PATH, REGISTERED_MESSAGE, RelayConfig,
    RelayHandle, RelayLimits, STREAM_MESSAGE_PREFIX,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// Client side of one relay WebSocket.
pub(crate) type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Result type for tests and the helpers they share.
pub(crate) type TestResult<Value = ()> = Result<Value, Box<dyn Error>>;

/// Longest wait for an expected message before a test fails.
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts a relay on a free loopback port.
pub(crate) async fn start(limits: RelayLimits) -> TestResult<RelayHandle> {
    start_with_header(limits, None).await
}

/// Starts a relay that reads client addresses from `header`.
pub(crate) async fn start_with_header(
    limits: RelayLimits,
    header: Option<&str>,
) -> TestResult<RelayHandle> {
    let relay = ag_relay::start(RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: header.map(str::to_string),
        limits,
    })
    .await?;

    Ok(relay)
}

/// Generates an instance key pair.
pub(crate) fn keypair() -> TestResult<StaticKeypair> {
    Ok(StaticKeypair::generate()?)
}

/// Opens a WebSocket to `path` on the relay.
pub(crate) async fn open(relay: &RelayHandle, path: &str) -> TestResult<Client> {
    open_at(relay.local_addr(), path).await
}

/// Opens a WebSocket to `path` on the relay listening at `address`.
pub(crate) async fn open_at(address: SocketAddr, path: &str) -> TestResult<Client> {
    let (client, _) = connect_async(format!("ws://{address}{path}")).await?;

    Ok(client)
}

/// Opens a control connection that claims to come from `address`.
pub(crate) async fn open_control_from(
    relay: &RelayHandle,
    address: &'static str,
) -> TestResult<Client> {
    let mut request =
        format!("ws://{}{REGISTER_PATH}", relay.local_addr()).into_client_request()?;
    request
        .headers_mut()
        .insert("x-real-ip", HeaderValue::from_static(address));
    let (client, _) = connect_async(request).await?;

    Ok(client)
}

/// Runs the instance side of the registration handshake on `control`.
pub(crate) async fn prove_key(control: &mut Client, instance: &StaticKeypair) -> TestResult {
    let mut handshake = Handshake::relay_initiator(instance)?;
    send_binary(control, handshake.write_message()?).await?;
    let second = next_binary(control).await?;
    handshake.read_message(&second)?;
    send_binary(control, handshake.write_message()?).await?;

    Ok(())
}

/// Registers `instance` and returns its confirmed control connection.
pub(crate) async fn register(relay: &RelayHandle, instance: &StaticKeypair) -> TestResult<Client> {
    register_at(relay.local_addr(), instance).await
}

/// Registers `instance` with the relay listening at `address`.
pub(crate) async fn register_at(
    address: SocketAddr,
    instance: &StaticKeypair,
) -> TestResult<Client> {
    let mut control = open_at(address, REGISTER_PATH).await?;
    prove_key(&mut control, instance).await?;
    let confirmation = next_text(&mut control).await?;
    if confirmation != REGISTERED_MESSAGE {
        return Err(format!("unexpected confirmation: {confirmation}").into());
    }

    Ok(control)
}

/// Proves `instance` to the relay listening at `address` and returns the
/// code and reason of the close frame that refuses its registration.
pub(crate) async fn refused_registration(
    address: SocketAddr,
    instance: &StaticKeypair,
) -> TestResult<(u16, String)> {
    let mut control = open_at(address, REGISTER_PATH).await?;
    prove_key(&mut control, instance).await?;

    next_close(&mut control).await
}

/// Opens a device stream toward `instance`.
pub(crate) async fn connect_device(
    relay: &RelayHandle,
    instance: &StaticKeypair,
) -> TestResult<Client> {
    let path = format!("{CONNECT_PATH_PREFIX}{}", instance.public().fingerprint());

    open(relay, &path).await
}

/// Reads the next stream notice on `control` and answers the stream.
pub(crate) async fn accept_next_stream(
    relay: &RelayHandle,
    control: &mut Client,
) -> TestResult<Client> {
    let stream_id = next_stream_id(control).await?;

    open(relay, &format!("{ACCEPT_PATH_PREFIX}{stream_id}")).await
}

/// Reads the next stream notice on `control`.
pub(crate) async fn next_stream_id(control: &mut Client) -> TestResult<String> {
    let notice = next_text(control).await?;
    let stream_id = notice
        .strip_prefix(STREAM_MESSAGE_PREFIX)
        .ok_or("expected a stream notice")?;

    Ok(stream_id.to_string())
}

/// Returns a registered instance's control connection with one device
/// stream joined: `(control, device, instance)`.
pub(crate) async fn joined_stream(relay: &RelayHandle) -> TestResult<(Client, Client, Client)> {
    let instance_key = keypair()?;
    let mut control = register(relay, &instance_key).await?;
    let device = connect_device(relay, &instance_key).await?;
    let instance = accept_next_stream(relay, &mut control).await?;

    Ok((control, device, instance))
}

/// Sends one binary message.
pub(crate) async fn send_binary(client: &mut Client, bytes: Vec<u8>) -> TestResult {
    client.send(Message::Binary(bytes.into())).await?;

    Ok(())
}

/// Returns the next message that is not keepalive traffic.
pub(crate) async fn next_message(client: &mut Client) -> TestResult<Message> {
    loop {
        let message = tokio::time::timeout(MESSAGE_TIMEOUT, client.next())
            .await?
            .ok_or("connection ended")??;
        if !matches!(message, Message::Ping(_) | Message::Pong(_)) {
            return Ok(message);
        }
    }
}

/// Returns the next data message, which must be binary.
pub(crate) async fn next_binary(client: &mut Client) -> TestResult<Vec<u8>> {
    match next_message(client).await? {
        Message::Binary(bytes) => Ok(bytes.to_vec()),
        other => Err(format!("expected a binary message, got {other:?}").into()),
    }
}

/// Returns the next data message, which must be text.
pub(crate) async fn next_text(client: &mut Client) -> TestResult<String> {
    match next_message(client).await? {
        Message::Text(text) => Ok(text.to_string()),
        other => Err(format!("expected a text message, got {other:?}").into()),
    }
}

/// Returns the code and reason of the close frame that arrives next.
pub(crate) async fn next_close(client: &mut Client) -> TestResult<(u16, String)> {
    match next_message(client).await? {
        Message::Close(Some(frame)) => Ok((frame.code.into(), frame.reason.to_string())),
        other => Err(format!("expected a close frame, got {other:?}").into()),
    }
}

/// Waits for the first keepalive ping.
pub(crate) async fn next_ping(client: &mut Client) -> TestResult {
    loop {
        let message = tokio::time::timeout(MESSAGE_TIMEOUT, client.next())
            .await?
            .ok_or("connection ended")??;
        if matches!(message, Message::Ping(_)) {
            return Ok(());
        }
    }
}

/// Status code of a refused WebSocket upgrade to `path`.
pub(crate) async fn refused_status(relay: &RelayHandle, path: &str) -> TestResult<u16> {
    match open(relay, path).await {
        Ok(_) => Err("upgrade should be refused".into()),
        Err(error) => match error.downcast_ref() {
            Some(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                Ok(response.status().as_u16())
            }
            _ => Err(error),
        },
    }
}
