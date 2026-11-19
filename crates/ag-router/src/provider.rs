//! Routing of `provider/model` requests to host-supplied provider
//! specifications.

use std::collections::HashMap;
use std::sync::Arc;

use crate::anthropic::{self, AnthropicBackend};
use crate::chat_completion::ChatCompletionBackend;
use crate::http::HttpTransport;
use crate::model::{
    Completion, GeneratedResponse, GenerationOptions, ModelError, ModelErrorType, ModelRequest,
};
use crate::provider_spec::{Auth, ProviderSpec, WireProtocol};
use crate::responses::ResponsesBackend;
use crate::retry::RetryObserver;
use crate::stream::{self, DeltaObserver};
use crate::{OutputSchema, TurnInput, ensure_content_size};

/// Routing key of a specification: its identifier and the one model it
/// serves, if any.
type SpecKey = (String, Option<String>);

/// Single entry point for configured model providers.
pub struct Router {
    anthropic: Arc<HttpTransport>,
    specs: HashMap<SpecKey, ProviderSpec>,
    /// Transport of the streamed protocols, Responses and Chat Completions.
    streaming: Arc<HttpTransport>,
}

/// Observers of one request's retries and streamed deltas.
struct Observers {
    deltas: Option<Arc<dyn DeltaObserver>>,
    retries: Option<Arc<dyn RetryObserver>>,
}

impl Router {
    /// Creates a router from host-supplied provider specifications.
    ///
    /// A request for `provider/model` goes to the specification with
    /// [`ProviderSpec::id`] `provider` restricted to `model`
    /// ([`ProviderSpec::for_model`]), or else to the one serving every model
    /// of `provider`. Of duplicates, specifications with the same identifier
    /// and model, the first one stays in every build, so a later one cannot
    /// change the wire policy; replace a specification with
    /// [`Self::with_override`]. Each request validates its specification
    /// ([`ProviderSpec::validate`]) before any network access.
    pub fn new(specs: impl IntoIterator<Item = ProviderSpec>) -> Self {
        let mut router = Self {
            anthropic: anthropic::default_transport(),
            specs: HashMap::new(),
            streaming: stream::default_transport(),
        };
        for spec in specs {
            router
                .specs
                .entry((spec.id.clone(), spec.model.clone()))
                .or_insert(spec);
        }

        router
    }

    /// Returns this router with `spec` in place of any specification with
    /// the same identifier and model, such as a host's adjusted copy of a
    /// preset.
    #[must_use]
    pub fn with_override(mut self, spec: ProviderSpec) -> Self {
        self.specs
            .insert((spec.id.clone(), spec.model.clone()), spec);

        self
    }

    /// Validates whether the selected model can represent the output schema.
    ///
    /// # Errors
    /// Returns a model or schema capability error.
    pub fn validate_schema(&self, model: &str, schema: &OutputSchema) -> Result<(), ModelError> {
        self.route(model)?.0.validate_schema(schema)
    }

    /// Validates whether the selected model accepts the input content.
    ///
    /// # Errors
    /// Returns a model or input capability error.
    pub fn validate_input(&self, model: &str, input: &TurnInput) -> Result<(), ModelError> {
        let (spec, model) = self.route(model)?;

        spec.validate_input(model, input)
    }

    /// Validates whether the selected model accepts the generation
    /// settings, such as the output-token bound, before any request.
    ///
    /// # Errors
    /// Returns a routing error, or [`ModelError::UnsupportedGeneration`]
    /// for settings the provider rejects.
    pub fn validate_generation(
        &self,
        model: &str,
        options: &GenerationOptions,
    ) -> Result<(), ModelError> {
        self.route(model)?.0.validate_generation(options)
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
        Ok(self.route(model)?.0.keeps_prefix_across_schemas())
    }

    /// Returns whether the selected model's replayed assistant messages
    /// carry plain-text reasoning natively, as thinking-mode Kimi and Qwen
    /// models require on replayed tool calls (see
    /// [`ProviderSpec::with_reasoning_text_replay`]). Other models drop such
    /// text.
    ///
    /// # Errors
    /// Returns a routing error for an unknown or unconfigured model.
    pub fn replays_reasoning_text(&self, model: &str) -> Result<bool, ModelError> {
        Ok(self.route(model)?.0.replay.reasoning_text)
    }

    /// Returns whether the selected model needs earlier turns replayed with
    /// their turn instructions, exactly as first sent: Anthropic Messages
    /// models drop or reject signed thinking that follows an edited earlier
    /// message, so their history must only grow. Other models replay a
    /// turn's instructions with that turn only.
    ///
    /// # Errors
    /// Returns a routing error for an unknown or unconfigured model.
    pub fn replays_turn_instructions(&self, model: &str) -> Result<bool, ModelError> {
        Ok(self.route(model)?.0.protocol == WireProtocol::AnthropicMessages)
    }

