//! Open provider specifications: the wire protocol, endpoint,
//! authentication, extra headers and query parameters, and request policy
//! that let a host add any OpenAI-compatible, Responses, or Anthropic
//! Messages provider without code changes, served through
//! [`crate::model::ModelClient::from_spec`].

use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

#[cfg(feature = "providers-anthropic")]
use super::AnthropicConfig;
#[cfg(feature = "providers-openai")]
use super::{ChatGptConfig, OpenAiConfig};
#[cfg(feature = "providers-compat")]
use super::{KimiConfig, MuseConfig, OpenAiCompatibleConfig, QwenConfig};
use crate::model::{ModelMetadataError, ReasoningEffort, ReasoningFamily};
use crate::telemetry;

/// Routing prefix of every host-defined specification; each client routes
/// to one provider, so the prefix never reaches the wire.
const CUSTOM_ROUTE: &str = "custom";

/// Wire API a provider implements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WireProtocol {
    /// The Anthropic Messages API (`POST {base_url}/messages`), sent
    /// unstreamed: Claude models with signed thinking replayed verbatim to
    /// models of the same reasoning family.
    AnthropicMessages,
    /// The `OpenAI` Chat Completions API (`POST {base_url}/chat/completions`),
    /// implemented by most OpenAI-compatible servers.
    ChatCompletions,
    /// The `OpenAI` Responses API (`POST {base_url}/responses`), streamed and
    /// stateless.
    Responses,
}

/// Bearer token source asked for a token before every request, such as a
/// refreshing sign-in like [`super::ChatGptAuth`]; configured as
/// [`Auth::Credential`].
#[async_trait]
pub trait CredentialSource: Send + Sync {
    /// Returns a bearer token valid for the next request.
    ///
    /// # Errors
    /// Returns an error when no valid token can be produced, such as when the
    /// user must sign in again; the request then fails as a provider error.
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>>;
}

#[cfg(feature = "providers-openai")]
#[async_trait]
impl CredentialSource for ag_router::chatgpt::ChatGptAuth {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        ag_router::CredentialSource::bearer_token(self).await
    }
}

/// How requests authenticate with a provider.
///
/// Keys are literal values the host has already resolved, from the
/// environment, a secret store, or its own configuration; the harness never
/// reads environment variables on a specification's behalf.
#[derive(Clone)]
#[non_exhaustive]
pub enum Auth {
    /// Send no authentication header, as many local servers expect.
    None,
    /// Send `Authorization: Bearer <key>`.
    Bearer(String),
    /// Send the key in a named header, such as Azure `OpenAI`'s `api-key`.
    Header {
        /// Header name.
        name: String,
        /// Header value.
        value: String,
    },
    /// Send `Authorization: Bearer <token>` with a token the source returns
    /// before each request.
    Credential(Arc<dyn CredentialSource>),
}

impl fmt::Debug for Auth {
    /// Names the authentication kind without revealing any secret.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.clone().into_router(), formatter)
    }
}

impl Auth {
    /// Converts to the routed authentication.
    fn into_router(self) -> ag_router::Auth {
        match self {
            Self::None => ag_router::Auth::None,
            Self::Bearer(key) => ag_router::Auth::Bearer(key),
            Self::Header { name, value } => ag_router::Auth::Header { name, value },
            Self::Credential(source) => {
                ag_router::Auth::Credential(Arc::new(RoutedCredential(source)))
            }
        }
    }
}

/// How a provider is asked for output matching the request's JSON Schema;
/// every mode validates the answer locally afterwards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StructuredOutputMode {
    /// Constrain output natively: the `json_schema` response format, or the
    /// Anthropic Messages `output_config.format`. The only mode the Responses
    /// API supports.
    JsonSchema,
    /// Advertise a `structured_output` tool whose input schema is the output
    /// schema, and read its input as the answer (Anthropic Messages only).
    /// A request without tools or a reasoning effort forces that tool; one
    /// with tools requires some tool call; one with an effort, which turns
    /// thinking on and so forbids forcing a tool, lets the model choose and
    /// also reads a text answer. For models without native structured
    /// output or with schemas it rejects; current Claude models reject a
    /// forced tool.
    ForcedTool,
    /// State the schema in a leading system message and request the
    /// `json_object` response format.
    JsonObject,
    /// State the schema in a leading system message, worded to invite tool
    /// calls first, and constrain requests without tools by the
    /// `json_schema` response format.
    SchemaPrompt,
}

