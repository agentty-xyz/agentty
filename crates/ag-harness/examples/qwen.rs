//! Requests structured output from a configured Qwen model and prints the
//! validated JSON, with optional request-duration metrics over OTLP/HTTP.

use ag_harness::{ModelClient, QwenConfig};

#[path = "support/greeting.rs"]
mod greeting;
#[cfg(test)]
#[path = "support/process_fixture.rs"]
mod process_fixture;
#[path = "support/telemetry.rs"]
mod telemetry;

use telemetry::DynError;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    telemetry::run_with_metrics("ag-harness-qwen", run()).await
}

async fn run() -> Result<(), DynError> {
    let config = QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: "qwen-plus".to_string(),
    };

    request_greeting(config).await
}

async fn request_greeting(config: QwenConfig) -> Result<(), DynError> {
    let client = ModelClient::qwen(config)?;

    greeting::request(client, "Qwen").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn request_greeting_rejects_invalid_model() {
        // Arrange
        let config = QwenConfig {
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
        let environment = process_fixture::ProviderEnvironment::new(
            "DASHSCOPE_API_KEY",
            "DASHSCOPE_BASE_URL",
            None,
        );

        // Act and Assert
        process_fixture::assert_exports_metrics(environment).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_timeout_kills_and_reaps_fixture() {
        // Arrange
        let environment = process_fixture::ProviderEnvironment::new(
            "DASHSCOPE_API_KEY",
            "DASHSCOPE_BASE_URL",
            None,
        );

        // Act and Assert
        process_fixture::assert_timeout_kills_and_reaps(environment).await;
    }

    #[test]
    fn process_fixture_runs_main() {
        // Arrange
        let run = main;

        // Act and Assert
        process_fixture::run_main_if_requested(run);
    }
}
