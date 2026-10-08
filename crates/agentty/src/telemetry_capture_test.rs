//! Local HTTP capture fixture for lifecycle telemetry tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::analytics::Analytics;

/// Serializes tests that replace the process-global trace provider across
/// awaits.
pub(crate) static TRACER_PROVIDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Configures a local sender and captures exactly the expected event count.
pub(crate) fn capture_events(count: usize) -> (Analytics, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("telemetry listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let host = format!(
        "http://{}",
        listener.local_addr().expect("telemetry address")
    );
    let analytics =
        Analytics::new("test-token", &host, "test-installation").expect("local telemetry sender");
    let receiver = thread::spawn(move || (0..count).map(|_| receive_event(&listener)).collect());

    (analytics, receiver)
}

#[test]
fn capture_response_disables_reuse_of_closed_connections() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("telemetry listener");
    let address = listener.local_addr().expect("telemetry address");
    let event = json!({ "event": "agentty_launch" });
    let body = serde_json::to_vec(&event).expect("event JSON");
    let receiver = thread::spawn(move || receive_event(&listener));
    let mut client = TcpStream::connect(address).expect("telemetry connection");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");

    // Act
    write!(
        client,
        "POST /i/v0/e/ HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .expect("request headers");
    client.write_all(&body).expect("request body");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("response followed by connection closure");
    let captured_event = receiver.join().expect("telemetry receiver");

    // Assert
    let (headers, response_body) = response.split_once("\r\n\r\n").expect("HTTP response");
    assert!(
        headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("Connection: close")),
        "a receiver that closes after one request must disable connection reuse"
    );
    assert_eq!(response_body, "{}");
    assert_eq!(captured_event, event);
}

fn receive_event(listener: &TcpListener) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut stream, _) = loop {
        let connection = listener.accept();
        if connection
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));

            continue;
        }

        break connection.expect("telemetry connection before deadline");
    };
    stream.set_nonblocking(false).expect("blocking stream");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("stream timeout");
    let mut request = Vec::new();
    let mut buffer = [0; 4096];

    loop {
        let count = stream.read(&mut buffer).expect("telemetry request");
        assert!(count > 0, "telemetry request ended before body");
        request.extend_from_slice(&buffer[..count]);
        let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let header_end = header_end + 4;
        let headers = std::str::from_utf8(&request[..header_end]).expect("request headers");
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .expect("content length");
        if request.len() >= header_end + length {
            let event = serde_json::from_slice(&request[header_end..header_end + length])
                .expect("event JSON");
            // Each socket serves one request, so pooled senders must not reuse
            // it.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .expect("telemetry response");

            return event;
        }
    }
}