impl StructuredOutputMode {
    /// Converts to the routed structured-output mode.
    const fn to_router(self) -> ag_router::StructuredOutputMode {
        match self {
            Self::ForcedTool => ag_router::StructuredOutputMode::ForcedTool,
            Self::JsonSchema => ag_router::StructuredOutputMode::JsonSchema,
            Self::JsonObject => ag_router::StructuredOutputMode::JsonObject,
            Self::SchemaPrompt => ag_router::StructuredOutputMode::SchemaPrompt,
        }
    }
}

/// How a requested [`ReasoningEffort`] reaches the provider.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReasoningMapping {
    /// Send no reasoning control.
    None,
    /// Send the effort under these wire names: `reasoning_effort` for Chat
    /// Completions, `reasoning.effort` for the Responses API, and
    /// `output_config.effort` beside summarized adaptive thinking for
    /// Anthropic Messages.
    Effort(EffortNames),
    /// Send `enable_thinking`, off for the low effort (Chat Completions
    /// only).
    EnableThinking,
    /// Send a `thinking` object (Chat Completions only).
    Thinking(ThinkingOptions),
}

impl ReasoningMapping {
    /// Converts to the routed reasoning mapping.
    fn into_router(self) -> ag_router::ReasoningMapping {
        match self {
            Self::None => ag_router::ReasoningMapping::None,
            Self::Effort(EffortNames(names)) => ag_router::ReasoningMapping::Effort(names),
            Self::EnableThinking => ag_router::ReasoningMapping::EnableThinking,
            Self::Thinking(ThinkingOptions(options)) => {
                ag_router::ReasoningMapping::Thinking(options)
            }
        }
    }
}

/// Options of a [`ReasoningMapping::Thinking`] control; the default keeps
/// thinking on for a low effort and sends nothing without an effort.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThinkingOptions(ag_router::ThinkingOptions);

impl ThinkingOptions {
    /// Sets whether a low effort turns thinking off.
    #[must_use]
    pub fn with_disable_supported(self, disable_supported: bool) -> Self {
        Self(self.0.with_disable_supported(disable_supported))
    }

    /// Sets whether thinking stays on, and keeps all earlier reasoning,
    /// when no effort is requested.
    #[must_use]
    pub fn with_preserve_reasoning(self, preserve_reasoning: bool) -> Self {
        Self(self.0.with_preserve_reasoning(preserve_reasoning))
    }
}

/// Wire names of each reasoning effort; the default sends each effort under
/// its own name, and [`Self::with`] renames one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffortNames(ag_router::EffortNames);

impl EffortNames {
    /// Sends `effort` under `name`.
    #[must_use]
    pub fn with(self, effort: ReasoningEffort, name: impl Into<String>) -> Self {
        Self(self.0.with(effort.to_router(), name))
    }
}

/// Where a Chat Completions request places a turn's instructions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstructionPlacement {
    /// Their own user message, the standard request.
    #[default]
    Separate,
    /// Appended to the user message they follow, for chat templates that
    /// reject two user messages in a row; without a preceding user message
    /// they form their own.
    JoinPreviousUser,
}

impl InstructionPlacement {
    /// Converts to the routed instruction placement.
    const fn to_router(self) -> ag_router::InstructionPlacement {
        match self {
            Self::Separate => ag_router::InstructionPlacement::Separate,
            Self::JoinPreviousUser => ag_router::InstructionPlacement::JoinPreviousUser,
        }
    }
}

