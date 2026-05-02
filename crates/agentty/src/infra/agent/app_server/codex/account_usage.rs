//! Codex account and rate-limit usage collection.

use std::path::Path;

use serde_json::Value;

use super::lifecycle;
use super::transport::{CodexRuntimeTransport, CodexStdioTransport};
use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::domain::agent_usage::{AgentRateLimit, AgentUsageDetails};
use crate::infra::app_server::AppServerError;
use crate::infra::{agent, app_server_transport};

/// Loads ChatGPT account plan and current Codex rate-limit buckets from one
/// temporary `codex app-server` runtime.
pub(crate) async fn load_codex_account_usage(
    folder: &Path,
    model: &str,
) -> Result<AgentUsageDetails, AppServerError> {
    let request_kind = crate::infra::channel::AgentRequestKind::AccountRead;
    let command = agent::create_backend(AgentKind::Codex)
        .build_command(agent::BuildCommandRequest {
            attachments: &[],
            folder,
            prompt: "",
            request_kind: &request_kind,
            model,
            reasoning_level: ReasoningLevel::default(),
        })
        .map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to build `codex app-server` account usage command: {error}"
            ))
        })?;
    let (mut child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "codex app-server")?;
    let mut transport = CodexStdioTransport::new(stdin, stdout);

    let usage_result = load_codex_account_usage_with_transport(&mut transport).await;
    transport.close_stdin();
    app_server_transport::shutdown_child(&mut child).await;

    usage_result
}

/// Loads Codex account usage through an initialized app-server transport.
pub(super) async fn load_codex_account_usage_with_transport<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
) -> Result<AgentUsageDetails, AppServerError> {
    lifecycle::initialize_runtime(transport).await?;
    let account_response =
        send_account_request(transport, "account/read", "codex-account-read").await?;
    let rate_limits_response = send_account_request(
        transport,
        "account/rateLimits/read",
        "codex-rate-limits-read",
    )
    .await?;

    Ok(AgentUsageDetails {
        plan: extract_plan_type(&account_response),
        rate_limits: extract_rate_limits(&rate_limits_response),
        reached_type: extract_reached_type(&rate_limits_response),
    })
}

/// Sends one account-scoped JSON-RPC request and returns its parsed response.
async fn send_account_request<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    method: &str,
    id_prefix: &str,
) -> Result<Value, AppServerError> {
    let request_id = format!("{id_prefix}-{}", uuid::Uuid::new_v4());
    let payload = serde_json::json!({
        "method": method,
        "id": request_id,
        "params": {}
    });
    transport.write_json_line(payload).await?;
    let response_line = transport.wait_for_response_line(request_id).await?;
    let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse Codex account usage response: {error}"
        ))
    })?;

    if let Some(error_message) = app_server_transport::extract_json_error_message(&response_value) {
        return Err(AppServerError::Provider(format!(
            "Codex account usage request `{method}` failed: {error_message}"
        )));
    }

    Ok(response_value)
}

