//! [`ProviderSpec`] presets of the built-in providers.
//!
//! Each preset routes under a fixed identifier: `chatgpt`, `kimi`, `muse`,
//! `openai`, `openai-compatible`, and `qwen`. Every preset except
//! [`openai_compatible`] is tailored to the model it is built for and
//! serves only that model ([`ProviderSpec::for_model`]), so a router serving
//! several models of one provider holds one preset per model. A preset is
//! an ordinary specification, so a host can adjust it with the `with_*`
//! methods, such as adding a header.
//!
//! [`ProviderSpec::new`] sends the standard Chat Completions request, so each
//! Chat Completions preset opts in to the quirks its provider needs: the
//! legacy `max_tokens` field for Kimi, Qwen, and OpenAI-compatible servers,
//! and turn instructions joined to the preceding user message for every
//! one of them.
//!
//! A preset given a model it does not know serves it under the provider's
//! shared policy alone, without the model-specific options: [`kimi`] and
//! [`qwen`] then send no reasoning control and accept no image input or
//! reasoning-text replay, and [`muse`] accepts no image input. Presets
//! taking both a base URL and a key take them in that order; a key passed
//! as the base URL fails validation without the key appearing in the error.

use std::sync::Arc;

use crate::credential::CredentialSource;
use crate::model::ReasoningEffort;
use crate::provider_spec::{
    Auth, EffortNames, InstructionPlacement, MaxTokensField, ProviderSpec, ReasoningMapping,
    StructuredOutputMode, ThinkingOptions, WireProtocol,
};

/// Kimi's `400` message for input over the window, as in "Your request
/// exceeded model token limit: 262144 (requested: 422274)".
const KIMI_CONTEXT_OVERFLOW: &str = "exceeded model token limit";

/// The Meta Model API's whole `400` message for input over the window. It
/// names no limit, but every other request validation failure observed live
/// names the offending parameter instead, so only an overflow carries it.
const MUSE_CONTEXT_OVERFLOW: &str = "the request contains invalid parameters. check the request \
                                     body for any errors or inconsistencies.";

/// llama.cpp server's `400` error type for input over the window, as in
/// "the request exceeds the available context size, try increasing it".
/// vLLM reports the shared `OpenAI` markers, while Ollama truncates
/// oversized input silently instead of failing.
const LLAMA_CPP_CONTEXT_OVERFLOW: &str = "exceed_context_size_error";

/// `DashScope`'s `400` message for input over the window, as in "Range of
/// input length should be [1, 983616]".
const QWEN_CONTEXT_OVERFLOW: &str = "range of input length";

/// The `OpenAI` Responses API's message for input over the window, as in
/// "Your input exceeds the context window of this model", which a streamed
/// `response.failed` may carry beside the shared `context_length_exceeded`
/// code.
const OPENAI_CONTEXT_OVERFLOW: &str = "exceeds the context window";

/// Prefixes of the `OpenAI` model families that accept the `max` reasoning
/// effort; earlier models, such as `gpt-5.3-codex`, top out at `xhigh`.
const OPENAI_MAX_EFFORT_PREFIXES: [&str; 2] = ["gpt-5.6-", "gpt-6"];

/// `OpenAI` models billed to the user's `ChatGPT` plan through "Sign in with
/// `ChatGPT`": the Responses API authenticated by a refreshing `credential`,
/// without an output-token bound, which the plan backend rejects.
pub fn chatgpt(
    base_url: impl Into<String>,
    credential: Arc<dyn CredentialSource>,
    model: &str,
) -> ProviderSpec {
    openai_responses("chatgpt", "ChatGPT plan", base_url, model)
        .with_auth(Auth::Credential(credential))
        .with_max_tokens_field(None)
}

/// Moonshot AI Kimi through its Chat Completions API: JSON Object mode,
/// `prompt_cache_key`, and the reasoning controls and image input of
/// `model`.
pub fn kimi(base_url: impl Into<String>, api_key: impl Into<String>, model: &str) -> ProviderSpec {
    let reasoning = match model {
        "kimi-k2.6" => ReasoningMapping::Thinking(
            ThinkingOptions::default()
                .with_disable_supported(true)
                .with_preserve_reasoning(true),
        ),
        "kimi-k2.7-code" => ReasoningMapping::Thinking(ThinkingOptions::default()),
        "kimi-k3" => ReasoningMapping::Effort(
            EffortNames::default()
                .with(ReasoningEffort::Medium, "high")
                .with(ReasoningEffort::XHigh, "max"),
        ),
        _ => ReasoningMapping::None,
    };

    ProviderSpec::new("kimi", WireProtocol::ChatCompletions, base_url)
        .for_model(model)
        .with_reasoning_text_replay(matches!(model, "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k3"))
        .with_auth(Auth::Bearer(api_key.into()))
        .with_display_name("Kimi")
        .with_image_input(matches!(
            model,
            "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k2.7-code-highspeed" | "kimi-k3"
        ))
        .with_instruction_placement(InstructionPlacement::JoinPreviousUser)
        .with_max_tokens_field(Some(MaxTokensField::MaxTokens))
        .with_overflow_marker(KIMI_CONTEXT_OVERFLOW)
        .with_prompt_cache_key(true)
        .with_reasoning(reasoning)
        .with_structured_output(StructuredOutputMode::JsonObject)
        .with_tool_result_name(true)
}

