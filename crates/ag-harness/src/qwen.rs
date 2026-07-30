use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use opentelemetry::global;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{Instrument, field};

use crate::{model, schema_contract, telemetry};

const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const GEN_AI_OPERATION: &str = "chat";
const JSON_STRING_MAX_EXPANSION: usize = 6;
const MAX_RETRY_DELAY: Duration = Duration::from_mins(1);
const MAX_HTTP_RESENDS: usize = 2;
const PROVIDER_NAME: &str = "alibaba_cloud";
const REQUEST_CANCELLED_ERROR_TYPE: &str = "cancelled";
const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const RETRY_DELAYS: [Duration; MAX_HTTP_RESENDS] =
    [Duration::from_millis(100), Duration::from_millis(250)];
const SUCCESS_BODY_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_ENVELOPE_LIMIT_BYTES;
const STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Return only one JSON object. The object must validate against this JSON Schema. ",
    "Do not include Markdown fences or any other text.\n\nJSON Schema:\n",
);
const UNSUPPORTED_SCHEMA_REASON: &str =
    "Qwen JSON Object mode requires an explicit object root schema";

/// Configuration for a Qwen model served through Alibaba Cloud Model Studio's
/// OpenAI-compatible API.
pub struct QwenConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Qwen model identifier sent with each request.
    pub model: String,
}

/// Qwen implementation of the provider-neutral [`model::Model`] contract.
///
/// Calls emit `GenAI` spans and metrics without prompt or response content.
/// Connection failures and timeouts, including interrupted response bodies,
/// plus rate limits and server errors are retried twice with bounded jitter.
/// Rate limits honor a bounded `Retry-After` value. Every HTTP attempt is
/// represented by its own child span. Cancelling an in-flight call records its
/// error span, failure log, and duration before exporter shutdown.
pub struct Qwen {
    client: reqwest::Client,
    config: QwenConfig,
    metrics: OnceLock<telemetry::RequestMetrics>,
}

impl Qwen {
    /// Creates a Qwen model adapter whose metric instruments initialize on the
    /// first request, after the application has installed its meter provider.
    ///
    /// # Errors
    ///
    /// Returns [`model::ModelError`] when the HTTP client cannot be
    /// initialized.
    pub fn new(config: QwenConfig) -> Result<Self, model::ModelError> {
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .build()
            .map_err(model::ModelError::request)?;

        Ok(Self {
            client,
            config,
            metrics: OnceLock::new(),
        })
    }

    /// Creates the root span that owns all telemetry for one model call.
    fn call_span(&self) -> tracing::Span {
        tracing::info_span!(
            target: "ag_harness::qwen",
            "gen_ai.client.operation",
            "otel.name" = format_args!("{GEN_AI_OPERATION} {}", self.config.model),
            "otel.kind" = "client",
            "otel.status_code" = field::Empty,
            "error.type" = field::Empty,
            "gen_ai.conversation.id" = field::Empty,
            "gen_ai.operation.name" = GEN_AI_OPERATION,
            "gen_ai.output.type" = "json",
            "gen_ai.provider.name" = PROVIDER_NAME,
            "gen_ai.request.model" = %self.config.model,
            "gen_ai.response.id" = field::Empty,
            "gen_ai.response.model" = field::Empty,
            "gen_ai.usage.input_tokens" = field::Empty,
            "gen_ai.usage.output_tokens" = field::Empty,
        )
    }

    /// Compiles, sends, parses, and validates one provider request inside the
    /// model-call span.
    async fn complete_call(
        &self,
        request: &model::ModelRequest,
    ) -> Result<QwenEnvelope, model::ModelError> {
        let compile_span = tracing::info_span!(
            target: "ag_harness::qwen",
            "gen_ai.schema.compile",
            "otel.kind" = "internal",
            "otel.status_code" = field::Empty,
            "error.type" = field::Empty,
        );
        let payload = compile_span.in_scope(|| self.compile_request(request));
        if let Err(error) = &payload {
            mark_span_error(&compile_span, error_type(error));
        }
        let payload = payload?;

        let mut resend_count = 0_usize;
        let body = loop {
            let result = self
                .send_request(&payload)
                .instrument(Self::http_attempt_span(resend_count))
                .await;

            match result {
                Ok(body) => break body,
                Err(error) if error.is_retryable && resend_count < MAX_HTTP_RESENDS => {
                    let next_resend_count = resend_count + 1;
                    let retry_delay = Self::jittered_retry_delay(
                        error.retry_after.unwrap_or(RETRY_DELAYS[resend_count]),
                    );
                    tracing::warn!(
                        target: "ag_harness::qwen",
                        resend_count = next_resend_count,
                        "Retrying GenAI HTTP request"
                    );
                    tokio::time::sleep(retry_delay).await;
                    resend_count += 1;
                }
                Err(error) => return Err(error.error),
            }
        };

        let parse_span = tracing::info_span!(
            target: "ag_harness::qwen",
            "gen_ai.response.parse_validate",
            "otel.kind" = "internal",
            "otel.status_code" = field::Empty,
            "error.type" = field::Empty,
        );
        let envelope = parse_span.in_scope(|| Self::parse_response(request, &body));
        let error = match &envelope {
            Ok(envelope) => envelope.result.as_ref().err(),
            Err(error) => Some(error),
        };
        if let Some(error) = error {
            mark_span_error(&parse_span, error_type(error));
        }

        envelope
    }

