//! Data-driven provider specifications: the wire protocol, endpoint,
//! authentication, extra headers and query parameters, and request policy of
//! one provider, so a host adds an OpenAI-compatible, Responses, or Anthropic
//! Messages provider without code changes.

use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};

use crate::credential::CredentialSource;
use crate::input::TurnInput;
use crate::model::{GenerationOptions, ModelError, ModelMessage, ReasoningEffort};
use crate::retry::RetryPolicy;
use crate::schema::OutputSchema;

/// Smallest `max_output_tokens` the Responses API accepts.
const MIN_MAX_OUTPUT_TOKENS: u32 = 16;

/// `max_tokens` an Anthropic Messages request sends when neither the request
/// nor [`ProviderSpec::with_fallback_max_tokens`] sets a bound, since the API
/// requires one: Anthropic's guidance for a request without a stated bound.
pub(crate) const DEFAULT_FALLBACK_MAX_TOKENS: NonZeroU32 = NonZeroU32::MIN.saturating_add(15_999);

/// Header naming the Anthropic Messages API version.
const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";

/// The Anthropic Messages API version a specification requests unless a
/// header replaces it: the first stable version, which every Claude model
/// and Anthropic-compatible gateway accepts.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Headers the router sets itself, which an extra header must not repeat.
const RESERVED_HEADERS: [&str; 4] = ["authorization", "content-length", "content-type", "host"];

/// Wire API a provider implements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WireProtocol {
    /// The Anthropic Messages API (`POST {base_url}/messages`), streamed:
    /// Claude models with signed thinking replayed verbatim.
    AnthropicMessages,
    /// The `OpenAI` Chat Completions API (`POST {base_url}/chat/completions`),
    /// implemented by most OpenAI-compatible servers.
    ChatCompletions,
    /// The `OpenAI` Responses API (`POST {base_url}/responses`), streamed and
    /// stateless.
    Responses,
}

/// How requests authenticate with a provider. Keys are literal values the
/// host resolved, never environment variable names.
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
    /// before each request, such as a refreshing `ChatGPT` plan sign-in.
    Credential(Arc<dyn CredentialSource>),
}

impl fmt::Debug for Auth {
    /// Names the authentication kind without revealing any secret.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Bearer(_) => formatter.write_str("Bearer(<redacted>)"),
            Self::Header { name, .. } => write!(formatter, "Header({name}: <redacted>)"),
            Self::Credential(_) => formatter.write_str("Credential"),
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
    /// Under [`ReasoningMapping::None`], for a model that does not think by
    /// default, a request without tools forces that tool and one with tools
    /// requires some tool call. A model with a reasoning mapping may think,
    /// which forbids forcing a tool, so it chooses freely and a text answer
    /// is read too. For models without native structured output or with
    /// schemas native output rejects.
    ForcedTool,
    /// State the schema in a leading system message and request the
    /// `json_object` response format.
    JsonObject,
    /// State the schema in a leading system message, worded to invite tool
    /// calls first, and constrain requests without tools by the
    /// `json_schema` response format: the one structured-output form every
    /// common OpenAI-compatible server accepts.
    SchemaPrompt,
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

/// Options of a [`ReasoningMapping::Thinking`] control; the default keeps
/// thinking on for a low effort and sends nothing without an effort.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThinkingOptions {
    pub(crate) disable_supported: bool,
    pub(crate) preserve_reasoning: bool,
}

impl ThinkingOptions {
    /// Sets whether a low effort turns thinking off.
    #[must_use]
    pub fn with_disable_supported(mut self, disable_supported: bool) -> Self {
        self.disable_supported = disable_supported;

        self
    }

    /// Sets whether thinking stays on, and keeps all earlier reasoning,
    /// when no effort is requested.
    #[must_use]
    pub fn with_preserve_reasoning(mut self, preserve_reasoning: bool) -> Self {
        self.preserve_reasoning = preserve_reasoning;

        self
    }
}