    /// Executes one request and validates terminal JSON against its schema;
    /// a request without a schema returns bounded, nonblank terminal text.
    ///
    /// # Errors
    /// Returns a routing, transport, decoding, or output validation error;
    /// a blank text answer fails as [`ModelError::InvalidResponse`].
    pub async fn execute(&self, request: ModelRequest) -> Result<Completion, ModelError> {
        self.execute_routed(
            request,
            Observers {
                deltas: None,
                retries: None,
            },
        )
        .await
    }

    /// Executes one request as [`Self::execute`] does, reporting each retry
    /// its provider's [`crate::RetryPolicy`] sends to `observer` before
    /// waiting for it.
    ///
    /// # Errors
    /// Returns the errors of [`Self::execute`].
    pub async fn execute_observed(
        &self,
        request: ModelRequest,
        observer: Arc<dyn RetryObserver>,
    ) -> Result<Completion, ModelError> {
        self.execute_routed(
            request,
            Observers {
                deltas: None,
                retries: Some(observer),
            },
        )
        .await
    }

    /// Executes one request as [`Self::execute`] does, reporting each
    /// [`crate::ResponseDelta`] of a streamed response to `deltas` as it
    /// arrives, and each retry to `retries` as [`Self::execute_observed`]
    /// does.
    ///
    /// Responses and Chat Completions requests stream; an Anthropic
    /// Messages request reports no deltas. The returned completion is the
    /// one [`Self::execute`] returns for the same response, so deltas only
    /// preview it: a response that fails after streaming began, or whose
    /// output fails validation, returns its error after its deltas.
    ///
    /// # Errors
    /// Returns the errors of [`Self::execute`].
    pub async fn execute_streamed(
        &self,
        request: ModelRequest,
        deltas: Arc<dyn DeltaObserver>,
        retries: Option<Arc<dyn RetryObserver>>,
    ) -> Result<Completion, ModelError> {
        self.execute_routed(
            request,
            Observers {
                deltas: Some(deltas),
                retries,
            },
        )
        .await
    }

    /// Executes one request, reporting to `observers`.
    async fn execute_routed(
        &self,
        request: ModelRequest,
        observers: Observers,
    ) -> Result<Completion, ModelError> {
        let (spec, model) = self.route(&request.model)?;
        if let Some(schema) = request.schema() {
            spec.validate_schema(schema)?;
        }
        spec.validate_generation(&request.options)?;
        let bearer = match &spec.auth {
            Auth::Credential(credential) => {
                Some(credential.bearer_token().await.map_err(|error| {
                    ModelError::classified_request(ModelErrorType::Provider, None, error)
                })?)
            }
            Auth::None | Auth::Bearer(_) | Auth::Header { .. } => None,
        };
        let headers = spec.request_headers(bearer.as_deref())?;
        let response = match spec.protocol {
            WireProtocol::AnthropicMessages => {
                AnthropicBackend::new(
                    spec,
                    model.to_string(),
                    headers,
                    Arc::clone(&self.anthropic),
                    observers.retries,
                )
                .generate(&request)
                .await?
            }
            WireProtocol::ChatCompletions => {
                ChatCompletionBackend::new(
                    spec,
                    model.to_string(),
                    headers,
                    Arc::clone(&self.streaming),
                    observers.retries,
                    observers.deltas,
                )
                .generate(&request)
                .await?
            }
            WireProtocol::Responses => {
                ResponsesBackend::new(
                    spec,
                    model.to_string(),
                    headers,
                    Arc::clone(&self.streaming),
                    observers.retries,
                    observers.deltas,
                )
                .generate(&request)
                .await?
            }
        };
        match response {
            GeneratedResponse::Output {
                metadata,
                output,
                reasoning_content,
            } => {
                let invalid = |error| ModelError::from(error).with_metadata(metadata.clone());
                let Some(schema) = request.schema() else {
                    // An empty answer, such as a thinking model spending its
                    // whole output on reasoning, is no answer.
                    if output.trim().is_empty() {
                        return Err(ModelError::InvalidResponse.with_metadata(metadata.clone()));
                    }
                    ensure_content_size(&output).map_err(invalid)?;

                    return Ok(Completion::Text {
                        text: output,
                        metadata,
                        reasoning_content,
                    });
                };

                Ok(Completion::Output {
                    value: schema.parse_and_validate(&output).map_err(invalid)?,
                    metadata,
                    reasoning_content,
                })
            }
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

    /// Resolves a `provider/model` identifier to its configured, valid
    /// provider specification: the one restricted to `model`, or else the
    /// one serving every model of the provider.
    fn route<'id>(&self, id: &'id str) -> Result<(&ProviderSpec, &'id str), ModelError> {
        let (prefix, model) = id.split_once('/').ok_or(ModelError::InvalidModelId)?;
        if model.trim().is_empty() || prefix.trim().is_empty() {
            return Err(ModelError::InvalidModelId);
        }
        let spec = self
            .specs
            .get(&(prefix.to_string(), Some(model.to_string())))
            .or_else(|| self.specs.get(&(prefix.to_string(), None)))
            .ok_or(ModelError::NoProviderSpec)?;
        spec.validate()?;

        Ok((spec, model))
    }
}

#[cfg(test)]
#[path = "provider_test.rs"]
mod tests;