    /// Creates one child span for an individual HTTP send or resend.
    fn http_attempt_span(resend_count: usize) -> tracing::Span {
        let resend_count = i64::try_from(resend_count).unwrap_or(i64::MAX);

        tracing::info_span!(
            target: "ag_harness::qwen",
            "HTTP POST",
            "otel.kind" = "client",
            "otel.status_code" = field::Empty,
            "http.request.method" = "POST",
            "http.request.resend_count" = resend_count,
            "http.response.status_code" = field::Empty,
        )
    }

    /// Converts the provider-neutral request into Qwen's JSON Object payload.
    fn compile_request<'a>(
        &'a self,
        request: &model::ModelRequest,
    ) -> Result<QwenRequest<'a>, model::ModelError> {
        if !request.schema().has_object_root() {
            return Err(model::ModelError::UnsupportedOutputSchema {
                reason: UNSUPPORTED_SCHEMA_REASON.to_string(),
            });
        }

        let messages = vec![
            QwenMessage {
                content: format!(
                    "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                    request.schema().value()
                ),
                role: "system",
            },
            QwenMessage {
                content: request.prompt().to_string(),
                role: "user",
            },
        ];

        Ok(QwenRequest {
            messages,
            model: &self.config.model,
            response_format: QwenResponseFormat {
                kind: "json_object",
            },
        })
    }

    /// Sends one HTTP attempt and classifies failures for the outer retry loop.
    async fn send_request(&self, payload: &QwenRequest<'_>) -> Result<Vec<u8>, QwenAttemptError> {
        let mut response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.config.api_key)
            .timeout(REQUEST_TIMEOUT)
            .json(payload)
            .send()
            .await
            .map_err(QwenAttemptError::transport)?;
        let status = response.status();
        tracing::Span::current().record("http.response.status_code", i64::from(status.as_u16()));

        if let Err(source) = response.error_for_status_ref() {
            tracing::Span::current().record("otel.status_code", "ERROR");
            let is_retryable = Self::is_retryable_status(status);
            let retry_after = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                Self::retry_after_delay(&response, SystemTime::now())
            } else {
                None
            };
            let body = Self::error_body_summary(&mut response)
                .await
                .map_err(|error| QwenAttemptError::error_body(error, is_retryable, retry_after))?;
            let error = QwenHttpError {
                body,
                source,
                status,
            };

            return Err(QwenAttemptError {
                error: model::ModelError::request(error),
                is_retryable,
                retry_after,
            });
        }

        Self::success_body(&mut response).await
    }

    /// Returns whether an HTTP status is eligible for a bounded resend.
    fn is_retryable_status(status: reqwest::StatusCode) -> bool {
        status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
    }

    /// Reads a valid `Retry-After` delta or HTTP date and caps the requested
    /// delay so provider input cannot suspend the harness indefinitely.
    fn retry_after_delay(response: &reqwest::Response, now: SystemTime) -> Option<Duration> {
        let value = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)?
            .to_str()
            .ok()?;

        Self::parse_retry_after(value, now)
    }

    /// Parses either form allowed by the `Retry-After` header.
    fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
        let delay = match value.parse::<u64>() {
            Ok(seconds) => Duration::from_secs(seconds),
            Err(_) => httpdate::parse_http_date(value)
                .ok()?
                .duration_since(now)
                .unwrap_or_default(),
        };

        Some(delay.min(MAX_RETRY_DELAY))
    }

    /// Adds bounded randomized jitter to a fallback or provider-requested
    /// retry delay, preventing concurrent calls from retrying in lockstep.
    fn jittered_retry_delay(base: Duration) -> Duration {
        let base = base.min(MAX_RETRY_DELAY);
        let max_jitter = (base / 4).min(Duration::from_millis(250));
        if max_jitter.is_zero() {
            return base;
        }

        let max_jitter_nanos = u64::try_from(max_jitter.as_nanos()).unwrap_or(u64::MAX);
        let jitter_nanos = RandomState::new().hash_one(()) % max_jitter_nanos.saturating_add(1);

        base.saturating_add(Duration::from_nanos(jitter_nanos))
            .min(MAX_RETRY_DELAY)
    }

    /// Decodes the provider envelope while retaining response and usage
    /// metadata even when output validation fails.
    fn parse_response(
        request: &model::ModelRequest,
        body: &[u8],
    ) -> Result<QwenEnvelope, model::ModelError> {
        let response =
            serde_json::from_slice::<QwenResponse>(body).map_err(model::ModelError::request)?;
        let result = Self::parse_choice(request, response.choices);

        Ok(QwenEnvelope {
            response_id: response.id,
            response_model: response.model,
            result,
            usage: response.usage,
        })
    }

    /// Converts the first completed provider choice into validated JSON.
    fn parse_choice(
        request: &model::ModelRequest,
        choices: Vec<QwenChoice>,
    ) -> Result<model::ModelResponse, model::ModelError> {
        let choice = choices
            .into_iter()
            .next()
            .ok_or(model::ModelError::InvalidResponse)?;
        if choice.finish_reason != "stop" {
            return Err(model::ModelError::IncompleteResponse {
                reason: schema_contract::bounded_diagnostic(choice.finish_reason),
            });
        }

        let text = choice
            .message
            .content
            .ok_or(model::ModelError::InvalidResponse)?;
        let output = request.schema().parse_and_validate(&text)?;

        Ok(model::ModelResponse::new(output))
    }

    /// Returns the configured Chat Completions endpoint.
    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// Reads a bounded, whitespace-normalized provider error summary.
    async fn error_body_summary(
        response: &mut reqwest::Response,
    ) -> Result<String, reqwest::Error> {
        let mut body = Vec::new();
        let mut is_truncated = false;

        while let Some(chunk) = response.chunk().await? {
            let remaining = ERROR_BODY_LIMIT_BYTES.saturating_sub(body.len());

            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                is_truncated = true;

                break;
            }

            body.extend_from_slice(&chunk);
        }

        let mut summary = String::from_utf8_lossy(&body)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if is_truncated {
            summary.push_str(" ...");
        }

        Ok(summary)
    }

    /// Reads a successful response body without exceeding the envelope limit.
    async fn success_body(response: &mut reqwest::Response) -> Result<Vec<u8>, QwenAttemptError> {
        let limit = u64::try_from(SUCCESS_BODY_LIMIT_BYTES).unwrap_or(u64::MAX);
        if response
            .content_length()
            .is_some_and(|content_length| content_length > limit)
        {
            return Err(QwenAttemptError::not_retryable(
                model::ModelError::ResponseBodyTooLarge,
            ));
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(QwenAttemptError::success_body)?
        {
            Self::append_success_chunk(&mut body, &chunk)
                .map_err(QwenAttemptError::not_retryable)?;
        }

        Ok(body)
    }

    /// Appends one response chunk when it fits within the remaining limit.
    fn append_success_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), model::ModelError> {
        let remaining = SUCCESS_BODY_LIMIT_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            return Err(model::ModelError::ResponseBodyTooLarge);
        }

        body.extend_from_slice(chunk);

        Ok(())
    }
}

