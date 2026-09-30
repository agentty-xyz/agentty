//! Local HTTP capture fixture for lifecycle telemetry tests.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::analytics::Analytics;

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
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .expect("telemetry response");

            return event;
        }
    }
}
