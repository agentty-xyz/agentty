//! Public retry contract of built-in clients: each retry a provider's
//! `RetryPolicy` sends streams as a `Retrying` event before the run
//! completes, and cancelling a run during a retry's wait stops it.

use std::error::Error;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ag_harness::model::ModelClient;
use ag_harness::provider::{ProviderSpec, RetryKind, RetryPolicy, WireProtocol};
use ag_harness::{
    ErrorKind, EventStream, Harness, OutputSchema, RunEvent, RunEventKind, RunOutcome, RunRequest,
};
use futures_core::Stream;
use serde_json::json;
use tokio::time::timeout;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::context_budget_fixture::unbounded_context_budget;

/// Bounds waits that would hang if a run never resolved.
const HANG: Duration = Duration::from_secs(30);

/// Returns a harness whose model is served at `base_url` under `retry`.
fn harness(base_url: String, retry: RetryPolicy) -> Result<Harness, Box<dyn Error>> {
    let spec = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, base_url)
        .for_model("gateway-model")
        .with_retry(retry);

    Ok(Harness::builder()
        .model(Arc::new(ModelClient::from_spec(spec)?))
        .context_budget(unbounded_context_budget())
        .build()?)
}

fn request() -> Result<RunRequest, Box<dyn Error>> {
    Ok(
        RunRequest::new("Find the name").output(OutputSchema::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        }))?),
    )
}

/// Waits for the next event through the `Stream` implementation.
async fn next_event(stream: &mut EventStream) -> Option<RunEvent> {
    poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = next_event(&mut stream).await {
        events.push(event);
    }

    events
}

/// Returns the `(attempt, kind, after)` of each `Retrying` event.
fn retries(events: &[RunEvent]) -> Vec<(u32, ErrorKind, Duration)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Retrying {
                after,
                attempt,
                kind,
                ..
            } => Some((*attempt, *kind, *after)),
            _ => None,
        })
        .collect()
}

/// Returns the failure kind of a settled outcome.
fn failure(outcome: RunOutcome) -> Option<ErrorKind> {
    outcome.result.err().map(|error| error.kind())
}

#[tokio::test]
async fn streams_each_retry_before_the_run_completes() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    for status in [429, 529] {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "{\"name\":\"Ada\"}"}}]
        })))
        .mount(&server)
        .await;
    let retry = RetryPolicy::default()
        .with_retried(RetryKind::Overloaded, true)
        .with_base_delay(Duration::from_millis(1));
    let harness = harness(format!("{}/v1", server.uri()), retry)?;

    // Act
    let (stream, handle) = harness.run(request()?).into_parts();
    let (events, outcome) = timeout(HANG, async {
        tokio::join!(drain(stream), handle.outcome())
    })
    .await?;

    // Assert
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(
        retries(&events),
        [
            (1, ErrorKind::RateLimited, Duration::from_millis(1)),
            (2, ErrorKind::Overloaded, Duration::from_millis(2)),
        ]
    );
    let last_retrying = events
        .iter()
        .rposition(|event| matches!(event.kind, RunEventKind::Retrying { .. }))
        .ok_or("a retry is streamed")?;
    let requested = events
        .iter()
        .position(|event| matches!(event.kind, RunEventKind::ModelRequestCompleted { .. }))
        .ok_or("the request completes")?;
    assert!(
        last_retrying < requested,
        "every retry precedes the request's completion"
    );
    assert!(matches!(
        events.last().map(|event| &event.kind),
        Some(RunEventKind::Completed(summary)) if summary.error.is_none()
    ));

    Ok(())
}

#[tokio::test]
async fn streams_a_connection_retry_before_the_failed_completion() -> Result<(), Box<dyn Error>> {
    // Arrange
    let refused = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let retry = RetryPolicy::default().with_base_delay(Duration::from_millis(1));
    let harness = harness(format!("http://{refused}/v1"), retry)?;

    // Act
    let (stream, handle) = harness.run(request()?).into_parts();
    let (events, outcome) = timeout(HANG, async {
        tokio::join!(drain(stream), handle.outcome())
    })
    .await?;

    // Assert
    assert_eq!(failure(outcome), Some(ErrorKind::Transport));
    assert_eq!(
        retries(&events),
        [(1, ErrorKind::Transport, Duration::from_millis(1))]
    );
    assert!(matches!(
        events.last().map(|event| &event.kind),
        Some(RunEventKind::Completed(summary))
            if summary.error.as_ref().map(|error| error.kind) == Some(ErrorKind::Transport)
    ));

    Ok(())
}

#[tokio::test]
async fn cancelling_during_a_retry_wait_stops_the_run() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let retry = RetryPolicy::default()
        .with_base_delay(Duration::from_hours(1))
        .with_max_backoff(Duration::from_hours(1));
    let harness = harness(format!("{}/v1", server.uri()), retry)?;
    let (mut stream, handle) = harness.run(request()?).into_parts();
    let cancel = handle.cancel_token();
    let outcome = tokio::spawn(handle.outcome());

    // Act
    let retrying = timeout(HANG, async {
        while let Some(event) = next_event(&mut stream).await {
            if matches!(event.kind, RunEventKind::Retrying { .. }) {
                return Some(event);
            }
        }

        None
    })
    .await?
    .ok_or("the run streams its retry")?;
    cancel.cancel();
    let outcome = timeout(HANG, outcome).await??;

    // Assert
    assert_eq!(
        retries(&[retrying]),
        [(1, ErrorKind::RateLimited, Duration::from_hours(1))]
    );
    assert_eq!(failure(outcome), Some(ErrorKind::Cancelled));
    let requests = server.received_requests().await.ok_or("recording on")?;
    assert_eq!(requests.len(), 1, "a cancelled retry must not be sent");

    Ok(())
}