#[async_trait]
impl model::Model for Qwen {
    async fn complete(
        &self,
        request: model::ModelRequest,
    ) -> Result<model::ModelResponse, model::ModelError> {
        let span = self.call_span();
        if let Some(session_id) = request.session_id() {
            span.record("gen_ai.conversation.id", session_id);
        }
        let metrics = self
            .metrics
            .get_or_init(|| telemetry::RequestMetrics::new(&global::meter("ag-harness")));
        let mut call_telemetry = CallTelemetry::new(metrics, &self.config.model, span.clone());

        let result = async {
            tracing::info!(
                target: "ag_harness::qwen",
                "GenAI client operation started"
            );
            let envelope = self.complete_call(&request).await;
            let (result, total_tokens) = match envelope {
                Ok(envelope) => {
                    if let Some(response_id) = &envelope.response_id {
                        tracing::Span::current().record("gen_ai.response.id", response_id);
                    }
                    if let Some(response_model) = &envelope.response_model {
                        tracing::Span::current().record("gen_ai.response.model", response_model);
                    }
                    if let Some(usage) = &envelope.usage {
                        if let Some(input_tokens) = usage.prompt_tokens {
                            let input_tokens = i64::try_from(input_tokens).unwrap_or(i64::MAX);
                            tracing::Span::current()
                                .record("gen_ai.usage.input_tokens", input_tokens);
                        }
                        if let Some(output_tokens) = usage.completion_tokens {
                            let output_tokens = i64::try_from(output_tokens).unwrap_or(i64::MAX);
                            tracing::Span::current()
                                .record("gen_ai.usage.output_tokens", output_tokens);
                        }
                    }

                    (
                        envelope.result,
                        envelope.usage.and_then(|usage| usage.total_tokens()),
                    )
                }
                Err(error) => (Err(error), None),
            };

            (result, total_tokens)
        }
        .instrument(span)
        .await;

        call_telemetry.finish(&result.0, result.1);

        result.0
    }
}

/// Cancellation-safe telemetry finalizer for one model call.
struct CallTelemetry<'a> {
    is_finished: bool,
    metrics: &'a telemetry::RequestMetrics,
    model: &'a str,
    span: tracing::Span,
    started_at: Instant,
}

impl<'a> CallTelemetry<'a> {
    /// Starts duration measurement and retains the call span until
    /// finalization.
    fn new(metrics: &'a telemetry::RequestMetrics, model: &'a str, span: tracing::Span) -> Self {
        Self {
            is_finished: false,
            metrics,
            model,
            span,
            started_at: Instant::now(),
        }
    }

