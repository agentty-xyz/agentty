//! Telemetry: error-type labels shared by lifecycle events and errors and,
//! with the `otel` feature, OpenTelemetry `GenAI` metrics and trace-context
//! propagation.

mod label;
#[cfg(feature = "otel")]
mod metric;
mod propagation;
#[cfg(all(
    feature = "otel",
    any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    )
))]
mod request;
#[cfg(all(
    not(feature = "otel"),
    any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    )
))]
#[path = "telemetry/request_disabled.rs"]
mod request;

#[cfg(feature = "providers-anthropic")]
pub(crate) use label::PROVIDER_ANTHROPIC;
pub(crate) use label::{
    ERROR_BUDGET_LEDGER_COST, ERROR_BUDGET_LEDGER_MODEL_REQUESTS, ERROR_BUDGET_LEDGER_TOKENS,
    ERROR_BUDGET_LEDGER_TOOL_CALLS, ERROR_BUDGET_LEDGER_WALL_TIME, ERROR_CANCELLED,
    ERROR_COMPARISON_REPOSITORY_MISMATCH, ERROR_CONTEXT_BUDGET, ERROR_CONTEXT_OVERFLOW,
    ERROR_CONTEXT_SUMMARY_BUDGET_EXHAUSTED, ERROR_CONTEXT_SUMMARY_INVALID_OUTPUT,
    ERROR_CONTEXT_SUMMARY_MODEL, ERROR_CONTEXT_SUMMARY_REJECTED, ERROR_INVALID_OUTPUT,
    ERROR_INVALID_PROVIDER_RESPONSE, ERROR_INVALID_RESPONSE, ERROR_INVALID_TOOL_CALL,
    ERROR_PROVIDER, ERROR_REPEATED_TOOL_CALLS, ERROR_REPOSITORY_REQUIRED, ERROR_REQUEST,
    ERROR_RESPONSE_TOO_LARGE, ERROR_RUN_LIMIT_MODEL_REQUESTS, ERROR_RUN_LIMIT_TOKENS,
    ERROR_RUN_LIMIT_TOOL_CALLS, ERROR_RUN_LIMIT_WALL_TIME, ERROR_SESSION, ERROR_TOOL_EXECUTION,
    ERROR_TRANSPORT, ERROR_UNSUPPORTED_INPUT, ERROR_UNSUPPORTED_OUTPUT, PROVIDER_OPENAI,
    PROVIDER_OPENAI_CHATGPT, PROVIDER_OPENAI_COMPATIBLE,
};
#[cfg(any(test, feature = "providers-compat"))]
pub(crate) use label::{PROVIDER_ALIBABA_CLOUD, PROVIDER_META, PROVIDER_MOONSHOT_AI};
#[cfg(feature = "otel")]
pub use metric::LifecycleMetrics;
#[cfg(feature = "otel")]
pub(crate) use metric::{
    ATTRIBUTE_CONTEXT_OVERFLOW_ATTEMPT, ATTRIBUTE_ERROR_TYPE, ATTRIBUTE_OPERATION_NAME,
    ATTRIBUTE_OUTPUT_TYPE, ATTRIBUTE_PROVIDER_NAME, ATTRIBUTE_REPEATS, ATTRIBUTE_REQUEST_MODEL,
    ATTRIBUTE_RESPONSE_FINISH_REASONS, ATTRIBUTE_RESPONSE_ID, ATTRIBUTE_RESPONSE_MODEL,
    ATTRIBUTE_STOPPED, ATTRIBUTE_TOOL_CALL_ID, ATTRIBUTE_TOOL_NAME, ATTRIBUTE_TOOL_TYPE,
    ATTRIBUTE_USAGE_CACHE_READ_INPUT_TOKENS, ATTRIBUTE_USAGE_INPUT_TOKENS,
    ATTRIBUTE_USAGE_OUTPUT_TOKENS, ATTRIBUTE_USAGE_REASONING_OUTPUT_TOKENS,
    EVENT_CONTEXT_OVERFLOW_RETRY, EVENT_TOOL_CALLS_REPEATED, INSTRUMENTATION_SCOPE, OPERATION_CHAT,
    OPERATION_EXECUTE_TOOL, OPERATION_INVOKE_AGENT, OUTPUT_JSON, TOOL_TYPE_FUNCTION,
};
pub(crate) use propagation::with_current_context;
#[cfg(any(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
))]
pub(crate) use request::RequestMetrics;