/// Body field carrying the output-token bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaxTokensField {
    /// The current `OpenAI` Chat Completions name, `max_completion_tokens`.
    MaxCompletionTokens,
    /// The Responses API's `max_output_tokens`, at least 16.
    MaxOutputTokens,
    /// The original Chat Completions `max_tokens`, which most compatible
    /// servers read, and the field Anthropic Messages requires.
    MaxTokens,
}

/// Transient failure a [`RetryPolicy`] may retry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum RetryKind {
    /// HTTP `429` without a spent-quota marker; a `429` reporting a spent
    /// quota is never retried.
    RateLimited,
    /// HTTP `500`, `502`, `503`, or `504`.
    ServerError,
    /// HTTP `529`, an overloaded provider.
    Overloaded,
    /// The connection failed before any response; a timed-out request is
    /// never retried.
    Connection,
}

impl RetryKind {
    /// Converts to the routed retry kind.
    const fn to_router(self) -> ag_router::RetryKind {
        match self {
            Self::RateLimited => ag_router::RetryKind::RateLimited,
            Self::ServerError => ag_router::RetryKind::ServerError,
            Self::Overloaded => ag_router::RetryKind::Overloaded,
            Self::Connection => ag_router::RetryKind::Connection,
        }
    }
}

/// Random spread applied to each computed retry backoff, so clients limited
/// together do not retry together; a provider's `Retry-After` is never
/// shortened by it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Jitter {
    /// Wait exactly the computed backoff.
    #[default]
    None,
    /// Wait a uniformly random delay between zero and the backoff.
    Full,
    /// Wait half the backoff plus a uniformly random delay up to the other
    /// half.
    Equal,
}

impl Jitter {
    /// Converts to the routed jitter.
    const fn to_router(self) -> ag_router::Jitter {
        match self {
            Self::None => ag_router::Jitter::None,
            Self::Full => ag_router::Jitter::Full,
            Self::Equal => ag_router::Jitter::Equal,
        }
    }
}

/// How a provider's failed requests are retried, set with
/// [`ProviderSpec::with_retry`]; each retry is reported to a streamed run as
/// a [`crate::event::RunEventKind::Retrying`] event before its wait.
///
/// The default, which every preset uses, retries a `429` without a
/// spent-quota marker up to five times and one connection failure. Each
/// retry waits an exponential backoff from one second, doubled per status
/// retry and capped at five seconds, or the provider's `Retry-After`, as
/// delta-seconds or an HTTP date capped at sixty seconds, whichever is
/// longer. Server errors (`500`, `502`-`504`) and an overloaded `529` are
/// opt-ins through [`Self::with_retried`]; a spent quota (`402`, or a `429`
/// reporting one) and a timed-out request are never retried. Cancelling a
/// run stops a pending wait.
///
/// Unsuccessful statuses and connection failures have separate budgets: a
/// model request sends at most [`Self::with_max_attempts`] requests
/// answered with retried statuses, and retries at most
/// [`Self::with_max_connection_retries`] connection failures beside them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetryPolicy(ag_router::RetryPolicy);

impl RetryPolicy {
    /// Returns a policy that retries nothing, with the default delays and
    /// budgets for any kind [`Self::with_retried`] turns back on.
    pub fn none() -> Self {
        Self(ag_router::RetryPolicy::none())
    }

    /// Sets the backoff before the first status retry, doubled for each
    /// later one; a connection retry waits it undoubled.
    #[must_use]
    pub fn with_base_delay(self, base_delay: Duration) -> Self {
        Self(self.0.with_base_delay(base_delay))
    }

    /// Sets the random spread of each computed backoff.
    #[must_use]
    pub fn with_jitter(self, jitter: Jitter) -> Self {
        Self(self.0.with_jitter(jitter.to_router()))
    }

    /// Sets the most requests one model request sends while each is
    /// answered with a retried status, the first included, so at most
    /// `max_attempts - 1` status retries; `0` reads as `1`, retrying no
    /// status. Connection retries do not count against it.
    #[must_use]
    pub fn with_max_attempts(self, max_attempts: u32) -> Self {
        Self(self.0.with_max_attempts(max_attempts))
    }

