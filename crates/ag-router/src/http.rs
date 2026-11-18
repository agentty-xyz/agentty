//! Shared HTTP transport for provider wire APIs: provider headers, retries
//! under the provider's [`RetryPolicy`], bounded bodies, and context-overflow
//! classification.

use std::error::Error;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use thiserror::Error;

use crate::retry::{Retry, RetryKind, RetryObserver, RetryPolicy};
use crate::{model, schema as schema_contract};

pub(crate) const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const JSON_STRING_MAX_EXPANSION: usize = 6;
pub(crate) const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
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
    provider_markers: &[String],
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

/// Lowercase markers that, in a `429` body, mean the account's quota or
/// credit is spent rather than its request rate exceeded: the
/// `insufficient_quota` code and "exceeded your current quota" message of
/// the `OpenAI` API, and the `exceeded_current_quota_error` type of the
/// Moonshot (Kimi) API.
const QUOTA_EXCEEDED_MARKERS: [&str; 3] = [
    "insufficient_quota",
    "exceeded your current quota",
    "exceeded_current_quota",
];

/// Returns whether an unsuccessful response reports a spent quota or
/// credit balance, which waiting does not fix: any `402`, or a `429` whose
/// body carries a quota marker.
pub(crate) fn is_quota_exceeded(status: reqwest::StatusCode, body: &str) -> bool {
    if status == reqwest::StatusCode::PAYMENT_REQUIRED {
        return true;
    }
    if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return false;
    }

    let body = body.to_lowercase();

    QUOTA_EXCEEDED_MARKERS
        .iter()
        .any(|marker| body.contains(marker))
}

/// Returns whether `text`, read case-insensitively, carries a shared or one
/// of the lowercase `provider_markers` of a context-window overflow.
pub(crate) fn has_context_overflow_marker(text: &str, provider_markers: &[String]) -> bool {
    let text = text.to_lowercase();

    CONTEXT_OVERFLOW_MARKERS
        .iter()
        .copied()
        .chain(provider_markers.iter().map(String::as_str))
        .any(|marker| text.contains(marker))
}

/// One provider-authenticated JSON `POST` and the policy that retries it.
pub(crate) struct ProviderRequest {
    endpoint: String,
    headers: HeaderMap,
    observer: Option<Arc<dyn RetryObserver>>,
    payload: Value,
    retry: RetryPolicy,
}

impl ProviderRequest {
    /// Creates a request from provider-owned headers, such as its
    /// authentication, and payload data, retried by the default policy.
    pub(crate) fn new(headers: HeaderMap, endpoint: String, payload: Value) -> Self {
        Self {
            endpoint,
            headers,
            observer: None,
            payload,
            retry: RetryPolicy::default(),
        }
    }

    /// Retries this request under `retry`, reporting each retry to
    /// `observer`.
    #[must_use]
    pub(crate) fn with_retry(
        mut self,
        retry: RetryPolicy,
        observer: Option<Arc<dyn RetryObserver>>,
    ) -> Self {
        self.observer = observer;
        self.retry = retry;

        self
    }

    /// Returns the headers sent before the JSON content type.
    pub(crate) fn headers(&self) -> &HeaderMap {
        &self.headers
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

/// Reqwest transport that retries each request under its [`RetryPolicy`]
/// and bounds every response body it reads.
pub(crate) struct HttpTransport {
    client: reqwest::Client,
    clock: RetryClock,
    request_timeout: Duration,
}

impl HttpTransport {
    /// Creates a transport whose requests each time out after
    /// `request_timeout`, waiting out retries on the system clock.
    pub(crate) fn new(request_timeout: Duration) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            client: Self::client(reqwest::Client::builder()),
            clock: RetryClock::system(),
            request_timeout,
        }
    }

    /// Creates a transport for streamed responses: the whole request may run
    /// for `request_timeout`, but the stream fails once it stays silent for
    /// `idle_timeout`.
    pub(crate) fn streaming(idle_timeout: Duration, request_timeout: Duration) -> Self {
        let mut transport = Self::new(request_timeout);
        transport.client = Self::client(reqwest::Client::builder().read_timeout(idle_timeout));

        transport
    }

    /// Builds a client that follows no redirect, so authentication and
    /// extra headers, which may carry keys, never reach another host; a
    /// `3xx` fails as an HTTP error. A build failure falls back to
    /// [`reqwest::Client::default`], which panics on the same failure.
    fn client(builder: reqwest::ClientBuilder) -> reqwest::Client {
        builder
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    }

    /// Sends `request` and decodes its successful body.
    ///
    /// # Errors
    /// Returns a transport, HTTP, size, or decoding failure.
    pub(crate) async fn post_json<Response: DeserializeOwned>(
        &self,
        request: &ProviderRequest,
    ) -> Result<Response, TransportError> {
        let mut response = self.send(request).await?;
        let body = success_body(&mut response).await?;

        serde_json::from_slice(&body).map_err(TransportError::InvalidResponse)
    }