/// Wire names of each [`ReasoningEffort`]; the default sends each effort
/// under its own name, and [`Self::with`] renames one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffortNames {
    names: [String; 5],
}

impl EffortNames {
    /// Sends `effort` under `name`.
    #[must_use]
    pub fn with(mut self, effort: ReasoningEffort, name: impl Into<String>) -> Self {
        self.names[index(effort)] = name.into();

        self
    }

    /// Returns the wire name of `effort`.
    pub(crate) fn name(&self, effort: ReasoningEffort) -> &str {
        &self.names[index(effort)]
    }
}

impl Default for EffortNames {
    fn default() -> Self {
        Self {
            names: [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
                ReasoningEffort::Max,
            ]
            .map(|effort| effort.as_str().to_string()),
        }
    }
}

/// Returns the position of `effort` in [`EffortNames`].
fn index(effort: ReasoningEffort) -> usize {
    match effort {
        ReasoningEffort::Low => 0,
        ReasoningEffort::Medium => 1,
        ReasoningEffort::High => 2,
        ReasoningEffort::XHigh => 3,
        ReasoningEffort::Max => 4,
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

/// Body field carrying [`crate::GenerationOptions::max_output_tokens`].
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

/// Wire protocol, endpoint, authentication, and request policy of one
/// provider, routed by its [`Self::id`] as the prefix of a `provider/model`
/// identifier.
///
/// [`Self::new`] starts from a conservative policy for the protocol, and the
/// `with_*` methods adjust it. The built-in providers are presets in
/// [`crate::preset`]; a preset whose policy depends on the model serves only
/// that model ([`Self::for_model`]). [`Self::validate`] rejects a
/// specification no request can be sent with, and every routed request runs
/// it before any network access.
///
/// The debug output names headers and query parameters but never prints
/// their values, a key, or credentials, a query, or a fragment of the base
/// URL.
#[derive(Clone)]
pub struct ProviderSpec {
    pub(crate) auth: Auth,
    pub(crate) base_url: String,
    pub(crate) cache_breakpoints: CacheBreakpoints,
    pub(crate) display_name: String,
    pub(crate) fallback_max_tokens: Option<NonZeroU32>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) id: String,
    pub(crate) image_input: bool,
    pub(crate) instruction_placement: InstructionPlacement,
    pub(crate) max_tokens_field: Option<MaxTokensField>,
    pub(crate) model: Option<String>,
    pub(crate) overflow_markers: Vec<String>,
    pub(crate) prompt_cache_key: bool,
    pub(crate) protocol: WireProtocol,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) reasoning: ReasoningMapping,
    pub(crate) replay: HistoryReplay,
    pub(crate) response_format_with_tools: bool,
    pub(crate) retry: RetryPolicy,
    pub(crate) stream_usage: StreamUsage,
    pub(crate) structured_output: StructuredOutputMode,
    pub(crate) thinking_binding: ThinkingBinding,
}

/// How an Anthropic Messages model treats replayed signed thinking whose
/// conversation changed since the model produced it.
///
/// Claude models from Claude Fable 5.1 on bind each thinking block to the
/// conversation before it. Where the check is enforced, a request replaying
/// a block after an earlier message changed fails with a `400` unless it
/// sets `thinking.block_binding.prefix_mismatch_behavior: "drop_block"`
/// under the `thinking-binding-controls-2026-08-01` beta. Setting the field
/// opts the request into enforcement, so an account the check does not
/// enforce, which keeps such blocks, would lose them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThinkingBinding {
    /// The model runs no conversation check, or is unknown.
    #[default]
    Unchecked,
    /// The check is enforced for some accounts only, such as those created
    /// on or after 2026-08-31: requests leave the field unset, and one
    /// rejected for an invalidated block is sent once more dropping it.
    OnRejection,
    /// The check is enforced for every account: every request that sends
    /// thinking drops invalidated blocks.
    Always,
}

