//! Token endpoint wire format shared by sign-in and refresh.

use serde::Deserialize;

use super::error::ChatGptAuthError;
use crate::http::ERROR_BODY_LIMIT_BYTES;

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

/// Posts a form-encoded grant to the issuer's token endpoint.
pub(super) async fn request_tokens(
    http: &reqwest::Client,
    issuer: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, ChatGptAuthError> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let response = http
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
    let text = response.text().await.map_err(ChatGptAuthError::Transport)?;
    if status.is_success() {
        return serde_json::from_str(&text).map_err(ChatGptAuthError::InvalidTokenResponse);
    }
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

#[cfg(test)]
#[path = "token_test.rs"]
mod tests;
