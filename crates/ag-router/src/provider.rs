//! Configured provider routing and provider-specific wire policies.

use std::collections::HashMap;
use std::sync::Arc;

use crate::chat_completion::{
    self, ChatCompletionBackend, ChatCompletionProviderPolicy, ReasoningFormat,
    StructuredOutputMode,
};
use crate::http::HttpTransport;
use crate::model::{Completion, GeneratedResponse, ModelError, ModelRequest, ReasoningEffort};
use crate::responses::{self, ResponsesBackend, ResponsesProviderPolicy};
use crate::{OutputSchema, TurnInput};

/// Built-in model provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Provider {
    /// Moonshot AI Kimi.
    Kimi,
    /// Meta Model API Muse.
    Muse,
    /// `OpenAI` models, such as the GPT models Codex runs, through the
    /// Responses API.
    OpenAi,
    /// Any server implementing the `OpenAI` Chat Completions API, such as
    /// Ollama, llama.cpp, LM Studio, or vLLM, under a conservative policy.
    OpenAiCompatible,
    /// Alibaba Cloud Model Studio Qwen.
    Qwen,
}

impl Provider {
    const ALL: [Self; 5] = [
        Self::Kimi,
        Self::Muse,
        Self::OpenAi,
        Self::OpenAiCompatible,
        Self::Qwen,
    ];

    /// Prefix used in a `provider/model` request identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kimi => "kimi",
            Self::Muse => "muse",
            Self::OpenAi => "openai",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Qwen => "qwen",
        }
    }

    /// Returns whether requests need an API key; local OpenAI-compatible
    /// servers usually accept unauthenticated requests.
    const fn requires_api_key(self) -> bool {
        !matches!(self, Self::OpenAiCompatible)
    }
}

/// Host-owned authentication and endpoint for one provider.
pub struct ProviderConfig {
    /// Built-in provider implementation.
    pub provider: Provider,
    /// Bearer token sent to the provider, or `None` to send no
    /// `Authorization` header. Only [`Provider::OpenAiCompatible`] accepts
    /// `None`; every provider rejects a blank key.
    pub api_key: Option<String>,
    /// API base URL, including its version path.
    pub base_url: String,
}

/// Single entry point for configured model providers.
pub struct Router {
    providers: HashMap<Provider, ProviderConfig>,
    client: Arc<dyn chat_completion::ChatCompletionClient>,
    responses: Arc<HttpTransport>,
}

impl Router {
    /// Creates a router for one provider configuration.
    pub fn single(config: ProviderConfig) -> Self {
        Self {
            providers: HashMap::from([(config.provider, config)]),
            client: chat_completion::default_client(),
            responses: responses::default_transport(),
        }
    }

    /// Creates a router from host-supplied provider configurations.
    ///
    /// # Errors
    /// Returns an error when a provider is configured more than once.
    pub fn new(configs: impl IntoIterator<Item = ProviderConfig>) -> Result<Self, ModelError> {
        let mut providers = HashMap::new();
        for config in configs {
            if providers.insert(config.provider, config).is_some() {
                return Err(ModelError::DuplicateProvider);
            }
        }
        Ok(Self {
            providers,
            client: chat_completion::default_client(),
            responses: responses::default_transport(),
        })
    }

    /// Validates whether the selected model can represent the output schema.
    ///
    /// # Errors
    /// Returns a model or schema capability error.
    pub fn validate_schema(&self, model: &str, schema: &OutputSchema) -> Result<(), ModelError> {
        self.backend(model)?.validate_schema(schema)
    }

    /// Validates whether the selected model accepts the input content.
    ///
    /// # Errors
    /// Returns a model or input capability error.
    pub fn validate_input(&self, model: &str, input: &TurnInput) -> Result<(), ModelError> {
        self.backend(model)?.validate_input(input)
    }

    /// Returns whether the selected model is asked for structured output by
    /// a leading prompt message stating the schema, so a request carrying
    /// [`crate::GenerationOptions::prefix_schema`] keeps the cached prefix of
    /// the conversation it continues while asking for another schema. A
    /// model constrained natively renders the requested schema into its
    /// prompt, so changing it loses that prefix.
    ///
    /// # Errors
    /// Returns a routing error for an unknown or unconfigured model.
    pub fn keeps_prefix_across_schemas(&self, model: &str) -> Result<bool, ModelError> {
        Ok(self.backend(model)?.keeps_prefix_across_schemas())
    }