/// Whether an Anthropic Messages request marks its stable prefix with
/// prompt-cache breakpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheBreakpoints {
    /// Send `cache_control` on the tools, `system`, and the last message.
    Marked,
    /// Send no `cache_control`.
    Unmarked,
}

/// Whether a streamed Chat Completions request asks for its usage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamUsage {
    /// Send `stream_options.include_usage`.
    Requested,
    /// Send no `stream_options`.
    Unrequested,
}

/// How Chat Completions history replays earlier turns.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HistoryReplay {
    /// Replay the assistant's plain-text reasoning as `reasoning_content`.
    pub(crate) reasoning_text: bool,
    /// Name the function on each tool-result message.
    pub(crate) tool_result_name: bool,
}

impl fmt::Debug for ProviderSpec {
    /// Prints the policy with header and query names only, since gateways
    /// carry keys in both, and the base URL without credentials, query, or
    /// fragment.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |pairs: &[(String, String)]| {
            pairs
                .iter()
                .map(|(name, _)| format!("{name}: <redacted>"))
                .collect::<Vec<_>>()
        };

        formatter
            .debug_struct("ProviderSpec")
            .field("id", &self.id)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("base_url", &redacted_base_url(&self.base_url))
            .field("auth", &self.auth)
            .field("headers", &redacted(&self.headers))
            .field("query", &redacted(&self.query))
            .field("display_name", &self.display_name)
            .field("structured_output", &self.structured_output)
            .field("reasoning", &self.reasoning)
            .field("max_tokens_field", &self.max_tokens_field)
            .field("fallback_max_tokens", &self.fallback_max_tokens)
            .field("image_input", &self.image_input)
            .field("instruction_placement", &self.instruction_placement)
            .field("prompt_cache_key", &self.prompt_cache_key)
            .field("cache_breakpoints", &self.cache_breakpoints)
            .field(
                "response_format_with_tools",
                &self.response_format_with_tools,
            )
            .field("replay", &self.replay)
            .field("overflow_markers", &self.overflow_markers)
            .field("retry", &self.retry)
            .field("stream_usage", &self.stream_usage)
            .field("thinking_binding", &self.thinking_binding)
            .finish()
    }
}

impl ProviderSpec {
    /// Creates an unauthenticated specification for every model under `id`,
    /// whose display name is `id`.
    ///
    /// Chat Completions starts from the standard `OpenAI` request with no
    /// optional capabilities: the schema stated in the prompt, no reasoning
    /// controls, prompt cache key, or image input, and the bound sent as
    /// `max_completion_tokens`, the current `OpenAI` field. A server that
    /// ignores that field leaves output unbounded, so it needs
    /// `with_max_tokens_field(Some(MaxTokensField::MaxTokens))`. Provider
    /// quirks are opt-ins: the legacy `max_tokens` field
    /// ([`Self::with_max_tokens_field`]), turn instructions joined to the
    /// preceding user message ([`Self::with_instruction_placement`]),
    /// reasoning-content replay, and tool-result names; the presets in
    /// [`crate::preset`] enable those their provider needs. The Responses API
    /// starts with native structured output, efforts under their own names,
    /// image input, `prompt_cache_key`, and `max_output_tokens`. Anthropic
    /// Messages starts with native structured output (`output_config.format`),
    /// efforts under their own names, image input, the required `max_tokens`
    /// field (16,000 when a request sets no bound, see
    /// [`Self::with_fallback_max_tokens`]), prompt-cache breakpoints on the
    /// stable prefix ([`Self::with_cache_breakpoints`]), and the
    /// `anthropic-version: 2023-06-01` header, which [`Self::with_header`]
    /// can replace; it needs
    /// `x-api-key` authentication, which the [`crate::preset::anthropic`]
    /// preset sets.
    pub fn new(id: impl Into<String>, protocol: WireProtocol, base_url: impl Into<String>) -> Self {
        let id = id.into();
        let anthropic = protocol == WireProtocol::AnthropicMessages;
        let chat = protocol == WireProtocol::ChatCompletions;
        let responses = protocol == WireProtocol::Responses;

        Self {
            auth: Auth::None,
            base_url: base_url.into(),
            cache_breakpoints: if anthropic {
                CacheBreakpoints::Marked
            } else {
                CacheBreakpoints::Unmarked
            },
            display_name: id.clone(),
            fallback_max_tokens: None,
            headers: if anthropic {
                vec![(
                    ANTHROPIC_VERSION_HEADER.to_string(),
                    ANTHROPIC_VERSION.to_string(),
                )]
            } else {
                Vec::new()
            },
            id,
            image_input: !chat,
            instruction_placement: InstructionPlacement::Separate,
            max_tokens_field: Some(match protocol {
                WireProtocol::AnthropicMessages => MaxTokensField::MaxTokens,
                WireProtocol::ChatCompletions => MaxTokensField::MaxCompletionTokens,
                WireProtocol::Responses => MaxTokensField::MaxOutputTokens,
            }),
            model: None,
            overflow_markers: Vec::new(),
            prompt_cache_key: responses,
            protocol,
            query: Vec::new(),
            reasoning: if chat {
                ReasoningMapping::None
            } else {
                ReasoningMapping::Effort(EffortNames::default())
            },
            replay: HistoryReplay::default(),
            response_format_with_tools: false,
            retry: RetryPolicy::default(),
            stream_usage: if chat {
                StreamUsage::Requested
            } else {
                StreamUsage::Unrequested
            },
            structured_output: if chat {
                StructuredOutputMode::SchemaPrompt
            } else {
                StructuredOutputMode::JsonSchema
            },
            thinking_binding: ThinkingBinding::Unchecked,
        }
    }

