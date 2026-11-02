//! Token endpoint wire format shared by sign-in and refresh.

use std::time::Duration;

use serde::Deserialize;

use super::error::ChatGptAuthError;
use crate::http::ERROR_BODY_LIMIT_BYTES;

/// Deadline for one token request, including reading its response body.
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest token endpoint response body read; grants are a few kilobytes.
const TOKEN_BODY_LIMIT_BYTES: usize = 64 * 1024;

/// Production authorization server.
pub(super) const DEFAULT_ISSUER: &str = "https://auth.openai.com";
/// Resource the access token is issued for: the public `OpenAI` API.
pub(super) const RESOURCE: &str = "https://api.openai.com/v1";
/// Scope that lets an access token bill requests to the `ChatGPT` plan.
pub(super) const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// Refresh-grant error codes after which only a new sign-in helps.
const SIGN_IN_REQUIRED_CODES: [&str; 6] = [
    "invalid_grant",
    "invalid_refresh_token",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
    "token_expired",
];
const TOKEN_PATH: &str = "/api/accounts/oauth/token";

/// Successful token endpoint response for both grant types.
#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    pub(super) expires_in: u64,
    pub(super) id_token: Option<String>,
    pub(super) refresh_token: Option<String>,
    pub(super) scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenErrorBody {
    error: String,
}

/// Returns the HTTP client for token requests, which each fail after
/// [`TOKEN_REQUEST_TIMEOUT`].
pub(super) fn http_client() -> reqwest::Client {
    http_client_with_timeout(TOKEN_REQUEST_TIMEOUT)
}

/// Returns an HTTP client whose requests each fail after `timeout`.
pub(super) fn http_client_with_timeout(timeout: Duration) -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();

    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default()
}

/// Posts a form-encoded grant to the issuer's token endpoint.
pub(super) async fn request_tokens(
    http: &reqwest::Client,
    issuer: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, ChatGptAuthError> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let mut response = http
        .post(format!("{}{TOKEN_PATH}", issuer.trim_end_matches('/')))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(ChatGptAuthError::Transport)?;
    let status = response.status();
    let (body, is_truncated) = bounded_body(&mut response).await?;
    if status.is_success() {
        if is_truncated {
            return Err(ChatGptAuthError::TokenResponseTooLarge);
        }

        return serde_json::from_slice(&body).map_err(ChatGptAuthError::InvalidTokenResponse);
    }
    let text = String::from_utf8_lossy(&body);
    if let Ok(error) = serde_json::from_str::<TokenErrorBody>(&text)
        && SIGN_IN_REQUIRED_CODES.contains(&error.error.as_str())
    {
        return Err(ChatGptAuthError::SignInRequired { code: error.error });
    }

    Err(ChatGptAuthError::TokenRequest {
        body: text.chars().take(ERROR_BODY_LIMIT_BYTES).collect(),
        status: status.as_u16(),
    })
}

/// Reads at most [`TOKEN_BODY_LIMIT_BYTES`] of the response body and reports
/// whether more was left unread.
async fn bounded_body(
    response: &mut reqwest::Response,
) -> Result<(Vec<u8>, bool), ChatGptAuthError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(ChatGptAuthError::Transport)?
    {
        let remaining = TOKEN_BODY_LIMIT_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);

            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }

    Ok((body, false))
}

#[cfg(test)]
#[path = "token_test.rs"]
mod tests;
