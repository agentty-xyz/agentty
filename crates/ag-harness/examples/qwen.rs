//! Requests structured output from a configured Qwen model and prints the
//! validated JSON, with optional request-duration metrics over OTLP/HTTP.

use std::error::Error;
use std::io::{self, Write};

use ag_harness::{Model, ModelRequest, OutputSchema, Qwen, QwenConfig};
use opentelemetry::global;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use serde_json::json;

type DynError = Box<dyn Error + Send + Sync>;

/// Configures OTLP metrics when a collector endpoint is present.
fn init_metrics(endpoint: Option<String>) -> Result<Option<SdkMeterProvider>, DynError> {
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{}/v1/metrics", endpoint.trim_end_matches('/')))
        .build()?;
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_service_name("ag-harness-qwen")
                .build(),
        )
        .build();
    global::set_meter_provider(provider.clone());

    Ok(Some(provider))
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let meter_provider = init_metrics(std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())?;
    let result = run().await;
    let shutdown_result = shutdown_metrics(meter_provider).await;

    finish(result, shutdown_result)
}

/// Flushes configured metrics without blocking a Tokio worker thread.
async fn shutdown_metrics(meter_provider: Option<SdkMeterProvider>) -> Result<(), DynError> {
    let Some(meter_provider) = meter_provider else {
        return Ok(());
    };

    tokio::task::spawn_blocking(move || meter_provider.shutdown()).await??;

    Ok(())
}

/// Preserves an application failure when telemetry shutdown also fails.
fn finish(
    result: Result<(), DynError>,
    shutdown_result: Result<(), DynError>,
) -> Result<(), DynError> {
    match (result, shutdown_result) {
        (Err(error), Err(shutdown_error)) => {
            drop(writeln!(
                io::stderr().lock(),
                "telemetry shutdown also failed: {shutdown_error}"
            ));

            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), shutdown_result) => shutdown_result,
    }
}

async fn run() -> Result<(), DynError> {
    let model = Qwen::new(QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: "qwen-plus".to_string(),
    });
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "const": "hello"
            }
        },
        "required": ["message"],
        "additionalProperties": false
    }))?;
    let response = model
        .complete(ModelRequest::new(
            "Return a JSON greeting with the message set to hello.",
            schema,
        ))
        .await?;

    writeln!(io::stdout().lock(), "{}", response.output())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::{ExitStatus, Stdio};
    use std::time::Duration;

    use tokio::process::{Child, Command};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn test_error(message: &'static str) -> DynError {
        io::Error::other(message).into()
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

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_are_disabled_without_an_endpoint() {
        // Arrange
        let meter_provider = init_metrics(None).expect("disabled metrics setup should succeed");

        // Act
        let result = shutdown_metrics(meter_provider).await;

        // Assert
        result.expect("disabled metrics shutdown should succeed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_exports_metrics_over_otlp_http_on_shutdown() {
        // Arrange
        let qwen_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"message":"hello"}"#}
                }]
            })))
            .expect(1)
            .mount(&qwen_server)
            .await;
        let otlp_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/metrics"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&otlp_server)
            .await;
        let executable = std::env::current_exe().expect("test executable should be available");
        let qwen_endpoint = qwen_server.uri();
        let otlp_endpoint = otlp_server.uri();

        // Act
        let mut child = Command::new(executable)
            .arg("tests::process_fixture_runs_main")
            .arg("--exact")
            .arg("--nocapture")
            .env("AG_HARNESS_RUN_MAIN_FIXTURE", "1")
            .env("DASHSCOPE_API_KEY", "test-key")
            .env("DASHSCOPE_BASE_URL", qwen_endpoint)
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", otlp_endpoint)
            .env_remove("OTEL_EXPORTER_OTLP_COMPRESSION")
            .env_remove("OTEL_EXPORTER_OTLP_METRICS_COMPRESSION")
            .env_remove("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT")
            .env_remove("OTEL_EXPORTER_OTLP_TIMEOUT")
            .env_remove("OTEL_METRIC_EXPORT_INTERVAL")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("fixture process should start");
        let status = wait_for_fixture(&mut child, Duration::from_secs(10))
            .await
            .expect("fixture process should finish before the timeout");

        // Assert
        assert!(status.success(), "fixture process should succeed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_timeout_kills_and_reaps_fixture() {
        // Arrange
        let qwen_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .mount(&qwen_server)
            .await;
        let executable = std::env::current_exe().expect("test executable should be available");
        let mut child = Command::new(executable)
            .arg("tests::process_fixture_runs_main")
            .arg("--exact")
            .arg("--nocapture")
            .env("AG_HARNESS_RUN_MAIN_FIXTURE", "1")
            .env("DASHSCOPE_API_KEY", "test-key")
            .env("DASHSCOPE_BASE_URL", qwen_server.uri())
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("fixture process should start");

        // Act
        let result = wait_for_fixture(&mut child, Duration::from_millis(100)).await;

        // Assert
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

    #[test]
    fn process_fixture_runs_main() {
        // Arrange
        let should_run = std::env::var_os("AG_HARNESS_RUN_MAIN_FIXTURE").is_some();
        if !should_run {
            return;
        }

        // Act
        let result = main();

        // Assert
        result.expect("configured example should complete and flush metrics");
    }

    #[test]
    fn finish_preserves_application_error_priority() {
        // Arrange
        let application_error = test_error("Qwen request failed");
        let shutdown_error = test_error("metrics flush failed");

        // Act
        let combined_result = finish(Err(application_error), Err(shutdown_error));
        let application_only_result = finish(Err(test_error("application only")), Ok(()));
        let shutdown_only_result = finish(Ok(()), Err(test_error("shutdown only")));
        let success_result = finish(Ok(()), Ok(()));

        // Assert
        assert_eq!(
            combined_result
                .expect_err("both failures should fail")
                .to_string(),
            "Qwen request failed"
        );
        assert_eq!(
            application_only_result
                .expect_err("the application failure should be returned")
                .to_string(),
            "application only"
        );
        assert_eq!(
            shutdown_only_result
                .expect_err("the shutdown failure should be returned")
                .to_string(),
            "shutdown only"
        );
        success_result.expect("successful run and shutdown should succeed");
    }
}
