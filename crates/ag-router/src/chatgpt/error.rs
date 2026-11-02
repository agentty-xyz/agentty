use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Failure to sign in with `ChatGPT` or to keep its credentials valid.
#[derive(Debug, Error)]
pub enum ChatGptAuthError {
    /// The authorization server reported that the user denied access or
    /// the request was invalid.
    #[error("ChatGPT sign-in was not approved: {error}")]
    Denied {
        /// `OAuth` error code, such as `access_denied`.
        error: String,
    },
    /// The stored credentials could not be read or written.
    #[error("cannot access ChatGPT credentials at {path}: {source}")]
    Io {
        /// Credential file path.
        path: PathBuf,
        /// Filesystem failure.
        source: io::Error,
    },
    /// The stored credentials are not valid JSON credentials.
    #[error("ChatGPT credentials at {path} are invalid: {source}")]
    InvalidCredentials {
        /// Credential file path.
        path: PathBuf,
        /// Decoding failure.
        source: serde_json::Error,
    },
    /// The sign-in callback was malformed or did not match this sign-in.
    #[error("invalid ChatGPT sign-in callback: {reason}")]
    InvalidCallback {
        /// What was wrong with the callback.
        reason: &'static str,
    },
    /// The token endpoint returned an unexpected body.
    #[error("invalid ChatGPT token response: {0}")]
    InvalidTokenResponse(#[source] serde_json::Error),
    /// The grant does not allow using the `ChatGPT` plan for model requests.
    #[error("the ChatGPT sign-in did not grant plan usage (`chatgpt.tokens.use.direct`)")]
    MissingPlanScope,
    /// No credentials are stored; the user must sign in first.
    #[error("not signed in with ChatGPT; run `ag-harness login chatgpt` (expected {path})")]
    NotSignedIn {
        /// Credential file path that does not exist.
        path: PathBuf,
    },
    /// The refresh token is no longer accepted; the user must sign in again.
    #[error("the ChatGPT sign-in expired ({code}); run `ag-harness login chatgpt` again")]
    SignInRequired {
        /// `OAuth` error code returned by the token endpoint.
        code: String,
    },
    /// The token endpoint rejected the request.
    #[error("ChatGPT token request failed with HTTP {status}: {body}")]
    TokenRequest {
        /// Bounded response body.
        body: String,
        /// HTTP status code.
        status: u16,
    },
    /// The token endpoint returned a body larger than any grant.
    #[error("the ChatGPT token response is too large")]
    TokenResponseTooLarge,
    /// The token endpoint could not be reached or did not answer in time.
    #[error("cannot reach the ChatGPT token endpoint: {0}")]
    Transport(#[source] reqwest::Error),
}
