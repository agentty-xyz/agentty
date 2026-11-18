//! HTTP transport tests for retries under a provider's [`RetryPolicy`]:
//! retried and final statuses, `Retry-After` forms and caps, exhausted
//! attempts, observer reports, and cancellation during a wait.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use tokio::sync::Notify;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{HttpTransport, ProviderRequest, RetryClock, TransportError};
use crate::retry::{Retry, RetryKind, RetryObserver, RetryPolicy};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Records every retry it is told about, and wakes a waiter on the first.
#[derive(Default)]
struct RecordingObserver {
    reported: Notify,
    retries: Mutex<Vec<Retry>>,
}

impl RecordingObserver {
    /// Returns the retries reported so far.
    fn retries(&self) -> Vec<Retry> {
        self.retries.lock().expect("retries lock").clone()
    }
}

#[async_trait]
impl RetryObserver for RecordingObserver {
    async fn retrying(&self, retry: Retry) {
        self.retries.lock().expect("retries lock").push(retry);
        self.reported.notify_one();
    }
}

/// Fixed wall-clock time HTTP dates are read against.
fn now() -> SystemTime {
    httpdate::parse_http_date("Wed, 21 Oct 2026 07:28:00 GMT").expect("fixture date parses")
}

/// Returns a transport whose retries never wait, read `now` as the time,
/// and sample jitter at its midpoint.
fn instant_transport() -> HttpTransport {
    HttpTransport {
        clock: RetryClock {
            jitter: || 0.5,
            now,
            sleep: |_| tokio::time::sleep(Duration::ZERO),
        },
        ..HttpTransport::new(REQUEST_TIMEOUT)
    }
}

/// Returns a request to `server` retried under `policy`, reported to
/// `observer`.
fn request(
    server: &MockServer,
    policy: RetryPolicy,
    observer: &Arc<RecordingObserver>,
) -> ProviderRequest {
    let observer: Arc<dyn RetryObserver> = observer.clone();

    ProviderRequest::new(HeaderMap::new(), server.uri(), json!({}))
        .with_retry(policy, Some(observer))
}

/// Answers the next `times` requests to `server` with `response`.
async fn respond(server: &MockServer, response: ResponseTemplate, times: u64) {
    Mock::given(method("POST"))
        .respond_with(response)
        .up_to_n_times(times)
        .mount(server)
        .await;
}

/// Answers every later request to `server` with a JSON success.
async fn succeed(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(server)
        .await;
}

/// Returns a policy that retries every kind.
fn retry_everything() -> RetryPolicy {
    RetryPolicy::default()
        .with_retried(RetryKind::ServerError, true)
        .with_retried(RetryKind::Overloaded, true)
}

#[tokio::test]
async fn retries_each_opted_in_status_then_succeeds() {
    // Arrange
    let cases = [
        (429, RetryKind::RateLimited),
        (500, RetryKind::ServerError),
        (502, RetryKind::ServerError),
        (503, RetryKind::ServerError),
        (504, RetryKind::ServerError),
        (529, RetryKind::Overloaded),
    ];

    for (status, kind) in cases {
        let server = MockServer::start().await;
        respond(&server, ResponseTemplate::new(status), 1).await;
        succeed(&server).await;
        let observer = Arc::new(RecordingObserver::default());

        // Act
        let body = instant_transport()
            .post_json::<Value>(&request(&server, retry_everything(), &observer))
            .await
            .expect("the retry should succeed");

        // Assert
        assert_eq!(body, json!({"ok": true}), "{status}");
        assert_eq!(
            observer.retries(),
            vec![Retry {
                attempt: 1,
                delay: Duration::from_secs(1),
                kind,
                status: Some(status),
            }],
            "{status}"
        );
    }
}

#[tokio::test]
async fn fails_at_once_on_statuses_the_policy_does_not_retry() {
    // Arrange
    let quota =
        ResponseTemplate::new(429).set_body_string(r#"{"error":{"code":"insufficient_quota"}}"#);
    let cases = [
        (RetryPolicy::default(), ResponseTemplate::new(500), 500),
        (RetryPolicy::default(), ResponseTemplate::new(529), 529),
        (RetryPolicy::none(), ResponseTemplate::new(429), 429),
        (retry_everything(), quota, 429),
        (retry_everything(), ResponseTemplate::new(400), 400),
        (retry_everything(), ResponseTemplate::new(501), 501),
    ];

    for (policy, response, status) in cases {
        let server = MockServer::start().await;
        respond(&server, response, 1).await;
        succeed(&server).await;
        let observer = Arc::new(RecordingObserver::default());

        // Act
        let error = instant_transport()
            .post_json::<Value>(&request(&server, policy, &observer))
            .await
            .expect_err("an unretried status should fail");

        // Assert
        assert!(
            matches!(&error, TransportError::Http { status: got, .. } if got.as_u16() == status),
            "{error:?}"
        );
        assert!(observer.retries().is_empty(), "{status}");
        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 1, "{status}");
    }
}

