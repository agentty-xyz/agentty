//! Lets Muse inspect this workspace with the native `read` tool and prints the
//! validated package summary, with optional request-duration metrics over
//! OTLP/HTTP.

use std::io::{self, Write as _};

use ag_harness::{Harness, MUSE_SPARK_1_2, Model, Muse, OutputSchema, Tool};
use serde_json::{Value, json};

#[cfg(test)]
#[path = "support/process_fixture.rs"]
mod process_fixture;
#[path = "support/telemetry.rs"]
mod telemetry;

use telemetry::DynError;

const PROMPT: &str = concat!(
    "Use the read tool to inspect Cargo.toml. ",
    "Return the exact package name from its [package] table."
);

#[tokio::main]
async fn main() -> Result<(), DynError> {
    telemetry::run_with_metrics("ag-harness-muse-read", run()).await
}

async fn run() -> Result<(), DynError> {
    let model = Muse::from_env(MUSE_SPARK_1_2)?;
    let output = inspect_manifest(model, package_schema()).await?;
    let output = serde_json::to_string_pretty(&output)?;

    writeln!(io::stdout().lock(), "{output}")?;

    Ok(())
}

fn package_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "package": { "type": "string" }
        },
        "required": ["package"],
        "additionalProperties": false
    })
}

async fn inspect_manifest(model: impl Model + 'static, schema: Value) -> Result<Value, DynError> {
    let harness = Harness::new(model)
        .repository(env!("CARGO_MANIFEST_DIR"))
        .allow(Tool::Read);
    let schema = OutputSchema::new(schema)?;

    Ok(harness.run(PROMPT, schema).await?)
}

#[cfg(test)]
mod tests {
    use ag_harness::{ModelClient, MuseConfig};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn inspection_rejects_invalid_schema() {
        // Arrange
        let model = ModelClient::muse(MuseConfig {
            api_key: "test-key".to_string(),
            base_url: "https://example.com".to_string(),
            model: MUSE_SPARK_1_2.to_string(),
        })
        .expect("fixture model should be valid");

        // Act
        let error = inspect_manifest(model, json!({ "type": "not-a-json-type" }))
            .await
            .expect_err("invalid output schema should fail");

        // Assert
        assert!(error.to_string().starts_with("invalid output schema:"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_completes_read_turn_and_exports_metrics() {
        // Arrange
        let environment =
            process_fixture::ProviderEnvironment::new("MODEL_API_KEY", "MODEL_API_BASE_URL", None);
        let responses = vec![
            json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_muse_read",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"Cargo.toml"}"#
                            }
                        }]
                    }
                }]
            }),
            process_fixture::terminal_response(r#"{"package":"ag-harness"}"#),
        ];

        // Act and Assert
        process_fixture::assert_exports_metrics(environment, responses).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_timeout_kills_and_reaps_fixture() {
        // Arrange
        let environment =
            process_fixture::ProviderEnvironment::new("MODEL_API_KEY", "MODEL_API_BASE_URL", None);

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
