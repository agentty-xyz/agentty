//! Stable, low-cardinality error-type labels that lifecycle events and
//! errors report whether or not OpenTelemetry export is compiled in.

pub(crate) const ERROR_CANCELLED: &str = "cancelled";
pub(crate) const ERROR_INVALID_OUTPUT: &str = "invalid_output";
pub(crate) const ERROR_INVALID_PROVIDER_RESPONSE: &str = "invalid_provider_response";
pub(crate) const ERROR_INVALID_RESPONSE: &str = "invalid_response";
pub(crate) const ERROR_INVALID_TOOL_CALL: &str = "invalid_tool_call";
pub(crate) const ERROR_PROVIDER: &str = "provider_error";
pub(crate) const ERROR_REQUEST: &str = "request_error";
pub(crate) const ERROR_RESPONSE_TOO_LARGE: &str = "response_too_large";
pub(crate) const ERROR_TRANSPORT: &str = "transport_error";
pub(crate) const ERROR_TOOL_EXECUTION: &str = "tool_execution_error";
pub(crate) const ERROR_APPROVAL_DENIED: &str = "approval_denied";
pub(crate) const ERROR_APPROVAL_TIMEOUT: &str = "approval_timeout";
pub(crate) const ERROR_REPOSITORY_REQUIRED: &str = "repository_required";
pub(crate) const ERROR_COMPARISON_REPOSITORY_MISMATCH: &str = "comparison_repository_mismatch";
pub(crate) const ERROR_CONTEXT_BUDGET: &str = "context_budget_exceeded";
pub(crate) const ERROR_CONTEXT_OVERFLOW: &str = "context_overflow";
pub(crate) const ERROR_REPEATED_TOOL_CALLS: &str = "repeated_tool_calls";
pub(crate) const ERROR_RUN_LIMIT_MODEL_REQUESTS: &str = "run_limit_model_requests";
pub(crate) const ERROR_RUN_LIMIT_TOOL_CALLS: &str = "run_limit_tool_calls";
pub(crate) const ERROR_RUN_LIMIT_TOKENS: &str = "run_limit_tokens";
pub(crate) const ERROR_RUN_LIMIT_WALL_TIME: &str = "run_limit_wall_time";
pub(crate) const ERROR_BUDGET_LEDGER_COST: &str = "budget_ledger_cost";
pub(crate) const ERROR_BUDGET_LEDGER_TOKENS: &str = "budget_ledger_tokens";
pub(crate) const ERROR_BUDGET_LEDGER_MODEL_REQUESTS: &str = "budget_ledger_model_requests";
pub(crate) const ERROR_BUDGET_LEDGER_TOOL_CALLS: &str = "budget_ledger_tool_calls";
pub(crate) const ERROR_BUDGET_LEDGER_WALL_TIME: &str = "budget_ledger_wall_time";
pub(crate) const ERROR_CONTEXT_SUMMARY_BUDGET_EXHAUSTED: &str = "context_summary_budget_exhausted";
pub(crate) const ERROR_CONTEXT_SUMMARY_INVALID_OUTPUT: &str = "context_summary_invalid_output";
pub(crate) const ERROR_CONTEXT_SUMMARY_MODEL: &str = "context_summary_model_error";
pub(crate) const ERROR_CONTEXT_SUMMARY_REJECTED: &str = "context_summary_rejected";
pub(crate) const ERROR_SANDBOX_UNAVAILABLE: &str = "sandbox_unavailable";
pub(crate) const ERROR_SESSION: &str = "session_error";
pub(crate) const ERROR_UNSUPPORTED_INPUT: &str = "unsupported_input";
pub(crate) const ERROR_UNSUPPORTED_OUTPUT: &str = "unsupported_output";

/// Provider names that telemetry and persisted model metadata use.
#[cfg(any(test, feature = "providers-compat"))]
pub(crate) const PROVIDER_ALIBABA_CLOUD: &str = "alibaba_cloud";
#[cfg(feature = "providers-anthropic")]
pub(crate) const PROVIDER_ANTHROPIC: &str = "anthropic";
#[cfg(any(test, feature = "providers-compat"))]
pub(crate) const PROVIDER_META: &str = "meta";
#[cfg(any(test, feature = "providers-compat"))]
pub(crate) const PROVIDER_MOONSHOT_AI: &str = "moonshot_ai";
pub(crate) const PROVIDER_OPENAI: &str = "openai";
/// Provider name for `OpenAI` billed to a `ChatGPT` plan, kept apart from
/// [`PROVIDER_OPENAI`] so a resumed session keeps its billing.
pub(crate) const PROVIDER_OPENAI_CHATGPT: &str = "openai_chatgpt";
/// Provider name for any OpenAI-compatible server, which `gen_ai` semantic
/// conventions do not enumerate.
pub(crate) const PROVIDER_OPENAI_COMPATIBLE: &str = "openai_compatible";