    /// Records the terminal span state, lifecycle log, duration, and optional
    /// token total for a normally completed future.
    fn finish(
        &mut self,
        result: &Result<model::ModelResponse, model::ModelError>,
        total_tokens: Option<u64>,
    ) {
        self.is_finished = true;
        self.span.in_scope(|| match result {
            Ok(_) => {
                tracing::info!(
                    target: "ag_harness::qwen",
                    "GenAI client operation completed"
                );
            }
            Err(error) => {
                let error_type = error_type(error);
                mark_span_error(&self.span, error_type);
                tracing::warn!(
                    target: "ag_harness::qwen",
                    error_type,
                    "GenAI client operation failed"
                );
            }
        });
        self.metrics.record(
            self.started_at.elapsed(),
            PROVIDER_NAME,
            self.model,
            total_tokens,
        );
    }
}

/// Finalizes a dropped in-flight call as cancelled before exporters shut down.
impl Drop for CallTelemetry<'_> {
    fn drop(&mut self) {
        if self.is_finished {
            return;
        }

        self.span.in_scope(|| {
            mark_span_error(&self.span, REQUEST_CANCELLED_ERROR_TYPE);
            tracing::warn!(
                target: "ag_harness::qwen",
                error_type = REQUEST_CANCELLED_ERROR_TYPE,
                "GenAI client operation cancelled"
            );
        });
        self.metrics
            .record(self.started_at.elapsed(), PROVIDER_NAME, self.model, None);
    }
}

/// Marks a span as failed with a bounded error classification.
fn mark_span_error(span: &tracing::Span, error_type: &'static str) {
    span.record("error.type", error_type);
    span.record("otel.status_code", "ERROR");
}

/// Maps model failures to bounded OpenTelemetry `error.type` values.
fn error_type(error: &model::ModelError) -> &'static str {
    match error {
        model::ModelError::Request(_) => "request_error",
        model::ModelError::InvalidResponse => "invalid_response",
        model::ModelError::IncompleteResponse { .. } => "incomplete_response",
        model::ModelError::ResponseBodyTooLarge => "response_body_too_large",
        model::ModelError::UnsupportedOutputSchema { .. } => "unsupported_output_schema",
        model::ModelError::ResponseContentTooLarge => "response_content_too_large",
        model::ModelError::InvalidJson { .. } => "invalid_json",
        model::ModelError::SchemaViolation { .. } => "schema_violation",
    }
}

/// Attempt-local failure classification consumed by the bounded retry loop.
struct QwenAttemptError {
    error: model::ModelError,
    is_retryable: bool,
    retry_after: Option<Duration>,
}

impl QwenAttemptError {
    /// Classifies a request-send transport failure.
    fn transport(error: reqwest::Error) -> Self {
        tracing::Span::current().record("otel.status_code", "ERROR");
        let is_retryable = error.is_connect() || error.is_timeout();

        Self {
            error: model::ModelError::request(error),
            is_retryable,
            retry_after: None,
        }
    }

    /// Preserves the response status decision and optional provider delay when
    /// reading an error body fails.
    fn error_body(
        error: reqwest::Error,
        is_retryable: bool,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            error: model::ModelError::request(error),
            is_retryable,
            retry_after,
        }
    }

    /// Classifies an interrupted or timed-out successful response body.
    fn success_body(error: reqwest::Error) -> Self {
        tracing::Span::current().record("otel.status_code", "ERROR");
        let is_retryable = error.is_connect() || error.is_timeout() || error.is_decode();

        Self {
            error: model::ModelError::request(error),
            is_retryable,
            retry_after: None,
        }
    }

    /// Marks a bounded local validation failure as non-retryable.
    fn not_retryable(error: model::ModelError) -> Self {
        tracing::Span::current().record("otel.status_code", "ERROR");

        Self {
            error,
            is_retryable: false,
            retry_after: None,
        }
    }
}

/// Bounded provider error returned for unsuccessful HTTP statuses.
#[derive(Debug, Error)]
#[error("Qwen returned HTTP {status}: {body}")]
struct QwenHttpError {
    body: String,
    #[source]
    source: reqwest::Error,
    status: reqwest::StatusCode,
}

/// Qwen Chat Completions request envelope.
#[derive(Serialize)]
struct QwenRequest<'a> {
    messages: Vec<QwenMessage>,
    model: &'a str,
    response_format: QwenResponseFormat,
}

/// One role/content message in a Qwen request.
#[derive(Serialize)]
struct QwenMessage {
    content: String,
    role: &'static str,
}

/// Qwen response-format selector for structured JSON output.
#[derive(Serialize)]
struct QwenResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// Relevant fields decoded from a Qwen response envelope.
#[derive(Deserialize)]
struct QwenResponse {
    choices: Vec<QwenChoice>,
    id: Option<String>,
    model: Option<String>,
    usage: Option<QwenUsage>,
}

/// One Qwen response choice.
#[derive(Deserialize)]
struct QwenChoice {
    finish_reason: String,
    message: QwenResponseMessage,
}