/// Extracts the ChatGPT plan type from an `account/read` response.
fn extract_plan_type(response_value: &Value) -> Option<String> {
    response_value
        .get("result")
        .and_then(|result| result.get("account"))
        .and_then(|account| account.get("planType").or_else(|| account.get("plan_type")))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Extracts Codex rate-limit buckets from an `account/rateLimits/read`
/// response.
fn extract_rate_limits(response_value: &Value) -> Vec<AgentRateLimit> {
    let Some(rate_limits) = response_value.get("result").and_then(|result| {
        result
            .get("rateLimits")
            .or_else(|| result.get("rate_limits"))
    }) else {
        return Vec::new();
    };

    ["primary", "secondary"]
        .into_iter()
        .filter_map(|bucket_name| {
            rate_limits
                .get(bucket_name)
                .filter(|bucket| !bucket.is_null())
                .map(|bucket| extract_rate_limit(bucket_name, bucket))
        })
        .collect()
}

/// Extracts one named Codex rate-limit bucket.
fn extract_rate_limit(bucket_name: &str, bucket: &Value) -> AgentRateLimit {
    AgentRateLimit {
        label: title_case_ascii(bucket_name),
        used_percent: bucket
            .get("usedPercent")
            .or_else(|| bucket.get("used_percent"))
            .and_then(format_percent_value),
        resets_at_unix_seconds: bucket
            .get("resetsAt")
            .or_else(|| bucket.get("resets_at"))
            .and_then(Value::as_i64),
        window_duration_mins: bucket
            .get("windowDurationMins")
            .or_else(|| bucket.get("window_duration_mins"))
            .and_then(Value::as_u64),
    }
}

/// Extracts the backend-classified reached-limit type, when present.
fn extract_reached_type(response_value: &Value) -> Option<String> {
    response_value
        .get("result")
        .and_then(|result| {
            result
                .get("rateLimits")
                .or_else(|| result.get("rate_limits"))
        })
        .and_then(|rate_limits| {
            rate_limits
                .get("rateLimitReachedType")
                .or_else(|| rate_limits.get("rate_limit_reached_type"))
        })
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Formats one JSON numeric percentage without adding noisy trailing zeros.
fn format_percent_value(value: &Value) -> Option<String> {
    if let Some(percent) = value.as_u64() {
        return Some(format!("{percent}%"));
    }

    let percent = value.as_f64()?;
    if percent.fract() == 0.0 {
        return Some(format!("{percent:.0}%"));
    }

    Some(format!("{percent:.1}%"))
}

/// Converts a known ASCII bucket identifier into a title label.
fn title_case_ascii(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };

    format!(
        "{}{}",
        first.to_ascii_uppercase(),
        characters.as_str().to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use mockall::Sequence;

    use super::*;
    use crate::infra::agent::app_server::codex::MockCodexRuntimeTransport;

    /// Stores the request id from one JSON-RPC write for a later response.
    fn remember_request_id(id_store: &Arc<Mutex<Option<String>>>, payload: &Value) {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if let Ok(mut guard) = id_store.lock() {
            *guard = id;
        }
    }

    #[tokio::test]
    async fn load_codex_account_usage_with_transport_reads_plan_and_rate_limits() {
        // Arrange
        let initialize_id = Arc::new(Mutex::new(None));
        let account_id = Arc::new(Mutex::new(None));
        let rate_limits_id = Arc::new(Mutex::new(None));
        let mut sequence = Sequence::new();
        let mut transport = MockCodexRuntimeTransport::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf({
                let initialize_id = Arc::clone(&initialize_id);

                move |payload| {
                    remember_request_id(&initialize_id, payload);

                    payload.get("method").and_then(Value::as_str) == Some("initialize")
                }
            })
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let initialize_id = Arc::clone(&initialize_id);

                move |response_id| {
                    let expected_id = initialize_id
                        .lock()
                        .expect("initialize id mutex should not be poisoned")
                        .clone()
                        .expect("initialize id should be captured");
                    assert_eq!(response_id, expected_id);

                    Box::pin(
                        async move { Ok(format!(r#"{{"id":"{response_id}","result":{{}}}}"#)) },
                    )
                }
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialized"))
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf({
                let account_id = Arc::clone(&account_id);

                move |payload| {
                    remember_request_id(&account_id, payload);

                    payload.get("method").and_then(Value::as_str) == Some("account/read")
                }
            })
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let account_id = Arc::clone(&account_id);

                move |response_id| {
                    let expected_id = account_id
                        .lock()
                        .expect("account id mutex should not be poisoned")
                        .clone()
                        .expect("account id should be captured");
                    assert_eq!(response_id, expected_id);

                    Box::pin(async move {
                        Ok(format!(
                            r#"{{"id":"{response_id}","result":{{"account":{{"type":"chatgpt","planType":"pro"}}}}}}"#
                        ))
                    })
                }
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf({
                let rate_limits_id = Arc::clone(&rate_limits_id);

                move |payload| {
                    remember_request_id(&rate_limits_id, payload);

                    payload.get("method").and_then(Value::as_str) == Some("account/rateLimits/read")
                }
            })
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let rate_limits_id = Arc::clone(&rate_limits_id);

                move |response_id| {
                    let expected_id = rate_limits_id
                        .lock()
                        .expect("rate-limit id mutex should not be poisoned")
                        .clone()
                        .expect("rate-limit id should be captured");
                    assert_eq!(response_id, expected_id);

                    Box::pin(async move {
                        Ok(format!(
                            r#"{{"id":"{response_id}","result":{{"rateLimits":{{"primary":{{"usedPercent":25,"windowDurationMins":15,"resetsAt":1730947200}},"secondary":null,"rateLimitReachedType":null}}}}}}"#
                        ))
                    })
                }
            });

        // Act
        let usage = load_codex_account_usage_with_transport(&mut transport)
            .await
            .expect("usage should load");

        // Assert
        assert_eq!(usage.plan, Some("pro".to_string()));
        assert_eq!(
            usage.rate_limits,
            vec![AgentRateLimit {
                label: "Primary".to_string(),
                used_percent: Some("25%".to_string()),
                resets_at_unix_seconds: Some(1_730_947_200),
                window_duration_mins: Some(15),
            }]
        );
        assert_eq!(usage.reached_type, None);
    }

    #[test]
    fn extract_rate_limits_reads_snake_case_response_fields() {
        // Arrange
        let response_value = serde_json::json!({
            "result": {
                "rate_limits": {
                    "primary": {
                        "used_percent": 12.5,
                        "window_duration_mins": 300,
                        "resets_at": 1730947200
                    },
                    "rate_limit_reached_type": "primary"
                }
            }
        });

        // Act
        let rate_limits = extract_rate_limits(&response_value);
        let reached_type = extract_reached_type(&response_value);

        // Assert
        assert_eq!(rate_limits[0].used_percent, Some("12.5%".to_string()));
        assert_eq!(rate_limits[0].window_duration_mins, Some(300));
        assert_eq!(reached_type, Some("primary".to_string()));
    }
}
