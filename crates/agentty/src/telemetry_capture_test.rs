//! Local HTTP capture fixture for lifecycle telemetry tests, and in-memory
//! span capture for workflow trace tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ag_telemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{Value, json};
use tokio::sync::MutexGuard;

use crate::analytics::Analytics;

/// Serializes tests that replace the process-global trace provider across
/// awaits.
pub(crate) static TRACER_PROVIDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Installs an in-memory exporter as the process-global trace provider. The
/// returned guard holds [`TRACER_PROVIDER_LOCK`] for the test's duration.
pub(crate) async fn capture_spans() -> (MutexGuard<'static, ()>, InMemorySpanExporter) {
    let guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );

    (guard, exporter)
}

/// Returns the only finished span named `name` whose parent is `parent`.
pub(crate) fn finished_span(
    exporter: &InMemorySpanExporter,
    name: &str,
    parent: SpanId,
) -> SpanData {
    let spans = exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .filter(|span| span.name == name && span.parent_span_id == parent)
        .collect::<Vec<_>>();
    assert_eq!(spans.len(), 1, "expected one `{name}` span: {spans:?}");

    spans.into_iter().next().expect("one span")
}

/// Waits for [`finished_span`] when the span ends in a background task after
/// its observable effects.
pub(crate) async fn wait_for_finished_span(
    exporter: &InMemorySpanExporter,
    name: &str,
    parent: SpanId,
) -> SpanData {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !exporter
            .get_finished_spans()
            .expect("finished spans")
            .iter()
            .any(|span| span.name == name && span.parent_span_id == parent)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("span must finish");

    finished_span(exporter, name, parent)
}

/// Asserts a span ended with `outcome` and records exactly the expected
/// attributes besides its terminal classification, so no content leaks.
pub(crate) fn assert_span_attributes(span: &SpanData, outcome: &str, expected: &[KeyValue]) {
    let mut attributes = span
        .attributes
        .iter()
        .filter(|attribute| {
            attribute.key.as_str() != "agentty.outcome" && attribute.key.as_str() != "error.type"
        })
        .cloned()
        .collect::<Vec<_>>();
    attributes.sort_by(|left, right| left.key.as_str().cmp(right.key.as_str()));
    let mut expected = expected.to_vec();
    expected.sort_by(|left, right| left.key.as_str().cmp(right.key.as_str()));

    assert!(
        span.attributes
            .contains(&KeyValue::new("agentty.outcome", outcome.to_string())),
        "`{}` outcome: {:?}",
        span.name,
        span.attributes
    );
    assert_eq!(attributes, expected, "`{}` attributes", span.name);
}

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
