use crate::chat_completion::{
    ChatCompletionBackend, ChatCompletionProviderPolicy, ReasoningFormat, StructuredOutputMode,
    default_client,
};
use crate::model;

pub(super) fn max_as_xhigh(reasoning_effort: model::ReasoningEffort) -> &'static str {
    match reasoning_effort {
        model::ReasoningEffort::Max => model::ReasoningEffort::XHigh.as_str(),
        reasoning_effort => reasoning_effort.as_str(),
    }
}

pub(super) fn native_schema_backend() -> ChatCompletionBackend {
    ChatCompletionBackend::with_client(
        "test-key".to_string(),
        "https://example.com/v1".to_string(),
        "native-schema-model".to_string(),
        ChatCompletionProviderPolicy {
            display_name: "Native schema provider",
            reasoning_format: ReasoningFormat::Effort(max_as_xhigh),
            response_format_with_tools: true,
            structured_output: StructuredOutputMode::JsonSchema,
            telemetry_name: "native_schema",
            unsupported_schema_reason: "object schema required",
        },
        default_client(),
    )
}