    /// Sets how the model treats replayed thinking whose conversation
    /// changed, as the Anthropic preset declares per model.
    #[must_use]
    pub(crate) fn with_thinking_binding(mut self, thinking_binding: ThinkingBinding) -> Self {
        self.thinking_binding = thinking_binding;

        self
    }

    /// Returns the routing prefix of this provider's model identifiers.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the one model this specification serves, or `None` when it
    /// serves every model under its identifier.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Restricts this specification to `model`, for a policy tailored to
    /// it: a router then routes only `{id}/{model}` here, and a router can
    /// hold one such specification per model of a provider.
    #[must_use]
    pub fn for_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());

        self
    }

    /// Sets whether replayed assistant messages carry their plain-text
    /// reasoning as `reasoning_content`, as thinking-mode models require on
    /// replayed tool calls, and whether that reasoning is kept from responses
    /// (Chat Completions only). See [`crate::Router::replays_reasoning_text`].
    #[must_use]
    pub fn with_reasoning_text_replay(mut self, reasoning_text: bool) -> Self {
        self.replay.reasoning_text = reasoning_text;

        self
    }

    /// Sets how requests authenticate.
    #[must_use]
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = auth;

        self
    }

    /// Sets the `max_tokens` an Anthropic Messages request sends when the
    /// request sets no output-token bound, in place of 16,000. The API
    /// requires a bound; requests stream, so a larger one is bounded only by
    /// the streaming transport's whole-response and idle timeouts. Other
    /// protocols send no bound then, so validation rejects this setting for
    /// them.
    #[must_use]
    pub fn with_fallback_max_tokens(mut self, max_tokens: NonZeroU32) -> Self {
        self.fallback_max_tokens = Some(max_tokens);

        self
    }

    /// Sets the name provider errors carry.
    #[must_use]
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = display_name.into();

        self
    }

    /// Adds a header sent with every request, such as `OpenRouter`'s
    /// `HTTP-Referer`, in place of any earlier header of the same name, read
    /// case-insensitively, such as the `anthropic-version` an Anthropic
    /// Messages specification starts with. Its value is sent as sensitive
    /// and never printed; a header the router sets itself, such as
    /// `Authorization` or `Content-Type`, fails validation.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
        self.headers.push((name, value.into()));

        self
    }

    /// Sets where turn instructions go (Chat Completions only; the
    /// Responses protocol accepts only [`InstructionPlacement::Separate`]).
    #[must_use]
    pub fn with_instruction_placement(mut self, placement: InstructionPlacement) -> Self {
        self.instruction_placement = placement;

        self
    }

    /// Sets whether the model reads image content.
    #[must_use]
    pub fn with_image_input(mut self, image_input: bool) -> Self {
        self.image_input = image_input;

        self
    }

    /// Sets the body field carrying the output-token bound, or `None` when
    /// the provider rejects one, which fails a bounded request before
    /// sending it.
    #[must_use]
    pub fn with_max_tokens_field(mut self, field: Option<MaxTokensField>) -> Self {
        self.max_tokens_field = field;

        self
    }

    /// Adds a marker that, found case-insensitively in a `400` or `422`
    /// body or in the message of a failed Responses stream, classifies the
    /// failure as a context-window overflow beside the shared `OpenAI`
    /// markers. A blank marker fails validation.
    #[must_use]
    pub fn with_overflow_marker(mut self, marker: impl Into<String>) -> Self {
        self.overflow_markers.push(marker.into().to_lowercase());

        self
    }

    /// Sets whether an Anthropic Messages request marks its stable prefix
    /// with `cache_control: {"type": "ephemeral"}` breakpoints, on by
    /// default: the last tool definition, the last `system` block, and the
    /// last block of the last message that is not signed thinking, three of
    /// the API's four breakpoints. A cache write costs more than uncached
    /// input, so a host whose requests share no prefix may turn it off;
    /// other protocols fail validation with it on.
    #[must_use]
    pub fn with_cache_breakpoints(mut self, cache_breakpoints: bool) -> Self {
        self.cache_breakpoints = if cache_breakpoints {
            CacheBreakpoints::Marked
        } else {
            CacheBreakpoints::Unmarked
        };

        self
    }

    /// Sets whether requests carry
    /// [`crate::GenerationOptions::prompt_cache_key`]; providers that reject
    /// unknown body fields must leave it off.
    #[must_use]
    pub fn with_prompt_cache_key(mut self, prompt_cache_key: bool) -> Self {
        self.prompt_cache_key = prompt_cache_key;

        self
    }

    /// Sets whether a streamed Chat Completions request asks for its usage
    /// with `stream_options.include_usage`, on by default. A server that
    /// rejects `stream_options` must turn it off, and then reports no
    /// usage unless it volunteers some. Only Chat Completions sends it:
    /// Responses and Anthropic Messages streams always report usage, so
    /// either protocol fails validation with it on.
    #[must_use]
    pub fn with_stream_usage(mut self, stream_usage: bool) -> Self {
        self.stream_usage = if stream_usage {
            StreamUsage::Requested
        } else {
            StreamUsage::Unrequested
        };

        self
    }

    /// Adds a query parameter sent with every request, such as Azure
    /// `OpenAI`'s `api-version`; a base URL carrying its own query fails
    /// validation.
    #[must_use]
    pub fn with_query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));

        self
    }

    /// Sets how a requested reasoning effort reaches the provider.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: ReasoningMapping) -> Self {
        self.reasoning = reasoning;

        self
    }

    /// Sets whether Chat Completions requests that advertise tools also
    /// carry the response format; many providers reject the combination.
    #[must_use]
    pub fn with_response_format_with_tools(mut self, response_format_with_tools: bool) -> Self {
        self.response_format_with_tools = response_format_with_tools;

        self
    }

    /// Sets how failed requests are retried; every specification starts
    /// from [`RetryPolicy::default`].
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;

        self
    }

    /// Sets how the provider is asked for structured output.
    #[must_use]
    pub fn with_structured_output(mut self, structured_output: StructuredOutputMode) -> Self {
        self.structured_output = structured_output;

        self
    }

    /// Sets whether tool-result messages name their function (Chat
    /// Completions only).
    #[must_use]
    pub fn with_tool_result_name(mut self, tool_result_name: bool) -> Self {
        self.replay.tool_result_name = tool_result_name;

        self
    }

    /// Rejects a specification no request can be sent with.
    ///
    /// # Errors
    /// Returns [`ModelError::InvalidProviderConfig`] naming the first
    /// problem: a blank identifier or one containing `/`, which no
    /// `provider/model` identifier routes to; a base URL that is not an
    /// absolute `http` or `https` URL without credentials, a query, or a
    /// fragment; a blank key; an invalid or reserved header, including an
    /// [`Auth::Header`] name the router sets itself other than
    /// `Authorization`; a blank effort name or overflow marker; or an option
    /// the protocol cannot express. No reason quotes the base URL or a
    /// header value, which may carry keys.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.id.trim().is_empty() || self.id.contains('/') {
            return Err(invalid("provider identifier must be nonblank without `/`"));
        }
        validate_base_url(&self.base_url)?;
        self.validate_auth()?;
        if let ReasoningMapping::Effort(names) = &self.reasoning
            && names.names.iter().any(|name| name.trim().is_empty())
        {
            return Err(invalid("reasoning effort names must not be blank"));
        }
        let auth_header = match &self.auth {
            Auth::Header { name, .. } => HeaderName::from_bytes(name.as_bytes()).ok(),
            Auth::None | Auth::Bearer(_) | Auth::Credential(_) => None,
        };
        for (name, value) in &self.headers {
            let (parsed, _) = header(name, value)
                .ok_or_else(|| invalid(format!("header `{name}` is invalid")))?;
            if RESERVED_HEADERS.contains(&parsed.as_str()) || auth_header.as_ref() == Some(&parsed)
            {
                return Err(invalid(format!(
                    "header `{name}` is set by the router or the authentication"
                )));
            }
        }
        if self
            .overflow_markers
            .iter()
            .any(|marker| marker.trim().is_empty())
        {
            return Err(invalid("overflow markers must not be blank"));
        }

        self.validate_protocol()
    }

    /// Returns the request endpoint for `path` under the base URL, with the
    /// configured query parameters.
    pub(crate) fn endpoint(&self, path: &str) -> String {
        let mut endpoint = format!("{}/{path}", self.base_url.trim_end_matches('/'));
        if !self.query.is_empty() {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(&self.query)
                .finish();
            endpoint.push('?');
            endpoint.push_str(&query);
        }

        endpoint
    }

    /// Returns whether the output schema is stated in a leading prompt
    /// message rather than constrained natively.
    pub(crate) fn keeps_prefix_across_schemas(&self) -> bool {
        self.protocol == WireProtocol::ChatCompletions
            && matches!(
                self.structured_output,
                StructuredOutputMode::JsonObject | StructuredOutputMode::SchemaPrompt
            )
    }

    /// Requires an object root, the only root every structured-output mode
    /// accepts.
    pub(crate) fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        if !schema.has_object_root() {
            return Err(ModelError::UnsupportedOutputSchema {
                reason: format!(
                    "{} structured output requires an explicit object root schema",
                    self.display_name
                ),
            });
        }

        Ok(())
    }

    /// Rejects image content unless the specification accepts it.
    pub(crate) fn validate_input(&self, model: &str, input: &TurnInput) -> Result<(), ModelError> {
        if input.has_images() && !self.image_input {
            return Err(ModelError::UnsupportedImageInput {
                reason: format!(
                    "{} configuration `{model}` does not accept image content",
                    self.display_name
                ),
            });
        }

        Ok(())
    }

    /// Validates every user input among `messages` as
    /// [`Self::validate_input`] does.
    pub(crate) fn validate_inputs(
        &self,
        model: &str,
        messages: &[ModelMessage],
    ) -> Result<(), ModelError> {
        messages
            .iter()
            .filter_map(|message| match message {
                ModelMessage::UserInput(input) => Some(input),
                _ => None,
            })
            .try_for_each(|input| self.validate_input(model, input))
    }

    /// Rejects an output-token bound the provider does not accept: any bound
    /// without a [`MaxTokensField`], and one below
    /// [`MIN_MAX_OUTPUT_TOKENS`] in `max_output_tokens`, which the Responses
    /// API answers with a `400`.
    pub(crate) fn validate_generation(
        &self,
        options: &GenerationOptions,
    ) -> Result<(), ModelError> {
        let Some(max_output_tokens) = options.max_output_tokens else {
            return Ok(());
        };
        let reason = match self.max_tokens_field {
            None => format!(
                "{} does not accept an output-token bound",
                self.display_name
            ),
            Some(MaxTokensField::MaxOutputTokens)
                if max_output_tokens.get() < MIN_MAX_OUTPUT_TOKENS =>
            {
                format!(
                    "{} requires an output-token bound of at least {MIN_MAX_OUTPUT_TOKENS}",
                    self.display_name
                )
            }
            Some(_) => return Ok(()),
        };

        Err(ModelError::UnsupportedGeneration { reason })
    }

    /// Builds the headers of one request, authenticated by `bearer` when
    /// the specification uses a [`Auth::Credential`]. Every authentication
    /// and extra header value is marked sensitive.
    ///
    /// # Errors
    /// Returns [`ModelError::InvalidProviderConfig`] for a token that cannot
    /// be sent as a header.
    pub(crate) fn request_headers(&self, bearer: Option<&str>) -> Result<HeaderMap, ModelError> {
        let token = match &self.auth {
            Auth::Bearer(key) => Some(key.as_str()),
            Auth::Credential(_) => bearer,
            Auth::None | Auth::Header { .. } => None,
        };
        let authorization = match &self.auth {
            Auth::Header { name, value } => Some((name.as_str(), value.clone())),
            Auth::None | Auth::Bearer(_) | Auth::Credential(_) => {
                token.map(|token| (AUTHORIZATION.as_str(), format!("Bearer {token}")))
            }
        };
        let mut headers = HeaderMap::new();
        let pairs = authorization
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .chain(
                self.headers
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
        for (name, value) in pairs {
            let (name, mut value) = header(name, value)
                .ok_or_else(|| invalid("authentication token cannot be sent as a header"))?;
            value.set_sensitive(true);
            headers.append(name, value);
        }

        Ok(headers)
    }

    /// Rejects a blank key, or an invalid authentication header or one
    /// naming a header the router sets itself other than `Authorization`.
    fn validate_auth(&self) -> Result<(), ModelError> {
        match &self.auth {
            Auth::Bearer(key) if key.trim().is_empty() => Err(invalid("bearer key is blank")),
            Auth::Header { name, value } => Self::validate_auth_header(name, value),
            Auth::None | Auth::Bearer(_) | Auth::Credential(_) => Ok(()),
        }
    }

    /// Rejects a blank, invalid, or router-owned authentication header.
    fn validate_auth_header(name: &str, value: &str) -> Result<(), ModelError> {
        if value.trim().is_empty() {
            return Err(invalid("authentication header value is blank"));
        }
        let (parsed, _) = header(name, value)
            .ok_or_else(|| invalid(format!("authentication header `{name}` is invalid")))?;
        if parsed != AUTHORIZATION && RESERVED_HEADERS.contains(&parsed.as_str()) {
            return Err(invalid(format!(
                "authentication header `{name}` is set by the router"
            )));
        }

        Ok(())
    }

    /// Rejects an option the protocol cannot express.
    fn validate_protocol(&self) -> Result<(), ModelError> {
        let unsupported = match self.protocol {
            WireProtocol::AnthropicMessages => self.unsupported_by_anthropic(),
            _ if self.fallback_max_tokens.is_some() => Some("a fallback `max_tokens`"),
            _ if self.cache_breakpoints == CacheBreakpoints::Marked => {
                Some("`cache_control` breakpoints")
            }
            WireProtocol::ChatCompletions => {
                if self.max_tokens_field == Some(MaxTokensField::MaxOutputTokens) {
                    Some("the `max_output_tokens` field")
                } else if self.structured_output == StructuredOutputMode::ForcedTool {
                    Some("forced-tool structured output")
                } else {
                    None
                }
            }
            WireProtocol::Responses => {
                if self.structured_output != StructuredOutputMode::JsonSchema {
                    Some("a structured-output mode other than JSON Schema")
                } else if !matches!(
                    self.reasoning,
                    ReasoningMapping::None | ReasoningMapping::Effort(_)
                ) {
                    Some("a reasoning mapping other than effort names")
                } else if !matches!(
                    self.max_tokens_field,
                    None | Some(MaxTokensField::MaxOutputTokens)
                ) {
                    Some("an output-token field other than `max_output_tokens`")
                } else if self.instruction_placement != InstructionPlacement::Separate {
                    Some("instructions placed other than as their own message")
                } else if self.replay.reasoning_text || self.replay.tool_result_name {
                    Some("reasoning-content replay or tool-result names")
                } else if self.stream_usage == StreamUsage::Requested {
                    Some("`stream_options.include_usage`")
                } else {
                    None
                }
            }
        };

        match unsupported {
            Some(option) => Err(invalid(format!(
                "the {:?} protocol cannot express {option}",
                self.protocol
            ))),
            None => Ok(()),
        }
    }

    /// Names the first option the Anthropic Messages protocol cannot
    /// express, which requires the `max_tokens` field and has no prompt
    /// cache key.
    fn unsupported_by_anthropic(&self) -> Option<&'static str> {
        if !matches!(
            self.structured_output,
            StructuredOutputMode::JsonSchema | StructuredOutputMode::ForcedTool
        ) {
            Some("a structured-output mode other than JSON Schema or a forced tool")
        } else if !matches!(
            self.reasoning,
            ReasoningMapping::None | ReasoningMapping::Effort(_)
        ) {
            Some("a reasoning mapping other than effort names")
        } else if self.max_tokens_field != Some(MaxTokensField::MaxTokens) {
            Some("an output-token field other than `max_tokens`")
        } else if self.instruction_placement != InstructionPlacement::Separate {
            Some("instructions placed other than as their own content")
        } else if self.replay.reasoning_text || self.replay.tool_result_name {
            Some("reasoning-content replay or tool-result names")
        } else if self.prompt_cache_key {
            Some("a prompt cache key")
        } else if self.stream_usage == StreamUsage::Requested {
            Some("`stream_options.include_usage`")
        } else {
            None
        }
    }
}