    /// Sends `request` and returns its successful response with the body
    /// unread, retrying the failures its [`RetryPolicy`] retries and
    /// reporting each retry to its observer before waiting. A `429` whose
    /// body reports a spent quota is returned without a retry.
    ///
    /// A timed-out request is reported without a retry, since a reasoning
    /// model that ran out the bound would likely run it out again. Dropping
    /// the returned future, as cancellation does, stops a pending wait.
    ///
    /// # Errors
    /// Returns a transport or HTTP failure.
    pub(crate) async fn send(
        &self,
        request: &ProviderRequest,
    ) -> Result<reqwest::Response, TransportError> {
        let policy = &request.retry;
        let mut connection_retries = 0_u32;
        let mut status_retries = 0_u32;
        loop {
            let response = self
                .client
                .post(request.endpoint())
                .headers(request.headers().clone())
                .timeout(self.request_timeout)
                .json(request.payload())
                .send()
                .await;
            let mut response = match response {
                Ok(response) => response,
                Err(error)
                    if !error.is_timeout()
                        && policy.allows(
                            RetryKind::Connection,
                            status_retries,
                            connection_retries,
                        ) =>
                {
                    connection_retries += 1;
                    let delay = policy.connection_delay((self.clock.jitter)());
                    self.wait(
                        request,
                        connection_retries + status_retries,
                        delay,
                        RetryKind::Connection,
                        None,
                    )
                    .await;

                    continue;
                }
                Err(error) => return Err(TransportError::reqwest(error)),
            };

            if let Err(source) = response.error_for_status_ref() {
                let status = response.status();
                let retry_after = policy.retry_after(response.headers(), (self.clock.now)());
                let body = error_body_summary(&mut response).await;
                let retried = RetryKind::of_status(status).filter(|kind| {
                    !is_quota_exceeded(status, &body)
                        && policy.allows(*kind, status_retries, connection_retries)
                });

                if let Some(kind) = retried {
                    let delay =
                        policy.status_delay(status_retries, retry_after, (self.clock.jitter)());
                    status_retries += 1;
                    drop(response);
                    self.wait(
                        request,
                        connection_retries + status_retries,
                        delay,
                        kind,
                        Some(status.as_u16()),
                    )
                    .await;

                    continue;
                }

                // Query parameters may carry keys, so no error names the URL.
                return Err(TransportError::Http {
                    body,
                    retry_after,
                    source: source.without_url(),
                    status,
                });
            }
            if response.status().is_redirection() {
                return Err(TransportError::Redirected(response.status()));
            }

            return Ok(response);
        }
    }

    /// Reports retry `attempt` of `request` to its observer, then waits
    /// `delay`.
    async fn wait(
        &self,
        request: &ProviderRequest,
        attempt: u32,
        delay: Duration,
        kind: RetryKind,
        status: Option<u16>,
    ) {
        if let Some(observer) = &request.observer {
            observer
                .retrying(Retry {
                    attempt,
                    delay,
                    kind,
                    status,
                })
                .await;
        }

        (self.clock.sleep)(delay).await;
    }
}

/// Time and randomness a transport's retries read: an injected boundary,
/// so tests wait and jitter deterministically.
#[derive(Clone, Copy)]
pub(crate) struct RetryClock {
    /// Returns a jitter sample in `[0, 1)`.
    pub(crate) jitter: fn() -> f64,
    /// Returns the wall-clock time an HTTP-date `Retry-After` is read
    /// against.
    pub(crate) now: fn() -> SystemTime,
    /// Waits out one retry delay.
    pub(crate) sleep: fn(Duration) -> tokio::time::Sleep,
}

impl RetryClock {
    /// Returns the system clock, Tokio's timer, and a fast random source.
    pub(crate) fn system() -> Self {
        Self {
            jitter: fastrand::f64,
            now: SystemTime::now,
            sleep: tokio::time::sleep,
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

async fn error_body_summary(response: &mut reqwest::Response) -> String {
    let mut body = Vec::new();
    let mut is_truncated = false;
    let read_error = loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break None,
            Err(error) => break Some(error.without_url()),
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
    while let Some(chunk) = response.chunk().await.map_err(TransportError::reqwest)? {
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
        /// Provider-declared `Retry-After` of the final response.
        retry_after: Option<Duration>,
        #[source]
        source: reqwest::Error,
        status: reqwest::StatusCode,
    },
    #[error("model API returned an invalid response: {0}")]
    InvalidResponse(#[source] serde_json::Error),
    /// A redirect, which the transport never follows; its target is not
    /// named, since a request's keys must not leave the configured host.
    #[error("model API answered with redirect HTTP {0}, which is not followed")]
    Redirected(reqwest::StatusCode),
    #[error("model API response body exceeds the size limit")]
    ResponseBodyTooLarge,
    #[error("model API transport failed: {0}")]
    Transport(#[source] Box<dyn Error + Send + Sync>),
}

impl TransportError {
    pub(crate) fn transport(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Transport(Box::new(error))
    }

    /// Wraps a client failure without its URL, whose query parameters may
    /// carry keys.
    pub(crate) fn reqwest(error: reqwest::Error) -> Self {
        Self::transport(error.without_url())
    }

    /// Classifies this failure for `provider`, recognizing a context-window
    /// overflow by the shared markers and `overflow_markers`.
    pub(crate) fn into_model_error(
        self,
        provider: &str,
        overflow_markers: &[String],
    ) -> model::ModelError {
        match self {
            Self::Http {
                body,
                retry_after,
                source,
                status,
            } => {
                let classification = model::ProviderRejection {
                    context_overflow: is_context_overflow(status, &body, overflow_markers),
                    quota_exceeded: is_quota_exceeded(status, &body),
                    retry_after,
                };

                model::ModelError::provider_request(provider, body, source, status, classification)
            }
            error @ Self::InvalidResponse(_) => model::ModelError::classified_request(
                model::ModelErrorType::InvalidProviderResponse,
                None,
                error.into(),
            ),
            error @ Self::Redirected(status) => model::ModelError::classified_request(
                model::ModelErrorType::Provider,
                Some(status.as_u16()),
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

#[cfg(test)]
#[path = "http_retry_test.rs"]
mod retry_tests;
