//! Requests structured output from a configured Kimi model and prints the
//! validated JSON, with optional request-duration metrics over OTLP/HTTP.

use std::io::{self, Write};

use ag_harness::{KimiConfig, ModelClient, ModelRequest, OutputSchema};

#[path = "support/telemetry.rs"]
mod telemetry;

use telemetry::DynError;

const GREETING_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "message": {
            "type": "string",
            "enum": ["hello"]
        }
    },
    "required": ["message"],
    "additionalProperties": false
}"#;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    telemetry::run_with_metrics("ag-harness-kimi", run()).await
}

async fn run() -> Result<(), DynError> {
    let config = KimiConfig {
        api_key: std::env::var("KIMI_API_KEY")?,
        base_url: std::env::var("KIMI_BASE_URL")?,
        model: std::env::var("KIMI_MODEL")?,
    };

    request_greeting(config).await
}

async fn request_greeting(config: KimiConfig) -> Result<(), DynError> {
    let model = ModelClient::kimi(config)?;
    let schema = OutputSchema::new(serde_json::from_str(GREETING_SCHEMA)?)?;
    let response = model
        .complete(ModelRequest::new(
            "Return a JSON greeting with the message set to hello.",
            schema,
        ))
        .await?;

    let output = response
        .output()
        .ok_or_else(|| io::Error::other("Kimi returned an unexpected tool call"))?;
    writeln!(io::stdout().lock(), "{output}")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::{ExitStatus, Stdio};
    use std::time::Duration;

    use serde_json::json;
    use tokio::process::{Child, Command};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

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
    async fn request_greeting_rejects_invalid_model() {
        // Arrange
        let config = KimiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://example.com".to_string(),
            model: "  ".to_string(),
        };

        // Act
        let error = request_greeting(config)
            .await
            .expect_err("invalid model configuration should fail");

        // Assert
        assert_eq!(error.to_string(), "model identifier must not be empty");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_exports_metrics_over_otlp_http_on_shutdown() {
        // Arrange
        let kimi_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"message":"hello"}"#}
                }]
            })))
            .expect(1)
            .mount(&kimi_server)
            .await;
        let otlp_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/otlp/v1/metrics"))
            .and(header("authorization", "Basic test-credentials"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&otlp_server)
            .await;
        let executable = std::env::current_exe().expect("test executable should be available");
        let kimi_endpoint = kimi_server.uri();
        let otlp_endpoint = format!("{}/otlp", otlp_server.uri());

        // Act
        let mut child = Command::new(executable)
            .arg("tests::process_fixture_runs_main")
            .arg("--exact")
            .arg("--nocapture")
            .env("AG_HARNESS_RUN_MAIN_FIXTURE", "1")
            .env("KIMI_API_KEY", "test-key")
            .env("KIMI_BASE_URL", kimi_endpoint)
            .env("KIMI_MODEL", "kimi-k2.6")
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", otlp_endpoint)
            .env(
                "OTEL_EXPORTER_OTLP_HEADERS",
                "Authorization=Basic%20test-credentials",
            )
            .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
            .env_remove("OTEL_EXPORTER_OTLP_METRICS_HEADERS")
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
        let kimi_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .mount(&kimi_server)
            .await;
        let executable = std::env::current_exe().expect("test executable should be available");
        let mut child = Command::new(executable)
            .arg("tests::process_fixture_runs_main")
            .arg("--exact")
            .arg("--nocapture")
            .env("AG_HARNESS_RUN_MAIN_FIXTURE", "1")
            .env("KIMI_API_KEY", "test-key")
            .env("KIMI_BASE_URL", kimi_server.uri())
            .env("KIMI_MODEL", "kimi-k2.6")
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
}
