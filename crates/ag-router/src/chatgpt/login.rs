//! Browser sign-in with PKCE that registers this app with `OpenAI` on first
//! use and reuses that registration afterwards.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use super::credentials::ChatGptCredentials;
use super::error::ChatGptAuthError;
use super::token::{self, DEFAULT_ISSUER, RESOURCE};

/// App name `OpenAI` shows the user at first sign-in.
const AGENT_NAME: &str = "Agentty";
const AUTHORIZE_PATH: &str = "/api/accounts/authorize";
/// Client identifier that asks `OpenAI` to register a new app client.
const REGISTRATION_CLIENT_ID: &str = "dynamic_agent_client";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// One pending sign-in: build [`Self::authorize_url`], open it in the
/// user's browser, and pass the query of the request the browser sends to
/// `redirect_uri` to [`Self::finish`].
pub struct ChatGptLogin {
    code_verifier: String,
    ext_agent_host_id: String,
    issuer: String,
    nonce: String,
    previous: Option<ChatGptCredentials>,
    redirect_uri: String,
    state: String,
}

impl ChatGptLogin {
    /// Starts a sign-in that returns to `redirect_uri`, a loopback URL such
    /// as `http://127.0.0.1:1455/auth/callback`.
    ///
    /// With `previous` credentials, the sign-in reuses their registered
    /// client and host identifier; otherwise it registers this app.
    pub fn new(redirect_uri: impl Into<String>, previous: Option<ChatGptCredentials>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ext_agent_host_id = previous.as_ref().map_or_else(
            || format!("urn:uuid:{}", uuid::Uuid::new_v4()),
            |credentials| credentials.ext_agent_host_id.clone(),
        );

        Self {
            code_verifier: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            ext_agent_host_id,
            issuer: DEFAULT_ISSUER.to_string(),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            previous,
            redirect_uri: redirect_uri.into(),
            state: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    /// Replaces the authorization server base URL, such as for a local test
    /// server.
    #[must_use]
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = issuer.into();

        self
    }

    /// Returns the URL the user opens to approve the sign-in.
    pub fn authorize_url(&self) -> String {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.code_verifier.as_bytes()));
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("client_id", self.client_id());
        match &self.previous {
            Some(previous) if !previous.id_token.is_empty() => {
                query.append_pair("id_token_hint", &previous.id_token);
            }
            Some(_) => {}
            None => {
                query.append_pair("agent_name_hint", AGENT_NAME);
            }
        }
        query
            .append_pair("ext_agent_host_id", &self.ext_agent_host_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("scope", SCOPE)
            .append_pair("resource", RESOURCE)
            .append_pair("state", &self.state)
            .append_pair("nonce", &self.nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge);

        format!(
            "{}{AUTHORIZE_PATH}?{}",
            self.issuer.trim_end_matches('/'),
            query.finish()
        )
    }

    /// Completes the sign-in from the callback's query string and exchanges
    /// its code for credentials.
    ///
    /// # Errors
    /// Returns an error when the user denied access, the callback does not
    /// belong to this sign-in, the token exchange fails, or the grant lacks
    /// `ChatGPT` plan usage.
    pub async fn finish(
        &self,
        callback_query: &str,
    ) -> Result<ChatGptCredentials, ChatGptAuthError> {
        let parameters: Vec<(String, String)> =
            url::form_urlencoded::parse(callback_query.trim_start_matches('?').as_bytes())
                .into_owned()
                .collect();
        let parameter = |name: &str| {
            parameters
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        if parameter("state") != Some(self.state.as_str()) {
            return Err(ChatGptAuthError::InvalidCallback {
                reason: "state does not match this sign-in",
            });
        }
        if let Some(error) = parameter("error") {
            return Err(ChatGptAuthError::Denied {
                error: error.to_string(),
            });
        }
        let code = parameter("code").ok_or(ChatGptAuthError::InvalidCallback {
            reason: "missing authorization code",
        })?;
        let client_id = match (&self.previous, parameter("client_id")) {
            (None, Some(issued)) => issued.to_string(),
            (None, None) => {
                return Err(ChatGptAuthError::InvalidCallback {
                    reason: "app registration returned no client_id",
                });
            }
            (Some(previous), Some(issued)) if issued != previous.client_id => {
                return Err(ChatGptAuthError::InvalidCallback {
                    reason: "client_id differs from the registered app",
                });
            }
            (Some(previous), _) => previous.client_id.clone(),
        };
        let grant = token::request_tokens(
            &reqwest::Client::new(),
            &self.issuer,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", code),
                ("code_verifier", &self.code_verifier),
                ("redirect_uri", &self.redirect_uri),
                ("resource", RESOURCE),
            ],
        )
        .await?;

        ChatGptCredentials::from_grant(grant, client_id, self.ext_agent_host_id.clone(), None)
    }

    fn client_id(&self) -> &str {
        self.previous
            .as_ref()
            .map_or(REGISTRATION_CLIENT_ID, |previous| {
                previous.client_id.as_str()
            })
    }
}

#[cfg(test)]
#[path = "login_test.rs"]
mod tests;