    /// Sets the ceiling of the exponential backoff.
    #[must_use]
    pub fn with_max_backoff(self, max_backoff: Duration) -> Self {
        Self(self.0.with_max_backoff(max_backoff))
    }

    /// Sets how many connection failures one model request retries, beside
    /// its status attempts; they are retried only while
    /// [`RetryKind::Connection`] is, so `0` and turning that kind off both
    /// disable them.
    #[must_use]
    pub fn with_max_connection_retries(self, max_connection_retries: u32) -> Self {
        Self(self.0.with_max_connection_retries(max_connection_retries))
    }

    /// Sets the ceiling of a provider-declared `Retry-After`.
    #[must_use]
    pub fn with_max_retry_after(self, max_retry_after: Duration) -> Self {
        Self(self.0.with_max_retry_after(max_retry_after))
    }

    /// Sets whether failures of `kind` are retried.
    #[must_use]
    pub fn with_retried(self, kind: RetryKind, retried: bool) -> Self {
        Self(self.0.with_retried(kind.to_router(), retried))
    }
}

/// Failure to build a client from a [`ProviderSpec`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderSpecError {
    /// The provider name or model identifier is blank.
    #[error(transparent)]
    Metadata(#[from] ModelMetadataError),
    /// No request can be sent with the specification.
    #[error("{reason}")]
    Invalid {
        /// Why no request can be sent, naming the field that failed.
        reason: String,
    },
    /// The specification names no model to serve; set one with
    /// [`ProviderSpec::for_model`].
    #[error("provider specification names no model")]
    MissingModel,
}

/// Wire protocol, endpoint, authentication, and request policy of one
/// provider, served by [`crate::model::ModelClient::from_spec`].
///
/// [`Self::new`] starts from a conservative policy for the protocol,
/// [`Self::for_model`] names the model a client serves, and the `with_*`
/// methods adjust the policy. [`Self::name`] is the provider name that
/// telemetry, persisted model metadata, and provider errors carry, and
/// [`Self::reasoning_family`] decides which recorded turns its model replays
/// with their provider reasoning intact.
///
/// The built-in providers are presets, such as [`Self::kimi`], each behind
/// its family's `providers-*` feature, serving the model of its
/// configuration under a policy tailored to that model. A preset keeps the
/// human-readable product name, such as `Kimi`, in provider errors.
///
/// [`Self::validate`] rejects a specification no request can be sent with,
/// and [`crate::model::ModelClient::from_spec`] runs it. The debug output
/// never prints a key, header value, or query value.
#[derive(Clone, Debug)]
pub struct ProviderSpec {
    name: Cow<'static, str>,
    reasoning_family: ReasoningFamily,
    spec: ag_router::ProviderSpec,
}

impl ProviderSpec {
    /// Creates an unauthenticated specification for the provider `name`.
    ///
    /// Chat Completions starts from the standard `OpenAI` request with no
    /// optional capabilities: the schema stated in the prompt, no reasoning
    /// controls, reasoning-text replay, prompt cache key, or image input,
    /// and the bound sent as `max_completion_tokens`, the current `OpenAI`
    /// field. A server that ignores that field leaves output unbounded, so it
    /// needs `with_max_tokens_field(Some(MaxTokensField::MaxTokens))`.
    /// Provider quirks are opt-ins: the legacy `max_tokens` field
    /// ([`Self::with_max_tokens_field`]), turn instructions joined to the
    /// preceding user message ([`Self::with_instruction_placement`]),
    /// reasoning-text replay, and tool-result names; each Chat Completions
    /// preset, such as [`Self::openai_compatible`], enables those its provider
    /// needs. The Responses API starts with native structured output, efforts
    /// under their own names, image input, `prompt_cache_key`, and
    /// `max_output_tokens`. Anthropic Messages starts with native structured
    /// output, efforts under their own names, image input, and the required
    /// `max_tokens`, filled from [`Self::with_default_max_output_tokens`] when
    /// a request sets no bound; it needs `x-api-key` authentication
    /// ([`Auth::Header`]) and an `anthropic-version` header
    /// ([`Self::with_header`]), which the `Self::anthropic` preset sets. Each
    /// names no model yet ([`Self::for_model`])
    /// and starts in the reasoning family [`ModelMetadata::new`] assigns
    /// `name`, so a built-in provider name keeps its built-in family.
    ///
    /// [`ModelMetadata::new`]: crate::model::ModelMetadata::new
    pub fn new(
        name: impl Into<Cow<'static, str>>,
        protocol: WireProtocol,
        base_url: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let protocol = match protocol {
            WireProtocol::AnthropicMessages => ag_router::WireProtocol::AnthropicMessages,
            WireProtocol::ChatCompletions => ag_router::WireProtocol::ChatCompletions,
            WireProtocol::Responses => ag_router::WireProtocol::Responses,
        };
        let spec = ag_router::ProviderSpec::new(CUSTOM_ROUTE, protocol, base_url)
            .with_display_name(name.as_ref());

        Self {
            reasoning_family: ReasoningFamily::for_provider(&name),
            name,
            spec,
        }
    }

