//! Shared HTTP transport for provider wire APIs: bearer authentication,
//! bounded retries, bounded bodies, and context-overflow classification.

use std::error::Error;
use std::fmt::Write as _;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use thiserror::Error;

use crate::{model, schema as schema_contract};

pub(crate) const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const JSON_STRING_MAX_EXPANSION: usize = 6;
const MAX_RATE_LIMIT_RETRIES: usize = 5;
/// Ceiling for the exponential backoff between rate-limit retries.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
/// Ceiling for a provider-declared `Retry-After`, so a per-minute quota can
/// be waited out without honoring an unbounded instruction.
const MAX_PROVIDER_RETRY_DELAY: Duration = Duration::from_secs(60);
const MAX_TRANSPORT_RETRIES: usize = 1;
pub(crate) const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
const RETRY_DELAY: Duration = Duration::from_secs(1);
pub(crate) const SUCCESS_BODY_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_ENVELOPE_LIMIT_BYTES;

/// Lowercase markers that, in an unsuccessful response body from any
/// OpenAI-compatible provider, mean the request input exceeded the model's
/// context window: the `context_length_exceeded` code and "maximum
/// context length" message of the `OpenAI` API.
const CONTEXT_OVERFLOW_MARKERS: [&str; 2] = ["context_length_exceeded", "maximum context length"];

/// Returns whether an unsuccessful response rejected the request because its
/// input exceeds the model's context window: any `413`, or a `400` or `422`
/// whose body carries a shared or one of `provider_markers`.
pub(crate) fn is_context_overflow(
    status: reqwest::StatusCode,
    body: &str,
    provider_markers: &[&str],
) -> bool {
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return true;
    }
    if !matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }

    has_context_overflow_marker(body, provider_markers)
}

/// Returns whether `text`, read case-insensitively, carries a shared or one
/// of `provider_markers` of a context-window overflow.
pub(crate) fn has_context_overflow_marker(text: &str, provider_markers: &[&str]) -> bool {
    let text = text.to_lowercase();

    CONTEXT_OVERFLOW_MARKERS
        .iter()
        .chain(provider_markers)
        .any(|marker| text.contains(marker))
}

/// One provider-authenticated JSON `POST`.
pub(crate) struct ProviderRequest<'a> {
    api_key: Option<&'a str>,
    endpoint: String,
    payload: Value,
}

impl<'a> ProviderRequest<'a> {
    /// Creates a request from provider-owned authentication and payload data.
    pub(crate) fn new(api_key: Option<&'a str>, endpoint: String, payload: Value) -> Self {
        Self {
            api_key,
            endpoint,
            payload,
        }
    }

    /// Returns the provider bearer token, or `None` for an unauthenticated
    /// request.
    pub(crate) fn api_key(&self) -> Option<&str> {
        self.api_key
    }

    /// Returns the provider request endpoint.
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the serialized provider request body.
    pub(crate) fn payload(&self) -> &Value {
        &self.payload
    }
}

/// Reqwest transport that retries rate limits and one connection failure,
/// and bounds every response body it reads.
pub(crate) struct HttpTransport {
    client: reqwest::Client,
    max_retry_delay: Duration,
    request_timeout: Duration,
    retry_delay: Duration,
}

impl HttpTransport {
    /// Creates a transport whose requests each time out after
    /// `request_timeout`, with the production retry delays.
    pub(crate) fn new(request_timeout: Duration) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            client: reqwest::Client::new(),
            max_retry_delay: MAX_RETRY_DELAY,
            request_timeout,
            retry_delay: RETRY_DELAY,
        }
    }

    /// Creates a transport for streamed responses: the whole request may run
    /// for `request_timeout`, but the stream fails once it stays silent for
    /// `idle_timeout`.
    pub(crate) fn streaming(idle_timeout: Duration, request_timeout: Duration) -> Self {
        let mut transport = Self::new(request_timeout);
        transport.client = reqwest::Client::builder()
            .read_timeout(idle_timeout)
            .build()
            .unwrap_or_default();

        transport
    }

    /// Sends `request` and decodes its successful body.
    ///
    /// # Errors
    /// Returns a transport, HTTP, size, or decoding failure.
    pub(crate) async fn post_json<Response: DeserializeOwned>(
        &self,
        request: &ProviderRequest<'_>,
    ) -> Result<Response, TransportError> {
        let mut response = self.send(request).await?;
        let body = success_body(&mut response).await?;

        serde_json::from_slice(&body).map_err(TransportError::InvalidResponse)
    }

    /// Sends `request` and returns its successful response with the body
    /// unread, retrying rate limits and one connection failure.
    ///
    /// A timed-out request is reported without a retry, since a reasoning
    /// model that ran out the bound would likely run it out again.
    ///
    /// # Errors
    /// Returns a transport or HTTP failure.
    pub(crate) async fn send(
        &self,
        request: &ProviderRequest<'_>,
    ) -> Result<reqwest::Response, TransportError> {
        let mut rate_limit_retries = 0_usize;
        let mut transport_retries = 0_usize;
        loop {
            let mut builder = self.client.post(request.endpoint());
            if let Some(api_key) = request.api_key() {
                builder = builder.bearer_auth(api_key);
            }
            let response = builder
                .timeout(self.request_timeout)
                .json(request.payload())
                .send()
                .await;
            let mut response = match response {
                Ok(response) => response,
                Err(error) if !error.is_timeout() && transport_retries < MAX_TRANSPORT_RETRIES => {
                    transport_retries += 1;
                    tokio::time::sleep(self.retry_delay).await;

                    continue;
                }
                Err(error) => return Err(TransportError::transport(error)),
            };

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                && rate_limit_retries < MAX_RATE_LIMIT_RETRIES
            {
                let delay = rate_limit_retry_delay(
                    response.headers(),
                    rate_limit_retries,
                    self.retry_delay,
                    self.max_retry_delay,
                );
                rate_limit_retries += 1;
                drop(response);
                tokio::time::sleep(delay).await;

                continue;
            }
            if let Err(source) = response.error_for_status_ref() {
                let status = response.status();
                let body = error_body_summary(&mut response).await;

                return Err(TransportError::Http {
                    body,
                    source,
                    status,
                });
            }

            return Ok(response);
        }
    }
}

