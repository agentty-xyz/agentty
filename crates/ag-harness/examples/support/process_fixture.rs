use std::ops::RangeInclusive;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::process::{Child, Command};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::telemetry::DynError;

const FIXTURE_TEST: &str = "tests::process_fixture_runs_main";

/// Provider-specific environment variables supplied to an example fixture.
pub(crate) struct ProviderEnvironment {
    api_key_name: &'static str,
    base_url_name: &'static str,
    model: Option<ModelEnvironment>,
}

impl ProviderEnvironment {
    /// Describes a provider and its optional model environment override.
    pub(crate) const fn new(
        api_key_name: &'static str,
        base_url_name: &'static str,
        model: Option<(&'static str, Option<&'static str>)>,
    ) -> Self {
        Self {
            api_key_name,
            base_url_name,
            model: match model {
                Some((variable, value)) => Some(ModelEnvironment { value, variable }),
                None => None,
            },
        }
    }

    fn apply(&self, command: &mut Command, base_url: &str) {
        command
            .env(self.api_key_name, "test-key")
            .env(self.base_url_name, base_url);

        if let Some(model) = self.model {
            match model.value {
                Some(value) => {
                    command.env(model.variable, value);
                }
                None => {
                    command.env_remove(model.variable);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ModelEnvironment {
    value: Option<&'static str>,
    variable: &'static str,
}

#[derive(Clone)]
struct SequenceResponder {
    next_response: Arc<AtomicUsize>,
    responses: Arc<Vec<ResponseTemplate>>,
}

impl Respond for SequenceResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let response_index = self.next_response.fetch_add(1, Ordering::SeqCst);

        self.responses[response_index].clone()
    }
}

/// Builds a successful terminal Chat Completions response for an example
/// fixture.
pub(crate) fn terminal_response(content: &str) -> Value {
    serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": content}
        }]
    })
}

/// Verifies that the example completes and flushes one OTLP metrics request.
pub(crate) async fn assert_exports_metrics(
    environment: ProviderEnvironment,
    model_responses: Vec<Value>,
) {
    let request_count =
        u64::try_from(model_responses.len()).expect("fixture response count should fit in u64");
    let responses = model_responses
        .into_iter()
        .map(|response| ResponseTemplate::new(200).set_body_json(response))
        .collect();
    let model_server = model_server(responses, request_count..=request_count).await;
    let otlp_server = metrics_server().await;
    let otlp_endpoint = format!("{}/otlp", otlp_server.uri());
    let mut child = spawn_fixture(environment, &model_server.uri(), Some(&otlp_endpoint));
    let status = wait_for_fixture(&mut child, Duration::from_secs(10))
        .await
        .expect("fixture process should finish before the timeout");

    assert!(status.success(), "fixture process should succeed");
}

/// Verifies that a stalled example process is killed and reaped after timeout.
pub(crate) async fn assert_timeout_kills_and_reaps(environment: ProviderEnvironment) {
    let model_server = model_server(
        vec![ResponseTemplate::new(200).set_delay(Duration::from_secs(10))],
        0..=1,
    )
    .await;
    let mut child = spawn_fixture(environment, &model_server.uri(), None);
    let result = wait_for_fixture(&mut child, Duration::from_millis(100)).await;

    assert_eq!(
        result
            .expect_err("fixture should exceed the timeout")
            .to_string(),
        "deadline has elapsed"
    );
    assert!(
        child
            .try_wait()
            .expect("reaped fixture status should be available")
            .is_some(),
        "timed-out fixture should be reaped"
    );
}

/// Runs the example entry point only inside its explicitly configured child
/// fixture.
pub(crate) fn run_main_if_requested(run: impl FnOnce() -> Result<(), DynError>) {
    if std::env::var_os("AG_HARNESS_RUN_MAIN_FIXTURE").is_none() {
        return;
    }

    run().expect("configured example should complete and flush metrics");
}

async fn model_server(
    responses: Vec<ResponseTemplate>,
    expected_requests: RangeInclusive<u64>,
) -> MockServer {
    let server = MockServer::start().await;
    let responder = SequenceResponder {
        next_response: Arc::new(AtomicUsize::new(0)),
        responses: Arc::new(responses),
    };
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(responder)
        .expect(expected_requests)
        .mount(&server)
        .await;

    server
}

async fn metrics_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/otlp/v1/metrics"))
        .and(header("authorization", "Basic test-credentials"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    server
}

fn spawn_fixture(
    environment: ProviderEnvironment,
    model_endpoint: &str,
    otlp_endpoint: Option<&str>,
) -> Child {
    let executable = std::env::current_exe().expect("test executable should be available");
    let mut command = Command::new(executable);
    command
        .arg(FIXTURE_TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env("AG_HARNESS_RUN_MAIN_FIXTURE", "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT")
        .env_remove("OTEL_EXPORTER_OTLP_TIMEOUT")
        .env_remove("OTEL_METRIC_EXPORT_INTERVAL")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    environment.apply(&mut command, model_endpoint);

    if let Some(endpoint) = otlp_endpoint {
        command.env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint).env(
            "OTEL_EXPORTER_OTLP_HEADERS",
            "Authorization=Basic%20test-credentials",
        );
    }

    command.spawn().expect("fixture process should start")
}

async fn wait_for_fixture(
    child: &mut Child,
    timeout_duration: Duration,
) -> Result<ExitStatus, DynError> {
    match tokio::time::timeout(timeout_duration, child.wait()).await {
        Ok(status) => Ok(status?),
        Err(error) => {
            child.kill().await?;
            child.wait().await?;

            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn optional_model_server_accepts_no_request() {
        // Arrange
        let server = model_server(vec![ResponseTemplate::new(200)], 0..=1).await;

        // Act and Assert
        server.verify().await;
    }
}
