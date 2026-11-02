//! Host-supplied bearer credentials resolved before each provider request.

use std::error::Error;

use async_trait::async_trait;

/// Bearer token source for a provider whose token expires, such as a
/// [`crate::chatgpt::ChatGptAuth`] sign-in. The router asks for a token before
/// every request, so an implementation refreshes it when needed.
#[async_trait]
pub trait BearerCredential: Send + Sync {
    /// Returns a bearer token valid for the next request.
    ///
    /// # Errors
    /// Returns an error when no valid token can be produced, such as when the
    /// user must sign in again.
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>>;
}