    /// Executes one request and validates terminal JSON against its schema.
    ///
    /// # Errors
    /// Returns a routing, transport, decoding, or output validation error.
    pub async fn execute(&self, request: ModelRequest) -> Result<Completion, ModelError> {
        let backend = self.backend(&request.model)?;
        let response = backend.generate(&request).await?;
        match response {
            GeneratedResponse::Output {
                metadata,
                output,
                reasoning_content,
            } => Ok(Completion::Output {
                value: request
                    .schema()
                    .parse_and_validate(&output)
                    .map_err(|error| ModelError::from(error).with_metadata(metadata.clone()))?,
                metadata,
                reasoning_content,
            }),
            GeneratedResponse::ToolCall { call, metadata } => Ok(Completion::ToolCalls {
                calls: vec![call],
                metadata,
            }),
            GeneratedResponse::ToolCalls { calls, metadata } => {
                Ok(Completion::ToolCalls { calls, metadata })
            }
            GeneratedResponse::Failed { error, metadata } => Err(error.with_metadata(metadata)),
        }
    }

    fn backend(&self, id: &str) -> Result<Backend, ModelError> {
        let (prefix, model) = id.split_once('/').ok_or(ModelError::InvalidModelId)?;
        if model.trim().is_empty() || prefix.trim().is_empty() {
            return Err(ModelError::InvalidModelId);
        }
        let provider = Provider::ALL
            .into_iter()
            .find(|provider| provider.as_str() == prefix)
            .ok_or(ModelError::UnknownProvider)?;
        let config = self
            .providers
            .get(&provider)
            .ok_or(ModelError::UnknownProvider)?;
        let api_key_valid = config
            .api_key
            .as_deref()
            .map_or(!provider.requires_api_key(), |key| !key.trim().is_empty());
        if !api_key_valid || config.base_url.trim().is_empty() {
            return Err(ModelError::InvalidProviderConfig);
        }

        Ok(match policy(provider, model) {
            ProviderPolicy::ChatCompletion(policy) => {
                Backend::ChatCompletion(ChatCompletionBackend::with_client(
                    config.api_key.clone(),
                    config.base_url.clone(),
                    model.to_string(),
                    policy,
                    Arc::clone(&self.client),
                ))
            }
            ProviderPolicy::Responses(policy) => Backend::Responses(ResponsesBackend::new(
                config.api_key.clone(),
                config.base_url.clone(),
                model.to_string(),
                policy,
                Arc::clone(&self.responses),
            )),
        })
    }
}

/// Wire backend serving one routed model.
enum Backend {
    ChatCompletion(ChatCompletionBackend),
    Responses(ResponsesBackend),
}

impl Backend {
    fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        match self {
            Self::ChatCompletion(backend) => backend.validate_schema(schema),
            Self::Responses(backend) => backend.validate_schema(schema),
        }
    }

    /// Validates input content; every current `OpenAI` model reads images.
    fn validate_input(&self, input: &TurnInput) -> Result<(), ModelError> {
        match self {
            Self::ChatCompletion(backend) => backend.validate_input(input),
            Self::Responses(_) => Ok(()),
        }
    }

    /// Returns whether the schema is stated in a leading prompt message; the
    /// Responses API constrains output natively.
    fn keeps_prefix_across_schemas(&self) -> bool {
        match self {
            Self::ChatCompletion(backend) => backend.keeps_prefix_across_schemas(),
            Self::Responses(_) => false,
        }
    }

    async fn generate(&self, request: &ModelRequest) -> Result<GeneratedResponse, ModelError> {
        match self {
            Self::ChatCompletion(backend) => backend.generate(request).await,
            Self::Responses(backend) => backend.generate(request).await,
        }
    }
}

/// Wire policy of one provider model.
enum ProviderPolicy {
    ChatCompletion(ChatCompletionProviderPolicy),
    Responses(ResponsesProviderPolicy),
}

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