    /// Claude models through the Anthropic Messages API, authenticated by
    /// `config.api_key` in the `x-api-key` header: native structured output,
    /// image input, and, for the models that take them, efforts with
    /// summarized adaptive thinking. Every Claude model shares one reasoning
    /// family, so a switch between Claude models replays signed thinking,
    /// which the API ignores for a model that cannot read it, while a switch
    /// to another provider carries it as a short note. Serve it with
    /// [`crate::model::ModelClient::from_spec`]; the catalog lists no Claude
    /// model yet, so the host declares the context profile and prices.
    #[cfg(feature = "providers-anthropic")]
    pub fn anthropic(config: AnthropicConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_ANTHROPIC,
            ag_router::preset::anthropic(config.base_url, config.api_key, &config.model),
        )
    }

    /// `OpenAI` models through the Responses API billed to an API key, as
    /// [`crate::model::ModelClient::openai`] sends them. They share their
    /// reasoning family with [`Self::chatgpt`] models.
    #[cfg(feature = "providers-openai")]
    pub fn openai(config: OpenAiConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_OPENAI,
            ag_router::preset::openai(config.base_url, config.api_key, &config.model),
        )
    }

    /// `OpenAI` models billed to a `ChatGPT` plan, authenticated by the
    /// sign-in stored at `config.auth_file`, as
    /// [`crate::model::ModelClient::chatgpt`] sends them. Its own provider
    /// name keeps plan billing apart from `OpenAI` API billing, so a resumed
    /// session keeps its billing, while its reasoning family includes
    /// [`Self::openai`] models.
    #[cfg(feature = "providers-openai")]
    pub fn chatgpt(config: ChatGptConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_OPENAI_CHATGPT,
            ag_router::preset::chatgpt(
                config.base_url,
                Arc::new(ag_router::chatgpt::ChatGptAuth::new(config.auth_file)),
                &config.model,
            ),
        )
    }

    /// Moonshot AI Kimi, as [`crate::model::ModelClient::kimi`] sends it.
    #[cfg(feature = "providers-compat")]
    pub fn kimi(config: KimiConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_MOONSHOT_AI,
            ag_router::preset::kimi(config.base_url, config.api_key, &config.model),
        )
    }

    /// Meta Model API Muse, as [`crate::model::ModelClient::muse`] sends it.
    #[cfg(feature = "providers-compat")]
    pub fn muse(config: MuseConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_META,
            ag_router::preset::muse(config.base_url, config.api_key, &config.model),
        )
    }

    /// A local or self-hosted OpenAI-compatible server, as
    /// [`crate::model::ModelClient::openai_compatible`] sends it; `gen_ai`
    /// semantic conventions do not enumerate such servers, so they get
    /// their own provider name. Unrelated servers share that name, so each
    /// model is its own reasoning family.
    #[cfg(feature = "providers-compat")]
    pub fn openai_compatible(config: OpenAiCompatibleConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_OPENAI_COMPATIBLE,
            ag_router::preset::openai_compatible(config.base_url, config.api_key)
                .for_model(config.model),
        )
    }

    /// Alibaba Cloud Model Studio Qwen, as
    /// [`crate::model::ModelClient::qwen`] sends it.
    #[cfg(feature = "providers-compat")]
    pub fn qwen(config: QwenConfig) -> Self {
        Self::preset(
            telemetry::PROVIDER_ALIBABA_CLOUD,
            ag_router::preset::qwen(config.base_url, config.api_key, &config.model),
        )
    }

    /// Returns the provider name telemetry, persisted model metadata, and
    /// provider errors carry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the model a client built from this specification serves, or
    /// `None` before [`Self::for_model`] names one.
    pub fn model(&self) -> Option<&str> {
        self.spec.model()
    }

    /// Sets the model a client built from this specification serves. A
    /// preset already serves the model of its configuration under a policy
    /// tailored to it, so configure a preset's model there instead.
    #[must_use]
    pub fn for_model(mut self, model: impl Into<String>) -> Self {
        self.spec = self.spec.for_model(model);

        self
    }

    /// Returns the reasoning family of the specification's models.
    pub fn reasoning_family(&self) -> &ReasoningFamily {
        &self.reasoning_family
    }

    /// Rejects a specification no request can be sent with.
    ///
    /// # Errors
    /// Returns [`ProviderSpecError::Invalid`] naming the first problem: a
    /// base URL that is not an absolute `http` or `https` URL without
    /// credentials, a query, or a fragment; a blank key; an invalid header or
    /// one the harness sets itself (`Authorization`, `Content-Type`,
    /// `Content-Length`, `Host`, or the [`Auth::Header`] name, which itself
    /// may name only `Authorization` among these); a blank effort name or
    /// overflow marker; or an option the protocol cannot express. The reason
    /// never quotes the base URL or a header value.
    pub fn validate(&self) -> Result<(), ProviderSpecError> {
        self.spec
            .validate()
            .map_err(|error| ProviderSpecError::Invalid {
                reason: error.to_string(),
            })
    }

    /// Sets how requests authenticate.
    #[must_use]
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.spec = self.spec.with_auth(auth.into_router());

        self
    }

    /// Sets the output-token bound sent when a request sets none (Anthropic
    /// Messages only, which requires a bound; other protocols then send
    /// none). It starts at 32,000 tokens.
    #[must_use]
    pub fn with_default_max_output_tokens(mut self, max_output_tokens: NonZeroU32) -> Self {
        self.spec = self.spec.with_default_max_output_tokens(max_output_tokens);

        self
    }

    /// Adds a header sent with every request, such as `OpenRouter`'s
    /// `HTTP-Referer`. Its value is sent as sensitive and never printed.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec = self.spec.with_header(name, value);

        self
    }

    /// Sets where turn instructions go (Chat Completions only; the
    /// Responses protocol accepts only [`InstructionPlacement::Separate`]).
    #[must_use]
    pub fn with_instruction_placement(mut self, placement: InstructionPlacement) -> Self {
        self.spec = self.spec.with_instruction_placement(placement.to_router());

        self
    }

    /// Sets whether the model reads image content.
    #[must_use]
    pub fn with_image_input(mut self, image_input: bool) -> Self {
        self.spec = self.spec.with_image_input(image_input);

        self
    }

    /// Sets the body field carrying the output-token bound, or `None` when
    /// the provider rejects one, which fails a bounded request before
    /// sending it.
    #[must_use]
    pub fn with_max_tokens_field(mut self, field: Option<MaxTokensField>) -> Self {
        let field = field.map(|field| match field {
            MaxTokensField::MaxCompletionTokens => ag_router::MaxTokensField::MaxCompletionTokens,
            MaxTokensField::MaxOutputTokens => ag_router::MaxTokensField::MaxOutputTokens,
            MaxTokensField::MaxTokens => ag_router::MaxTokensField::MaxTokens,
        });
        self.spec = self.spec.with_max_tokens_field(field);

        self
    }

    /// Adds a marker that, found case-insensitively in a `400` or `422`
    /// body or in the message of a failed Responses stream, classifies the
    /// failure as a context-window overflow beside the shared `OpenAI`
    /// markers. A blank marker fails validation.
    #[must_use]
    pub fn with_overflow_marker(mut self, marker: impl Into<String>) -> Self {
        self.spec = self.spec.with_overflow_marker(marker);

        self
    }

    /// Sets whether requests carry the session's prompt cache key; providers
    /// that reject unknown body fields must leave it off.
    #[must_use]
    pub fn with_prompt_cache_key(mut self, prompt_cache_key: bool) -> Self {
        self.spec = self.spec.with_prompt_cache_key(prompt_cache_key);

        self
    }

    /// Adds a query parameter sent with every request, such as Azure
    /// `OpenAI`'s `api-version`; a base URL carrying its own query fails
    /// validation. Its value never appears in errors.
    #[must_use]
    pub fn with_query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec = self.spec.with_query(name, value);

        self
    }

    /// Sets how a requested reasoning effort reaches the provider.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: ReasoningMapping) -> Self {
        self.spec = self.spec.with_reasoning(reasoning.into_router());

        self
    }

    /// Sets the reasoning family of the specification's models in place of
    /// the one [`Self::new`] derives from the provider name, which decides
    /// which recorded turns they replay with their provider reasoning
    /// intact; other turns replay it as short notes.
    #[must_use]
    pub fn with_reasoning_family(mut self, reasoning_family: ReasoningFamily) -> Self {
        self.reasoning_family = reasoning_family;

        self
    }

    /// Sets whether replayed assistant messages carry their plain-text
    /// reasoning as `reasoning_content`, as thinking-mode Kimi and Qwen
    /// models require on replayed tool calls (Chat Completions only).
    /// Reasoning from another reasoning family then replays as reasoning
    /// rather than as assistant text.
    #[must_use]
    pub fn with_reasoning_text_replay(mut self, reasoning_text: bool) -> Self {
        self.spec = self.spec.with_reasoning_text_replay(reasoning_text);

        self
    }

    /// Sets whether Chat Completions requests that advertise tools also
    /// carry the response format; many providers reject the combination.
    #[must_use]
    pub fn with_response_format_with_tools(mut self, response_format_with_tools: bool) -> Self {
        self.spec = self
            .spec
            .with_response_format_with_tools(response_format_with_tools);

        self
    }

    /// Sets how failed requests are retried; every specification, preset
    /// or not, starts from [`RetryPolicy::default`].
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.spec = self.spec.with_retry(retry.0);

        self
    }

    /// Sets how the provider is asked for structured output.
    #[must_use]
    pub fn with_structured_output(mut self, structured_output: StructuredOutputMode) -> Self {
        self.spec = self
            .spec
            .with_structured_output(structured_output.to_router());

        self
    }

    /// Sets whether tool-result messages name their function (Chat
    /// Completions only).
    #[must_use]
    pub fn with_tool_result_name(mut self, tool_result_name: bool) -> Self {
        self.spec = self.spec.with_tool_result_name(tool_result_name);

        self
    }

    /// Returns the provider name, reasoning family, and routed
    /// specification.
    pub(crate) fn into_parts(
        self,
    ) -> (Cow<'static, str>, ReasoningFamily, ag_router::ProviderSpec) {
        (self.name, self.reasoning_family, self.spec)
    }

    /// Wraps the routed preset `spec` of the built-in provider `name`, in
    /// that provider's reasoning family.
    fn preset(name: &'static str, spec: ag_router::ProviderSpec) -> Self {
        Self {
            name: Cow::Borrowed(name),
            reasoning_family: ReasoningFamily::for_provider(name),
            spec,
        }
    }
}

/// Routes a host [`CredentialSource`] through the router's credential
/// boundary.
struct RoutedCredential(Arc<dyn CredentialSource>);

#[async_trait]
impl ag_router::CredentialSource for RoutedCredential {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        self.0.bearer_token().await
    }
}

#[cfg(test)]
#[path = "spec_test.rs"]
mod tests;