/// Deserializes an optional field leniently: a value of another shape reads
/// as absent instead of failing the whole response.
pub(crate) fn deserialize_optional<'de, DeserializerType, ValueType>(
    deserializer: DeserializerType,
) -> Result<Option<ValueType>, DeserializerType::Error>
where
    DeserializerType: Deserializer<'de>,
    ValueType: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;

    Ok(serde_json::from_value(value).ok())
}

fn rate_limit_retry_delay(
    headers: &reqwest::header::HeaderMap,
    retry: usize,
    retry_delay: Duration,
    max_retry_delay: Duration,
) -> Duration {
    let provider_delay = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map_or_default(Duration::from_secs);
    let backoff = retry_delay
        .saturating_mul(1_u32 << retry.min(31))
        .min(max_retry_delay);

    provider_delay.min(MAX_PROVIDER_RETRY_DELAY).max(backoff)
}

async fn error_body_summary(response: &mut reqwest::Response) -> String {
    let mut body = Vec::new();
    let mut is_truncated = false;
    let read_error = loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break None,
            Err(error) => break Some(error),
        };

        let remaining = ERROR_BODY_LIMIT_BYTES.saturating_sub(body.len());

        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            is_truncated = true;

            break None;
        }

        body.extend_from_slice(&chunk);
    };

    let mut summary = String::from_utf8_lossy(&body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if is_truncated {
        summary.push_str(" ...");
    }
    if let Some(error) = read_error {
        if !summary.is_empty() {
            summary.push(' ');
        }
        let _ = write!(&mut summary, "[error body read failed: {error}]");
    }

    summary
}

async fn success_body(response: &mut reqwest::Response) -> Result<Vec<u8>, TransportError> {
    let limit = u64::try_from(SUCCESS_BODY_LIMIT_BYTES).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|content_length| content_length > limit)
    {
        return Err(TransportError::ResponseBodyTooLarge);
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(TransportError::transport)? {
        append_success_chunk(&mut body, &chunk)?;
    }

    Ok(body)
}

fn append_success_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), TransportError> {
    let remaining = SUCCESS_BODY_LIMIT_BYTES.saturating_sub(body.len());
    if chunk.len() > remaining {
        return Err(TransportError::ResponseBodyTooLarge);
    }

    body.extend_from_slice(chunk);

    Ok(())
}

/// Failure produced by a provider transport.
#[derive(Debug, Error)]
pub(crate) enum TransportError {
    #[error("model API returned HTTP {status}: {body}")]
    Http {
        body: String,
        #[source]
        source: reqwest::Error,
        status: reqwest::StatusCode,
    },
    #[error("model API returned an invalid response: {0}")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("model API response body exceeds the size limit")]
    ResponseBodyTooLarge,
    #[error("model API transport failed: {0}")]
    Transport(#[source] Box<dyn Error + Send + Sync>),
}

impl TransportError {
    pub(crate) fn transport(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Transport(Box::new(error))
    }

    /// Classifies this failure for `provider`, recognizing a context-window
    /// overflow by the shared markers and `overflow_markers`.
    pub(crate) fn into_model_error(
        self,
        provider: &'static str,
        overflow_markers: &[&str],
    ) -> model::ModelError {
        match self {
            Self::Http {
                body,
                source,
                status,
            } => {
                let context_overflow = is_context_overflow(status, &body, overflow_markers);

                model::ModelError::provider_request(
                    provider,
                    body,
                    source,
                    status,
                    context_overflow,
                )
            }
            error @ Self::InvalidResponse(_) => model::ModelError::classified_request(
                model::ModelErrorType::InvalidProviderResponse,
                None,
                error.into(),
            ),
            Self::ResponseBodyTooLarge => model::ModelError::ResponseBodyTooLarge,
            error @ Self::Transport(_) => model::ModelError::classified_request(
                model::ModelErrorType::Transport,
                None,
                error.into(),
            ),
        }
    }
}

#[cfg(test)]
#[path = "http_test.rs"]
mod tests;