/// Requires an absolute `http` or `https` URL without credentials, a query,
/// or a fragment, since request paths and query parameters are appended to
/// it and keys belong in [`Auth`]. The reason never quotes the URL, which a
/// rejected one may carry a key in.
fn validate_base_url(base_url: &str) -> Result<(), ModelError> {
    let valid = url::Url::parse(base_url).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if !valid {
        return Err(invalid(
            "base URL must be an absolute http(s) URL without credentials, query, or fragment",
        ));
    }

    Ok(())
}

/// Returns `base_url` without credentials, query, or fragment, which may
/// carry keys, or `<invalid>` when it does not parse.
fn redacted_base_url(base_url: &str) -> String {
    let Ok(mut url) = url::Url::parse(base_url) else {
        return "<invalid>".to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);

    url.to_string()
}

/// Returns an [`ModelError::InvalidProviderConfig`] for `reason`.
fn invalid(reason: impl Into<String>) -> ModelError {
    ModelError::InvalidProviderConfig {
        reason: reason.into(),
    }
}

/// Parses one header, or returns `None` for an invalid name or value.
fn header(name: &str, value: &str) -> Option<(HeaderName, HeaderValue)> {
    Some((
        HeaderName::from_bytes(name.as_bytes()).ok()?,
        HeaderValue::from_str(value).ok()?,
    ))
}

#[cfg(test)]
#[path = "provider_spec_test.rs"]
mod tests;
