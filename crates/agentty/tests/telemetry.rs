//! Public telemetry contract and captured HTTP payloads.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use agentty::analytics::{Analytics, SessionType, TurnOutcome};
use agentty::app::AppError;
use agentty::domain::agent::{AgentKind, AgentSelection, AgentSelectionMetadata};
use agentty::infra::db::DbError;
use serde_json::Value;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const INSTALLATION_ID: &str = "4f1c2e8a-5b6d-4c3e-9f0a-1b2c3d4e5f60";

#[test]
fn telemetry_is_enabled_unless_environment_opts_out() {
    // Arrange / Act / Assert
    assert!(Analytics::is_enabled(None));
    assert!(Analytics::is_enabled(Some(OsStr::new("1"))));
    assert!(!Analytics::is_enabled(Some(OsStr::new("0"))));
    assert!(!Analytics::is_enabled(Some(OsStr::new("false"))));
    assert!(!Analytics::is_enabled(Some(OsStr::new(""))));
}

#[tokio::test]
async fn outbound_events_contain_only_allowlisted_properties() -> TestResult<()> {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let host = format!("http://{}/", listener.local_addr()?);
    let receiver = std::thread::spawn(move || -> TestResult<[Value; 2]> {
        Ok([receive_event(&listener)?, receive_event(&listener)?])
    });
    let analytics = Analytics::new("test-token", &host, INSTALLATION_ID)
        .ok_or_else(|| io::Error::other("enabled sender should be configured"))?;
    let error = AppError::Workflow("private path /users/secret and prompt text".to_string());

    // Act
    analytics.record_launch().await;
    analytics.record_failure(&error).await;
    let [launch, failure] = receiver
        .join()
        .map_err(|_| io::Error::other("receiver thread panicked"))??;

    // Assert
    assert_event(&launch, "test-token", "agentty_launch", &[]);
    assert_event(
        &failure,
        "test-token",
        "agentty_failure",
        &[("failure_category", "application")],
    );
    assert!(!failure.to_string().contains("private path"));
    assert!(!failure.to_string().contains("prompt text"));

    Ok(())
}

#[tokio::test]
async fn database_failures_use_a_fixed_category() -> TestResult<()> {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let host = format!("http://{}", listener.local_addr()?);
    let receiver = std::thread::spawn(move || receive_event(&listener));
    let analytics = Analytics::new("token", &host, INSTALLATION_ID)
        .ok_or_else(|| io::Error::other("enabled sender should be configured"))?;
    let error = AppError::Db(DbError::Io(std::io::Error::other("secret database path")));

    // Act
    analytics.record_failure(&error).await;
    let event = receiver
        .join()
        .map_err(|_| io::Error::other("receiver thread panicked"))??;

    // Assert
    assert_event(
        &event,
        "token",
        "agentty_failure",
        &[("failure_category", "database")],
    );
    assert!(!event.to_string().contains("secret database path"));

    Ok(())
}

#[tokio::test]
async fn session_and_turn_lifecycle_events_report_only_bounded_metadata() -> TestResult<()> {
    // Arrange
    let session_types = [
        (SessionType::Regular, "regular"),
        (SessionType::Draft, "draft"),
        (SessionType::Stacked, "stacked"),
        (SessionType::Fork, "fork"),
        (SessionType::Orchestrator, "orchestrator"),
        (SessionType::OrchestrationChild, "orchestration_child"),
        (SessionType::OrchestrationResearch, "orchestration_research"),
    ];
    let agents = [
        AgentKind::Codex,
        AgentKind::Claude,
        AgentKind::Gemini,
        AgentKind::Antigravity,
    ]
    .map(|kind| AgentSelection::new(kind, kind.default_model()));
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let host = format!("http://{}", listener.local_addr()?);
    let receiver = std::thread::spawn(move || -> TestResult<Vec<Value>> {
        (0..19).map(|_| receive_event(&listener)).collect()
    });
    let analytics = Analytics::new("token", &host, INSTALLATION_ID)
        .ok_or_else(|| io::Error::other("configured sender"))?;

    // Act
    for (session_type, _) in session_types {
        analytics
            .record_session_start(session_type, agents[0])
            .await;
    }
    for agent in agents {
        analytics.record_turn_start(agent).await;
        analytics
            .record_turn_end(agent, TurnOutcome::Finished)
            .await;
        analytics
            .record_turn_end(agent, TurnOutcome::Interrupted)
            .await;
    }
    let events = receiver
        .join()
        .map_err(|_| io::Error::other("receiver thread panicked"))??;

    // Assert
    for (event, (_, expected_type)) in events.iter().zip(session_types) {
        assert_event(
            event,
            "token",
            "agentty_session_start",
            &[
                ("session_type", expected_type),
                ("agent", agents[0].kind().name()),
                ("model", agents[0].model().as_str()),
            ],
        );
    }
    for (turn_events, agent) in events[7..].as_chunks::<3>().0.iter().zip(agents) {
        for (event, name) in turn_events.iter().zip([
            "agentty_turn_start",
            "agentty_turn_finish",
            "agentty_turn_interrupt",
        ]) {
            assert_event(
                event,
                "token",
                name,
                &[
                    ("agent", agent.kind().name()),
                    ("model", agent.model().as_str()),
                ],
            );
        }
    }

    Ok(())
}

fn receive_event(listener: &TcpListener) -> TestResult<Value> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::new();
    let mut buffer = [0; 4096];

    loop {
        let count = stream.read(&mut buffer)?;
        assert!(count > 0, "request ended before its body");
        request.extend_from_slice(&buffer[..count]);

        if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let headers = std::str::from_utf8(&request[..header_end])?;
            assert!(headers.starts_with("POST /i/v0/e/ HTTP/1.1"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .ok_or_else(|| io::Error::other("missing content length"))?;
            if request.len() >= header_end + length {
                let event = serde_json::from_slice(&request[header_end..header_end + length])?;
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")?;

                return Ok(event);
            }
        }
    }
}

fn assert_event(event: &Value, token: &str, name: &str, event_properties: &[(&str, &str)]) {
    assert_eq!(event.as_object().map(serde_json::Map::len), Some(4));
    assert_eq!(event["api_key"], token);
    assert_eq!(event["event"], name);
    assert_eq!(event["distinct_id"], INSTALLATION_ID);

    let properties = &event["properties"];
    assert_eq!(properties["$process_person_profile"], false);
    assert_eq!(properties["app_source"], "cli");
    assert_eq!(properties["app_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(properties["install_method"], "unknown");
    for &(key, value) in event_properties {
        assert_eq!(properties[key], value);
    }
    assert_eq!(
        properties.as_object().map(serde_json::Map::len),
        Some(4 + event_properties.len())
    );
}