/// Meta Model API Muse through its Chat Completions API: native JSON Schema
/// output, also beside tools, `prompt_cache_key`, and reasoning effort.
pub fn muse(base_url: impl Into<String>, api_key: impl Into<String>, model: &str) -> ProviderSpec {
    ProviderSpec::new("muse", WireProtocol::ChatCompletions, base_url)
        .for_model(model)
        .with_auth(Auth::Bearer(api_key.into()))
        .with_display_name("Meta Model API")
        .with_image_input(matches!(
            model,
            "muse-spark-1.3" | "muse-spark-1.3-contributor"
        ))
        .with_instruction_placement(InstructionPlacement::JoinPreviousUser)
        .with_max_tokens_field(Some(MaxTokensField::MaxCompletionTokens))
        .with_overflow_marker(MUSE_CONTEXT_OVERFLOW)
        .with_prompt_cache_key(true)
        .with_reasoning(ReasoningMapping::Effort(
            EffortNames::default().with(ReasoningEffort::Max, "xhigh"),
        ))
        .with_response_format_with_tools(true)
        .with_structured_output(StructuredOutputMode::JsonSchema)
}

/// `OpenAI` models, such as the GPT models Codex runs, through the
/// Responses API billed to an API key.
pub fn openai(
    base_url: impl Into<String>,
    api_key: impl Into<String>,
    model: &str,
) -> ProviderSpec {
    openai_responses("openai", "OpenAI", base_url, model).with_auth(Auth::Bearer(api_key.into()))
}

/// Any server implementing the `OpenAI` Chat Completions API, such as
/// Ollama, llama.cpp, LM Studio, or vLLM, under the portable capabilities of
/// [`ProviderSpec::new`] with the quirks local servers need: the legacy
/// `max_tokens` field and turn instructions joined to the preceding user
/// message. A server that supports more opts in with the `with_*` methods,
/// such as [`ProviderSpec::with_image_input`] for a vision model or
/// [`ProviderSpec::with_structured_output`] for native JSON Schema; `None`
/// sends no `Authorization` header.
pub fn openai_compatible(base_url: impl Into<String>, api_key: Option<String>) -> ProviderSpec {
    ProviderSpec::new("openai-compatible", WireProtocol::ChatCompletions, base_url)
        .with_auth(api_key.map_or(Auth::None, Auth::Bearer))
        .with_display_name("OpenAI-compatible server")
        .with_instruction_placement(InstructionPlacement::JoinPreviousUser)
        .with_max_tokens_field(Some(MaxTokensField::MaxTokens))
        .with_overflow_marker(LLAMA_CPP_CONTEXT_OVERFLOW)
}

/// Alibaba Cloud Model Studio Qwen through its Chat Completions API: JSON
/// Object mode and the reasoning controls and image input of `model`.
pub fn qwen(base_url: impl Into<String>, api_key: impl Into<String>, model: &str) -> ProviderSpec {
    let preserves_reasoning = model.starts_with("qwen3.8-");
    let reasoning = if preserves_reasoning {
        ReasoningMapping::Effort(
            EffortNames::default()
                .with(ReasoningEffort::High, "xhigh")
                .with(ReasoningEffort::Max, "xhigh"),
        )
    } else if model == "qwen-plus" {
        ReasoningMapping::EnableThinking
    } else {
        ReasoningMapping::None
    };

    ProviderSpec::new("qwen", WireProtocol::ChatCompletions, base_url)
        .for_model(model)
        .with_reasoning_text_replay(preserves_reasoning)
        .with_auth(Auth::Bearer(api_key.into()))
        .with_display_name("Qwen")
        .with_image_input(
            model.starts_with("qwen-vl-")
                || model.starts_with("qwen3-vl-")
                || matches!(
                    model,
                    "qwen-plus" | "qwen3.8-27b" | "qwen3.8-flash" | "qwen3.8-max"
                ),
        )
        .with_instruction_placement(InstructionPlacement::JoinPreviousUser)
        .with_max_tokens_field(Some(MaxTokensField::MaxTokens))
        .with_overflow_marker(QWEN_CONTEXT_OVERFLOW)
        .with_reasoning(reasoning)
        .with_structured_output(StructuredOutputMode::JsonObject)
}

/// Responses API policy shared by the `OpenAI` models, reached with an API
/// key or billed to a `ChatGPT` plan: `max` maps to `xhigh` before GPT-5.6.
fn openai_responses(
    id: &str,
    display_name: &str,
    base_url: impl Into<String>,
    model: &str,
) -> ProviderSpec {
    let max_effort = if OPENAI_MAX_EFFORT_PREFIXES
        .iter()
        .any(|prefix| model.starts_with(prefix))
    {
        "max"
    } else {
        "xhigh"
    };

    ProviderSpec::new(id, WireProtocol::Responses, base_url)
        .for_model(model)
        .with_display_name(display_name)
        .with_overflow_marker(OPENAI_CONTEXT_OVERFLOW)
        .with_reasoning(ReasoningMapping::Effort(
            EffortNames::default().with(ReasoningEffort::Max, max_effort),
        ))
}

#[cfg(test)]
#[path = "preset_test.rs"]
mod tests;