#[tokio::test]
async fn honors_retry_after_as_delta_seconds_or_http_date_within_the_cap() {
    // Arrange
    let cases = [
        ("30", Duration::from_secs(30)),
        ("Wed, 21 Oct 2026 07:28:45 GMT", Duration::from_secs(45)),
        ("600", Duration::from_mins(1)),
        ("Wed, 21 Oct 2026 09:00:00 GMT", Duration::from_mins(1)),
        ("Wed, 21 Oct 2015 07:28:00 GMT", Duration::from_secs(1)),
    ];

    for (retry_after, expected) in cases {
        let server = MockServer::start().await;
        respond(
            &server,
            ResponseTemplate::new(429).insert_header("Retry-After", retry_after),
            1,
        )
        .await;
        succeed(&server).await;
        let observer = Arc::new(RecordingObserver::default());

        // Act
        instant_transport()
            .post_json::<Value>(&request(&server, RetryPolicy::default(), &observer))
            .await
            .expect("the retry should succeed");

        // Assert
        let delays: Vec<_> = observer.retries().iter().map(|retry| retry.delay).collect();
        assert_eq!(delays, vec![expected], "{retry_after}");
    }
}

#[tokio::test]
async fn reports_the_last_status_once_attempts_are_exhausted() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        ResponseTemplate::new(503).insert_header("Retry-After", "2"),
        u64::MAX,
    )
    .await;
    let observer = Arc::new(RecordingObserver::default());
    let policy = retry_everything().with_max_attempts(3);

    // Act
    let error = instant_transport()
        .post_json::<Value>(&request(&server, policy, &observer))
        .await
        .expect_err("exhausted retries should fail");

    // Assert
    assert!(
        matches!(&error, TransportError::Http { status, .. } if status.as_u16() == 503),
        "{error:?}"
    );
    let requests = server.received_requests().await.expect("recording on");
    assert_eq!(requests.len(), 3);
    let retries: Vec<_> = observer
        .retries()
        .iter()
        .map(|retry| (retry.attempt, retry.delay))
        .collect();
    assert_eq!(
        retries,
        vec![(1, Duration::from_secs(2)), (2, Duration::from_secs(2))]
    );
    let model_error = error.into_model_error("Provider", &[]);
    assert_eq!(model_error.retry_after(), Some(Duration::from_secs(2)));
}

#[tokio::test]
async fn reports_a_connection_retry_without_a_status() {
    // Arrange
    let refused = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("listener should bind")
        .local_addr()
        .expect("listener should have an address");
    let observer = Arc::new(RecordingObserver::default());
    let request = ProviderRequest::new(HeaderMap::new(), format!("http://{refused}"), json!({}))
        .with_retry(RetryPolicy::default(), Some(observer.clone()));

    // Act
    let error = instant_transport()
        .post_json::<Value>(&request)
        .await
        .expect_err("a refused connection should fail");

    // Assert
    assert!(matches!(error, TransportError::Transport(_)), "{error:?}");
    assert_eq!(
        observer.retries(),
        vec![Retry {
            attempt: 1,
            delay: Duration::from_secs(1),
            kind: RetryKind::Connection,
            status: None,
        }]
    );
}

#[tokio::test]
async fn cancelling_during_a_backoff_stops_promptly() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, ResponseTemplate::new(429), u64::MAX).await;
    let observer = Arc::new(RecordingObserver::default());
    let policy = RetryPolicy::default()
        .with_base_delay(Duration::from_hours(1))
        .with_max_backoff(Duration::from_hours(1));
    let request = request(&server, policy, &observer);
    let transport = HttpTransport::new(REQUEST_TIMEOUT);
    let started = std::time::Instant::now();

    // Act
    let finished = tokio::select! {
        result = transport.send(&request) => Some(result.map(drop)),
        () = observer.reported.notified() => None,
    };

    // Assert
    assert!(
        finished.is_none(),
        "the backoff must not finish: {finished:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(observer.retries().len(), 1);
    let requests = server.received_requests().await.expect("recording on");
    assert_eq!(requests.len(), 1, "a cancelled retry must not be sent");
}
