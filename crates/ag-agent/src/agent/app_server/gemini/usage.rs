//! Gemini ACP prompt-completion parsing helpers.

use serde_json::Value;

use crate::app_server::AppServerError;

/// Normalized data extracted from one ACP `session/prompt` completion
/// response.
#[derive(Debug)]
pub(super) struct PromptCompletion {
    /// Final assistant message when the completion returned one.
    pub(super) assistant_message: Option<String>,
    /// Reported prompt input token count.
    pub(super) input_tokens: u64,
    /// Reported prompt output token count.
    pub(super) output_tokens: u64,
}

/// Parses one completed `session/prompt` response into normalized turn fields.
pub(super) fn parse_prompt_completion_response(
    response_value: &Value,
) -> Result<PromptCompletion, AppServerError> {
    let result = response_value.get("result").ok_or_else(|| {
        AppServerError::Provider(
            "Gemini ACP `session/prompt` response missing `result`".to_string(),
        )
    })?;
    let (input_tokens, output_tokens) = extract_prompt_usage_tokens(result);
    let assistant_message = extract_prompt_result_text(result);

    Ok(PromptCompletion {
        assistant_message,
        input_tokens,
        output_tokens,
    })
}

/// Extracts prompt completion usage values from ACP result payloads.
pub(super) fn extract_prompt_usage_tokens(result: &Value) -> (u64, u64) {
    extract_token_count_object(result.get("usage"))
        .or_else(|| extract_meta_quota_token_count(result))
        .or_else(|| extract_meta_model_usage_totals(result))
        .unwrap_or((0, 0))
}

/// Extracts prompt usage totals from the current Gemini ACP `_meta.quota`
/// result payload shape.
fn extract_meta_quota_token_count(result: &Value) -> Option<(u64, u64)> {
    let quota = result.get("_meta")?.get("quota")?;
    extract_token_count_object(quota.get("token_count").or_else(|| quota.get("tokenCount")))
}

/// Extracts prompt usage totals by summing `_meta.quota.model_usage`
/// entries when the aggregate token count is absent.
fn extract_meta_model_usage_totals(result: &Value) -> Option<(u64, u64)> {
    let quota = result.get("_meta")?.get("quota")?;
    let model_usage = quota
        .get("model_usage")
        .or_else(|| quota.get("modelUsage"))?
        .as_array()?;
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut found_usage = false;

    for model_usage_entry in model_usage {
        if let Some((model_input_tokens, model_output_tokens)) = extract_token_count_object(
            model_usage_entry
                .get("token_count")
                .or_else(|| model_usage_entry.get("tokenCount")),
        ) {
            input_tokens += model_input_tokens;
            output_tokens += model_output_tokens;
            found_usage = true;
        }
    }

    if !found_usage {
        return None;
    }

    Some((input_tokens, output_tokens))
}

/// Extracts normalized prompt token counts from one usage/token-count object.
fn extract_token_count_object(value: Option<&Value>) -> Option<(u64, u64)> {
    let usage = value?;
    let input_tokens = usage
        .get("inputTokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

/// Extracts assistant text from known ACP prompt completion result shapes.
pub(super) fn extract_prompt_result_text(result: &Value) -> Option<String> {
    extract_string_field(result, "response")
        .or_else(|| extract_string_field(result, "text"))
        .or_else(|| extract_nonempty_content_field(result, "content"))
        .or_else(|| result.get("message").and_then(extract_message_text))
        .or_else(|| extract_output_text(result.get("output")?))
}

fn extract_string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

fn extract_nonempty_content_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(extract_text_from_content_value)
        .filter(|text| !text.is_empty())
}

fn extract_message_text(message: &Value) -> Option<String> {
    extract_string_field(message, "text")
        .or_else(|| extract_nonempty_content_field(message, "content"))
}

fn extract_output_text(output: &Value) -> Option<String> {
    let output_items = output.as_array()?;
    let mut output_text = String::new();
    for output_item in output_items {
        if let Some(item_text) = extract_string_field(output_item, "text") {
            output_text.push_str(&item_text);

            continue;
        }

        if let Some(content) = output_item.get("content")
            && let Some(content_text) = extract_text_from_content_value(content)
        {
            output_text.push_str(&content_text);
        }
    }
    if output_text.is_empty() {
        return None;
    }

    Some(output_text)
}

/// Extracts text from ACP content values represented as strings, arrays, or
/// nested objects.
pub(super) fn extract_text_from_content_value(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => extract_text_from_parts(parts),
        Value::Object(_) => extract_text_from_content_object(content),
        _ => None,
    }
}

fn extract_text_from_parts(parts: &[Value]) -> Option<String> {
    let combined_text = parts
        .iter()
        .filter_map(extract_text_from_content_value)
        .collect::<String>();

    (!combined_text.is_empty()).then_some(combined_text)
}

fn extract_text_from_content_object(content: &Value) -> Option<String> {
    extract_string_field(content, "text")
        .or_else(|| extract_nonempty_content_field(content, "parts"))
        .or_else(|| extract_nonempty_content_field(content, "content"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::extract_prompt_result_text;

    #[test]
    fn prompt_result_text_flattens_nested_content_parts() {
        // Arrange
        let result = json!({
            "content": {
                "parts": [null, "First", {"content": {"text": " second"}}]
            }
        });

        // Act
        let text = extract_prompt_result_text(&result);

        // Assert
        assert_eq!(text.as_deref(), Some("First second"));
    }

    #[test]
    fn prompt_result_text_reads_message_content() {
        // Arrange
        let result = json!({"message": {"content": ["Message text"]}});

        // Act
        let text = extract_prompt_result_text(&result);

        // Assert
        assert_eq!(text.as_deref(), Some("Message text"));
    }

    #[test]
    fn prompt_result_text_combines_output_items() {
        // Arrange
        let result = json!({
            "output": [
                {"text": "First"},
                {"content": {"text": " second"}}
            ]
        });

        // Act
        let text = extract_prompt_result_text(&result);

        // Assert
        assert_eq!(text.as_deref(), Some("First second"));
    }
}
