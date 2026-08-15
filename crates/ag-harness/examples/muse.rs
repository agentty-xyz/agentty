//! Requests structured output from Muse Spark and prints the validated JSON,
//! with optional request-duration metrics over OTLP/HTTP.

use ag_harness::{MUSE_SPARK_1_2, ModelClient, MuseConfig};

#[path = "support/greeting.rs"]
mod greeting;
#[cfg(test)]
#[path = "support/process_fixture.rs"]
mod process_fixture;
#[path = "support/telemetry.rs"]
mod telemetry;

use telemetry::DynError;

const MODEL_API_BASE_URL: &str = "https://api.meta.ai/v1";

#[tokio::main]
async fn main() -> Result<(), DynError> {
    telemetry::run_with_metrics("ag-harness-muse", run()).await
}

async fn run() -> Result<(), DynError> {
    let config = config(
        std::env::var("MODEL_API_KEY")?,
        std::env::var("MODEL_API_BASE_URL").ok(),
        std::env::var("MODEL_API_MODEL").ok(),
    );

    request_greeting(config).await
}

fn config(api_key: String, base_url: Option<String>, model: Option<String>) -> MuseConfig {
    MuseConfig {
        api_key,
        base_url: base_url.unwrap_or_else(|| MODEL_API_BASE_URL.to_string()),
        model: model.unwrap_or_else(|| MUSE_SPARK_1_2.to_string()),
    }
}

async fn request_greeting(config: MuseConfig) -> Result<(), DynError> {
    let client = ModelClient::muse(config)?;

    greeting::request(client, "Muse").await
}

#[cfg(test)]
mod tests {
    use ag_harness::MUSE_SPARK_1_2_CONTRIBUTOR;

    use super::*;

    #[test]
    fn configuration_uses_official_defaults() {
        // Arrange and Act
        let config = config("test-key".to_string(), None, None);

        // Assert
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.base_url, "https://api.meta.ai/v1");
        assert_eq!(config.model, "muse-spark-1.2");
    }

    #[test]
    fn configuration_accepts_contributor_model() {
        // Arrange and Act
        let config = config(
            "test-key".to_string(),
            None,
            Some(MUSE_SPARK_1_2_CONTRIBUTOR.to_string()),
        );

        // Assert
        assert_eq!(config.model, "muse-spark-1.2-contributor");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_greeting_rejects_invalid_model() {
        // Arrange
        let config = MuseConfig {
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
            "MODEL_API_KEY",
            "MODEL_API_BASE_URL",
            Some(("MODEL_API_MODEL", None)),
        );
        let responses = vec![process_fixture::terminal_response(r#"{"message":"hello"}"#)];

        // Act and Assert
        process_fixture::assert_exports_metrics(environment, responses).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_timeout_kills_and_reaps_fixture() {
        // Arrange
        let environment = process_fixture::ProviderEnvironment::new(
            "MODEL_API_KEY",
            "MODEL_API_BASE_URL",
            Some(("MODEL_API_MODEL", None)),
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
