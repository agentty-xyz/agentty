use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::auth;
use super::error::CodexClientError;
use super::sse::{self, CodexResponsesCompletion};
use crate::model::{ModelError, ReasoningEffort};
use crate::{schema_contract, transport};

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub(super) const RESPONSE_TIMEOUT: Duration = Duration::from_mins(30);
pub(super) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct CodexResponsesRequest {
    pub(super) input: Vec<Value>,
    pub(super) instructions: String,
    pub(super) model: String,
    pub(super) output_schema: Value,
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    pub(super) replay_account_fingerprint: Option<String>,
}

#[async_trait]
pub(super) trait CodexResponsesClient: Send + Sync {
    async fn complete(
        &self,
        request: CodexResponsesRequest,
    ) -> Result<CodexResponsesCompletion, ModelError>;
}

pub(super) struct HttpCodexResponsesClient {
    account_id: Mutex<Option<String>>,
    auth_file: Option<PathBuf>,
    endpoint: String,
    http: Result<reqwest::Client, Arc<reqwest::Error>>,
    request_timeout: Duration,
    response_timeout: Duration,
    stream_idle_timeout: Duration,
}

impl HttpCodexResponsesClient {
    pub(super) fn new(auth_file: Option<PathBuf>) -> Self {
        Self {
            account_id: Mutex::new(None),
            auth_file,
            endpoint: CODEX_RESPONSES_URL.to_string(),
            http: http_client(),
            request_timeout: REQUEST_TIMEOUT,
            response_timeout: RESPONSE_TIMEOUT,
            stream_idle_timeout: STREAM_IDLE_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(super) fn with_endpoint(auth_file: PathBuf, endpoint: String) -> Self {
        Self::with_endpoint_and_timeouts(
            auth_file,
            endpoint,
            REQUEST_TIMEOUT,
            RESPONSE_TIMEOUT,
            STREAM_IDLE_TIMEOUT,
        )
    }

    #[cfg(test)]
    pub(super) fn with_endpoint_and_timeouts(
        auth_file: PathBuf,
        endpoint: String,
        request_timeout: Duration,
        response_timeout: Duration,
        stream_idle_timeout: Duration,
    ) -> Self {
        Self {
            account_id: Mutex::new(None),
            auth_file: Some(auth_file),
            endpoint,
            http: http_client(),
            request_timeout,
            response_timeout,
            stream_idle_timeout,
        }
    }

    #[cfg(test)]
    pub(super) fn with_http_error(auth_file: PathBuf, source: reqwest::Error) -> Self {
        Self {
            account_id: Mutex::new(None),
            auth_file: Some(auth_file),
            endpoint: "https://example.invalid".to_string(),
            http: Err(Arc::new(source)),
            request_timeout: REQUEST_TIMEOUT,
            response_timeout: RESPONSE_TIMEOUT,
            stream_idle_timeout: STREAM_IDLE_TIMEOUT,
        }
    }

    fn bind_account(&self, account_id: &str) -> Result<(), CodexClientError> {
        let mut bound_account = self
            .account_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bound_account) = bound_account.as_deref() {
            if bound_account != account_id {
                return Err(CodexClientError::AuthAccountChanged);
            }

            return Ok(());
        }
        *bound_account = Some(account_id.to_string());

        Ok(())
    }
}

#[async_trait]
impl CodexResponsesClient for HttpCodexResponsesClient {
    async fn complete(
        &self,
        request: CodexResponsesRequest,
    ) -> Result<CodexResponsesCompletion, ModelError> {
        let auth = auth::request_auth(self.auth_file.as_deref())
            .await
            .map_err(CodexClientError::into_model_error)?;
        let account_fingerprint = auth.account_fingerprint();
        if request
            .replay_account_fingerprint
            .as_deref()
            .is_some_and(|expected| expected != account_fingerprint)
        {
            return Err(CodexClientError::AuthAccountChanged.into_model_error());
        }
        self.bind_account(&auth.account_id)
            .map_err(CodexClientError::into_model_error)?;
        let headers = auth.headers().map_err(CodexClientError::into_model_error)?;
        let payload = json!({
            "include": ["reasoning.encrypted_content"],
            "input": request.input,
            "instructions": request.instructions,
            "model": request.model,
            "reasoning": request.reasoning_effort.map(|effort| json!({
                "effort": effort
            })),
            "store": false,
            "stream": true,
            "text": {
                "format": {
                    "name": "ag_harness_output",
                    "type": "json_schema",
                    "strict": true,
                    "schema": request.output_schema
                }
            }
        });
        let http = self
            .http
            .as_ref()
            .map_err(|source| CodexClientError::HttpClient(source.clone()).into_model_error())?;
        let send = http
            .post(&self.endpoint)
            .headers(headers)
            .json(&payload)
            .send();
        let mut response = tokio::time::timeout(self.request_timeout, send)
            .await
            .map_err(|_| CodexClientError::RequestTimeout.into_model_error())?
            .map_err(|source| CodexClientError::Transport(source).into_model_error())?;
        if let Err(source) = response.error_for_status_ref() {
            let status = response.status();
            let body = sse::with_response_timeout(
                sse::read_response_body(
                    &mut response,
                    transport::ERROR_BODY_LIMIT_BYTES,
                    self.stream_idle_timeout,
                ),
                self.response_timeout,
            )
            .await
            .unwrap_or_else(|error| error.to_string());

            return Err(ModelError::provider_request(
                "Codex subscription endpoint",
                schema_contract::bounded_diagnostic(&body),
                source,
                status,
            ));
        }

        let mut completion = sse::with_response_timeout(
            sse::read_sse_response(&mut response, self.stream_idle_timeout),
            self.response_timeout,
        )
        .await
        .map_err(CodexClientError::into_model_error)?;
        completion.account_fingerprint = Some(account_fingerprint);

        Ok(completion)
    }
}

fn http_client() -> Result<reqwest::Client, Arc<reqwest::Error>> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Arc::new)
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