/// Assistant content returned inside a response choice.
#[derive(Deserialize)]
struct QwenResponseMessage {
    content: Option<String>,
}

/// Parsed metadata, usage, and validated result retained across validation
/// failures.
struct QwenEnvelope {
    response_id: Option<String>,
    response_model: Option<String>,
    result: Result<model::ModelResponse, model::ModelError>,
    usage: Option<QwenUsage>,
}

/// Provider-reported input and output token counts.
#[derive(Deserialize)]
struct QwenUsage {
    completion_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
}

impl QwenUsage {
    /// Returns the saturating total when either token count is present.
    fn total_tokens(&self) -> Option<u64> {
        match (self.prompt_tokens, self.completion_tokens) {
            (None, None) => None,
            (prompt_tokens, completion_tokens) => Some(
                prompt_tokens
                    .unwrap_or_default()
                    .saturating_add(completion_tokens.unwrap_or_default()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::UNIX_EPOCH;

    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry::trace::{Status, TracerProvider as _};
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use serde_json::json;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tracing_subscriber::layer::SubscriberExt as _;
    use wiremock::matchers::{bearer_token, body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::Model;

    struct TestTelemetry {
        _subscriber_guard: tracing::subscriber::DefaultGuard,
        log_exporter: InMemoryLogExporter,
        logger_provider: SdkLoggerProvider,
        meter_provider: SdkMeterProvider,
        metric_exporter: InMemoryMetricExporter,
        span_exporter: InMemorySpanExporter,
        tracer_provider: SdkTracerProvider,
    }

    impl TestTelemetry {
        fn install() -> Self {
            let span_exporter = InMemorySpanExporter::default();
            let tracer_provider = SdkTracerProvider::builder()
                .with_simple_exporter(span_exporter.clone())
                .build();
            let log_exporter = InMemoryLogExporter::default();
            let logger_provider = SdkLoggerProvider::builder()
                .with_simple_exporter(log_exporter.clone())
                .build();
            let metric_exporter = InMemoryMetricExporter::default();
            let meter_provider = SdkMeterProvider::builder()
                .with_periodic_exporter(metric_exporter.clone())
                .build();
            let tracer = tracer_provider.tracer("ag-harness-unit-test");
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(OpenTelemetryTracingBridge::new(&logger_provider));
            let subscriber_guard = tracing::subscriber::set_default(subscriber);

            Self {
                _subscriber_guard: subscriber_guard,
                log_exporter,
                logger_provider,
                meter_provider,
                metric_exporter,
                span_exporter,
                tracer_provider,
            }
        }

        fn request_metrics(&self) -> telemetry::RequestMetrics {
            let meter = self.meter_provider.meter("ag-harness-unit-test");

            telemetry::RequestMetrics::new(&meter)
        }

        fn flush(&self) {
            self.tracer_provider
                .force_flush()
                .expect("spans should flush");
            self.logger_provider
                .force_flush()
                .expect("logs should flush");
            self.meter_provider
                .force_flush()
                .expect("metrics should flush");
        }
    }

    fn person_schema_value() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn person_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(person_schema_value()).expect("schema should be valid")
    }

    fn request(prompt: &str) -> model::ModelRequest {
        model::ModelRequest::new(prompt, person_schema())
    }

    fn escaped_value_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            },
            "required": ["value"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn qwen(server: &MockServer) -> Qwen {
        Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: format!("{}/", server.uri()),
            model: "qwen-plus".to_string(),
        })
        .expect("test client should initialize")
    }

    fn span_attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a opentelemetry::Value> {
        span.attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == key)
            .map(|attribute| &attribute.value)
    }

    async fn qwen_with_interrupted_first_body(
        status_line: &'static str,
    ) -> (Qwen, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let address = listener
            .local_addr()
            .expect("test server address should be available");
        let success_body = serde_json::to_vec(&json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        }))
        .expect("response should serialize");
        let interrupted_response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n{{"
        )
        .into_bytes();
        let success_headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            success_body.len()
        );
        let mut success_response = success_headers.into_bytes();
        success_response.extend_from_slice(&success_body);
        let server_task = tokio::spawn(async move {
            for response in [interrupted_response, success_response] {
                let (mut stream, _) = listener
                    .accept()
                    .await
                    .expect("test server should accept a request");
                read_http_request(&mut stream).await;
                stream
                    .write_all(&response)
                    .await
                    .expect("test server should write a response");
                stream
                    .shutdown()
                    .await
                    .expect("test server should close the response");
            }
        });
        let model = Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: format!("http://{address}"),
            model: "qwen-plus".to_string(),
        })
        .expect("test client should initialize");

        (model, server_task)
    }

    async fn read_http_request(stream: &mut TcpStream) {
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(
                stream
                    .read_u8()
                    .await
                    .expect("test server should read request headers"),
            );
        }
        let headers = std::str::from_utf8(&headers).expect("request headers should be UTF-8");
        let content_length = headers
            .lines()
            .find_map(|header| {
                let (name, value) = header.split_once(':')?;
                if !name.eq_ignore_ascii_case("content-length") {
                    return None;
                }

                value.trim().parse::<usize>().ok()
            })
            .expect("request should declare its body length");
        let mut body = vec![0_u8; content_length];
        stream
            .read_exact(&mut body)
            .await
            .expect("test server should read the request body");
    }

    async fn mount_structured_response(
        server: &MockServer,
        prompt: &str,
        schema: &serde_json::Value,
        content: &str,
    ) {
        let schema_instruction = format!("{STRUCTURED_OUTPUT_INSTRUCTION}{schema}");
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": schema_instruction, "role": "system"},
                    {"content": prompt, "role": "user"}
                ],
                "model": "qwen-plus",
                "response_format": {"type": "json_object"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content}
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn completes_structured_request() {
        // Arrange
        let server = MockServer::start().await;
        let schema_value = person_schema_value();
        mount_structured_response(
            &server,
            "extract the name",
            &schema_value,
            r#"{"name":"Ada"}"#,
        )
        .await;
        let model = qwen(&server);

        // Act
        let response = model
            .complete(request("extract the name"))
            .await
            .expect("Qwen request should succeed");

        // Assert
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
    }

    #[test]
    fn finalizes_cancelled_call_telemetry_on_drop() {
        // Arrange
        let telemetry = TestTelemetry::install();
        let metrics = telemetry.request_metrics();
        let span = tracing::info_span!(
            "gen_ai.client.operation",
            "otel.status_code" = field::Empty,
            "error.type" = field::Empty,
        );
        let call_telemetry = CallTelemetry::new(&metrics, "qwen-plus", span);

        // Act
        drop(call_telemetry);
        telemetry.flush();

        // Assert
        let spans = telemetry
            .span_exporter
            .get_finished_spans()
            .expect("cancelled span should be exported");
        let call_span = spans.first().expect("call span should be exported");
        assert!(matches!(call_span.status, Status::Error { .. }));
        assert!(call_span.attributes.iter().any(|attribute| {
            attribute.key.as_str() == "error.type" && attribute.value.to_string() == "cancelled"
        }));
        let finished_metrics = telemetry
            .metric_exporter
            .get_finished_metrics()
            .expect("cancelled duration should be exported");
        let duration_metric = finished_metrics
            .iter()
            .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .find(|metric| metric.name() == "gen_ai.client.operation.duration")
            .expect("duration histogram should be exported");
        assert!(matches!(
            duration_metric.data(),
            AggregatedMetrics::F64(MetricData::Histogram(histogram))
                if histogram
                    .data_points()
                    .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                    .sum::<u64>() == 1
        ));
        let trace_id = call_span.span_context.trace_id();
        let logs = telemetry
            .log_exporter
            .get_emitted_logs()
            .expect("cancelled log should be exported");
        assert!(logs.iter().any(|log| {
            log.record
                .trace_context()
                .is_some_and(|context| context.trace_id == trace_id)
                && format!("{:?}", log.record).contains("cancelled")
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retries_transient_http_response_in_separate_attempts() {
        // Arrange
        let server = MockServer::start().await;
        let attempt_count = Arc::new(AtomicUsize::new(0));
        {
            let attempt_count = Arc::clone(&attempt_count);
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(move |_: &wiremock::Request| {
                    if attempt_count.fetch_add(1, Ordering::SeqCst) == 0 {
                        return ResponseTemplate::new(429).insert_header("Retry-After", "0");
                    }

                    ResponseTemplate::new(200).set_body_json(json!({
                        "choices": [{
                            "finish_reason": "stop",
                            "message": {"content": r#"{"name":"Ada"}"#}
                        }],
                        "id": "response-42",
                        "model": "qwen-plus-2026-07",
                        "usage": {
                            "completion_tokens": 3,
                            "prompt_tokens": 4
                        }
                    }))
                })
                .expect(2)
                .mount(&server)
                .await;
        }
        let model = qwen(&server);
        let telemetry = TestTelemetry::install();
        assert!(model.metrics.set(telemetry.request_metrics()).is_ok());

        // Act
        let response = model
            .complete(request("extract the name").with_session_id("session-retry-unit-test"))
            .await
            .expect("transient failure should be retried");
        telemetry.flush();

        // Assert
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
        assert_eq!(attempt_count.load(Ordering::SeqCst), 2);
        let spans = telemetry
            .span_exporter
            .get_finished_spans()
            .expect("retry spans should be exported");
        let call_span = spans
            .iter()
            .find(|span| span.name == "chat qwen-plus")
            .expect("call span should be exported");
        assert_eq!(
            span_attribute(call_span, "gen_ai.response.id").map(ToString::to_string),
            Some("response-42".to_string())
        );
        assert_eq!(
            span_attribute(call_span, "gen_ai.response.model").map(ToString::to_string),
            Some("qwen-plus-2026-07".to_string())
        );
        assert_eq!(
            span_attribute(call_span, "gen_ai.usage.input_tokens").map(ToString::to_string),
            Some("4".to_string())
        );
        assert_eq!(
            span_attribute(call_span, "gen_ai.usage.output_tokens").map(ToString::to_string),
            Some("3".to_string())
        );
        let finished_metrics = telemetry
            .metric_exporter
            .get_finished_metrics()
            .expect("token metric should be exported");
        let token_metric = finished_metrics
            .iter()
            .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .find(|metric| metric.name() == "gen_ai.client.token.usage")
            .expect("token histogram should be exported");
        assert!(matches!(
            token_metric.data(),
            AggregatedMetrics::U64(MetricData::Histogram(histogram))
                if histogram.data_points().any(|point| point.sum() == 7)
        ));
        let logs = telemetry
            .log_exporter
            .get_emitted_logs()
            .expect("retry log should be exported");
        assert!(format!("{logs:?}").contains("Retrying GenAI HTTP request"));
    }

    #[test]
    fn parses_and_bounds_retry_after_delays() {
        // Arrange
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let future_date = httpdate::fmt_http_date(now + Duration::from_secs(30));
        let past_date = httpdate::fmt_http_date(now - Duration::from_secs(30));

        // Act
        let delta_delay = Qwen::parse_retry_after("12", now);
        let capped_delay = Qwen::parse_retry_after("3600", now);
        let date_delay = Qwen::parse_retry_after(&future_date, now);
        let past_delay = Qwen::parse_retry_after(&past_date, now);
        let invalid_delay = Qwen::parse_retry_after("later", now);

        // Assert
        assert_eq!(delta_delay, Some(Duration::from_secs(12)));
        assert_eq!(capped_delay, Some(MAX_RETRY_DELAY));
        assert_eq!(date_delay, Some(Duration::from_secs(30)));
        assert_eq!(past_delay, Some(Duration::ZERO));
        assert_eq!(invalid_delay, None);
    }

    #[test]
    fn adds_bounded_retry_jitter() {
        // Arrange
        let base = Duration::from_millis(100);

        // Act
        let delays = (0..16)
            .map(|_| Qwen::jittered_retry_delay(base))
            .collect::<Vec<_>>();
        let zero_delay = Qwen::jittered_retry_delay(Duration::ZERO);
        let capped_delay = Qwen::jittered_retry_delay(Duration::from_hours(1));

        // Assert
        assert!(delays.iter().all(|delay| {
            *delay >= base && *delay <= base.saturating_add(Duration::from_millis(25))
        }));
        assert_eq!(zero_delay, Duration::ZERO);
        assert_eq!(capped_delay, MAX_RETRY_DELAY);
    }

    #[tokio::test]
    async fn returns_request_error_for_invalid_endpoint() {
        // Arrange
        let model = Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: "http://[::1".to_string(),
            model: "qwen-plus".to_string(),
        })
        .expect("test client should initialize");

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("invalid endpoint should fail");

        // Assert
        assert!(matches!(error, model::ModelError::Request(_)));
    }

    #[test]
    fn totals_partial_complete_and_saturated_usage() {
        // Arrange
        let usages = [
            QwenUsage {
                completion_tokens: None,
                prompt_tokens: None,
            },
            QwenUsage {
                completion_tokens: None,
                prompt_tokens: Some(4),
            },
            QwenUsage {
                completion_tokens: Some(3),
                prompt_tokens: None,
            },
            QwenUsage {
                completion_tokens: Some(3),
                prompt_tokens: Some(4),
            },
            QwenUsage {
                completion_tokens: Some(1),
                prompt_tokens: Some(u64::MAX),
            },
        ];

        // Act
        let totals = usages.map(|usage| usage.total_tokens());

        // Assert
        assert_eq!(totals, [None, Some(4), Some(3), Some(7), Some(u64::MAX)]);
    }

    #[tokio::test]
    async fn retries_interrupted_success_body() {
        // Arrange
        let (model, server_task) = qwen_with_interrupted_first_body("200 OK").await;

        // Act
        let result = model.complete(request("extract the name")).await;
        let server_result = tokio::time::timeout(Duration::from_secs(2), server_task).await;

        // Assert
        let response = result.expect("interrupted success body should be retried");
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
        server_result
            .expect("retry should reach the test server")
            .expect("test server should complete");
    }

    #[tokio::test]
    async fn retries_interrupted_retryable_error_body() {
        // Arrange
        let (model, server_task) =
            qwen_with_interrupted_first_body("503 Service Unavailable").await;

        // Act
        let result = model.complete(request("extract the name")).await;
        let server_result = tokio::time::timeout(Duration::from_secs(2), server_task).await;

        // Assert
        let response = result.expect("interrupted server-error body should be retried");
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
        server_result
            .expect("retry should reach the test server")
            .expect("test server should complete");
    }

    #[tokio::test]
    async fn rejects_structured_response_stopped_for_length() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "length",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("truncated response should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::IncompleteResponse { reason } if reason == "length"
        ));
    }

    #[tokio::test]
    async fn bounds_incomplete_response_reason() {
        // Arrange
        let server = MockServer::start().await;
        let finish_reason = "x".repeat(1024);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": finish_reason.clone(),
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("incomplete response should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::IncompleteResponse { reason }
                if reason == schema_contract::bounded_diagnostic(finish_reason)
        ));
    }

    #[tokio::test]
    async fn accepts_near_limit_escaped_structured_output() {
        // Arrange
        let server = MockServer::start().await;
        let empty_content =
            serde_json::to_string(&json!({ "value": "" })).expect("content should serialize");
        let value =
            "\\".repeat((schema_contract::RESPONSE_CONTENT_LIMIT_BYTES - empty_content.len()) / 2);
        let content =
            serde_json::to_string(&json!({ "value": value })).expect("content should serialize");
        let body = serde_json::to_vec(&json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": content}
            }]
        }))
        .expect("response should serialize");
        assert!(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES - content.len() <= 1);
        assert!(
            body.len()
                > schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + RESPONSE_ENVELOPE_LIMIT_BYTES
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let response = model
            .complete(model::ModelRequest::new(
                "return escaped content",
                escaped_value_schema(),
            ))
            .await
            .expect("near-limit escaped output should succeed");

        // Assert
        assert_eq!(
            response
                .output()
                .get("value")
                .and_then(serde_json::Value::as_str)
                .map(str::len),
            Some(value.len())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_oversized_success_body_before_decoding() {
        // Arrange
        let telemetry = TestTelemetry::install();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'x';
                SUCCESS_BODY_LIMIT_BYTES
                    + 1
            ]))
            .mount(&server)
            .await;
        let model = qwen(&server);
        assert!(model.metrics.set(telemetry.request_metrics()).is_ok());

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("oversized successful response should fail");
        telemetry.flush();

        // Assert
        assert!(matches!(error, model::ModelError::ResponseBodyTooLarge));
        let spans = telemetry
            .span_exporter
            .get_finished_spans()
            .expect("attempt span should be exported");
        let attempt_span = spans
            .iter()
            .find(|span| span.name == "HTTP POST")
            .expect("attempt span should exist");
        assert!(matches!(attempt_span.status, Status::Error { .. }));
    }

    #[test]
    fn rejects_success_chunk_that_exceeds_remaining_capacity() {
        // Arrange
        let mut body = vec![0; SUCCESS_BODY_LIMIT_BYTES - 1];

        // Act
        let error = Qwen::append_success_chunk(&mut body, &[0, 1])
            .expect_err("chunk exceeding the limit should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseBodyTooLarge));
    }

    #[tokio::test]
    async fn rejects_oversized_response_content() {
        // Arrange
        let server = MockServer::start().await;
        let content = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("oversized response content should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
    }

    #[tokio::test]
    async fn rejects_schemas_without_explicit_object_root() {
        // Arrange
        let server = MockServer::start().await;
        let model = qwen(&server);
        let schema_values = [
            json!({ "type": "array" }),
            json!({ "not": { "type": "object" } }),
            json!({
                "$defs": {
                    "result": { "type": "object" }
                },
                "$ref": "#/$defs/result"
            }),
        ];

        // Act
        let mut errors = Vec::new();
        for schema_value in schema_values {
            let schema = crate::OutputSchema::new(schema_value).expect("schema should be valid");
            errors.push(
                model
                    .complete(model::ModelRequest::new("list names", schema))
                    .await
                    .expect_err("schema without an explicit object root should fail"),
            );
        }

        // Assert
        assert!(errors.into_iter().all(|error| matches!(
            error,
            model::ModelError::UnsupportedOutputSchema { reason }
                if reason == UNSUPPORTED_SCHEMA_REASON
        )));
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording should be enabled")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_malformed_structured_output() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": "not JSON"}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("malformed JSON should fail");

        // Assert
        assert!(matches!(error, model::ModelError::InvalidJson { .. }));
    }

    #[tokio::test]
    async fn rejects_structured_output_schema_violation() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":42}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::SchemaViolation { path, reason }
                if path == "/name" && reason.contains("string")
        ));
    }

    #[tokio::test]
    async fn rejects_successful_response_without_content() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": []
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("missing response content should fail");

        // Assert
        assert!(matches!(error, model::ModelError::InvalidResponse));
    }

    #[tokio::test]
    async fn returns_request_error_for_http_failure() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid API key"}
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");

        // Assert
        assert_eq!(
            error.to_string(),
            "model request failed: Qwen returned HTTP 401 Unauthorized: \
             {\"error\":{\"message\":\"invalid API key\"}}"
        );
    }

    #[tokio::test]
    async fn bounds_http_error_body() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(500).set_body_string("x".repeat(ERROR_BODY_LIMIT_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");
        let message = error.to_string();

        // Assert
        assert_eq!(
            message,
            format!(
                "model request failed: Qwen returned HTTP 500 Internal Server Error: {} ...",
                "x".repeat(ERROR_BODY_LIMIT_BYTES)
            )
        );
    }

    #[tokio::test]
    async fn returns_request_error_for_malformed_response() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("malformed response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::Request(_)));
    }
}
