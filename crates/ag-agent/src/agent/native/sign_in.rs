//! Browser sign-in that lets `chatgpt/…` harness models use the user's
//! `ChatGPT` plan.

use std::collections::BTreeMap;

use ag_harness::provider::ChatGptSignIn;
use ag_session::{ChatGptSignInOutcome, ChatGptSignInUrlHandler};

use super::config::lookup;

/// Runs the `ChatGPT` browser sign-in configured by `environment`
/// (`CHATGPT_AUTH_FILE`, `CHATGPT_AUTH_URL`), passing the sign-in URL to
/// `on_url` before waiting for the browser callback, and stores the
/// credential the `chatgpt` provider reads.
///
/// # Errors
/// Returns a user-facing message when the credential file cannot be
/// located, the callback listener cannot start, or the sign-in is denied or
/// cannot be stored.
pub(crate) async fn sign_in_chatgpt(
    environment: &BTreeMap<String, String>,
    on_url: ChatGptSignInUrlHandler,
) -> Result<ChatGptSignInOutcome, String> {
    let sign_in =
        ChatGptSignIn::from_environment(lookup(environment)).map_err(|error| error.to_string())?;
    let pending = sign_in.start().await.map_err(|error| error.to_string())?;
    on_url(pending.authorize_url());
    let credentials = pending
        .complete()
        .await
        .map_err(|error| error.to_string())?;

    Ok(ChatGptSignInOutcome {
        email: credentials.email,
    })
}

#[cfg(test)]
#[path = "sign_in_test.rs"]
mod tests;