fn policy(provider: Provider, model: &str) -> ProviderPolicy {
    match provider {
        Provider::Kimi => ProviderPolicy::ChatCompletion(ChatCompletionProviderPolicy {
            context_overflow_markers: &[KIMI_CONTEXT_OVERFLOW],
            display_name: "Kimi",
            image_input: matches!(
                model,
                "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k2.7-code-highspeed" | "kimi-k3"
            ),
            prompt_cache_key: true,
            reasoning_format: match model {
                "kimi-k2.6" => ReasoningFormat::Thinking {
                    disable_supported: true,
                    preserve_reasoning: true,
                },
                "kimi-k2.7-code" => ReasoningFormat::Thinking {
                    disable_supported: false,
                    preserve_reasoning: false,
                },
                "kimi-k3" => ReasoningFormat::Effort(kimi_effort),
                _ => ReasoningFormat::None,
            },
            response_format_with_tools: false,
            structured_output: StructuredOutputMode::JsonObject {
                assistant_reasoning_content: matches!(
                    model,
                    "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k3"
                ),
                tool_result_name: true,
            },
            unsupported_schema_reason: "Kimi JSON Object mode requires an explicit object root \
                                        schema",
        }),
        Provider::Muse => ProviderPolicy::ChatCompletion(ChatCompletionProviderPolicy {
            context_overflow_markers: &[MUSE_CONTEXT_OVERFLOW],
            display_name: "Meta Model API",
            image_input: matches!(model, "muse-spark-1.3" | "muse-spark-1.3-contributor"),
            prompt_cache_key: true,
            reasoning_format: ReasoningFormat::Effort(muse_effort),
            response_format_with_tools: true,
            structured_output: StructuredOutputMode::JsonSchema,
            unsupported_schema_reason: "Muse structured output requires an explicit object root \
                                        schema",
        }),
        Provider::OpenAi => ProviderPolicy::Responses(ResponsesProviderPolicy {
            context_overflow_markers: &[OPENAI_CONTEXT_OVERFLOW],
            display_name: "OpenAI",
            reasoning_effort: if OPENAI_MAX_EFFORT_PREFIXES
                .iter()
                .any(|prefix| model.starts_with(prefix))
            {
                ReasoningEffort::as_str
            } else {
                openai_xhigh_effort
            },
            unsupported_schema_reason: "OpenAI structured output requires an explicit object root \
                                        schema",
        }),
        Provider::OpenAiCompatible => {
            ProviderPolicy::ChatCompletion(ChatCompletionProviderPolicy {
                context_overflow_markers: &[LLAMA_CPP_CONTEXT_OVERFLOW],
                display_name: "OpenAI-compatible server",
                image_input: false,
                prompt_cache_key: false,
                reasoning_format: ReasoningFormat::None,
                response_format_with_tools: false,
                structured_output: StructuredOutputMode::SchemaPrompt,
                unsupported_schema_reason: "OpenAI-compatible structured output requires an \
                                            explicit object root schema",
            })
        }
        Provider::Qwen => {
            let preserves_reasoning = model.starts_with("qwen3.8-");
            ProviderPolicy::ChatCompletion(ChatCompletionProviderPolicy {
                context_overflow_markers: &[QWEN_CONTEXT_OVERFLOW],
                display_name: "Qwen",
                image_input: model.starts_with("qwen-vl-")
                    || model.starts_with("qwen3-vl-")
                    || matches!(
                        model,
                        "qwen-plus" | "qwen3.8-27b" | "qwen3.8-flash" | "qwen3.8-max"
                    ),
                prompt_cache_key: false,
                reasoning_format: if preserves_reasoning {
                    ReasoningFormat::Effort(qwen_effort)
                } else if model == "qwen-plus" {
                    ReasoningFormat::EnableThinking
                } else {
                    ReasoningFormat::None
                },
                response_format_with_tools: false,
                structured_output: StructuredOutputMode::JsonObject {
                    assistant_reasoning_content: preserves_reasoning,
                    tool_result_name: false,
                },
                unsupported_schema_reason: "Qwen JSON Object mode requires an explicit object \
                                            root schema",
            })
        }
    }
}

/// Maps `max` to `xhigh`, the deepest effort of `OpenAI` models before
/// GPT-5.6, such as `gpt-5.3-codex`.
fn openai_xhigh_effort(effort: ReasoningEffort) -> &'static str {
    if effort == ReasoningEffort::Max {
        "xhigh"
    } else {
        effort.as_str()
    }
}

fn kimi_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium | ReasoningEffort::High => "high",
        ReasoningEffort::XHigh | ReasoningEffort::Max => "max",
    }
}

fn muse_effort(effort: ReasoningEffort) -> &'static str {
    if effort == ReasoningEffort::Max {
        "xhigh"
    } else {
        effort.as_str()
    }
}

fn qwen_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low | ReasoningEffort::Medium => effort.as_str(),
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => "xhigh",
    }
}

#[cfg(test)]
#[path = "provider_test.rs"]
mod tests;
