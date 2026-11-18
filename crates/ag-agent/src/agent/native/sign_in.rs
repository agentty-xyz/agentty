//! Browser sign-in that lets `chatgpt/…` harness models use the user's
//! `ChatGPT` plan.

use std::collections::BTreeMap;

use ag_harness::provider::ChatGptSignIn;
use ag_session::{ChatGptSignInOutcome, ChatGptSignInUrlHandler};

use super::config::{chatgpt_auth_file, lookup};

/// Failure when no variable locates Agentty's `ChatGPT` credential file.
const UNLOCATED_AUTH_FILE: &str =
    "set CHATGPT_AUTH_FILE, AGENTTY_ROOT, or HOME to locate the ChatGPT sign-in";

/// Runs the `ChatGPT` browser sign-in configured by `environment` (the
/// credential file Agentty reads, and `CHATGPT_AUTH_URL`), passing the
/// sign-in URL to `on_url` before waiting for the browser callback, and
/// stores the credential the `chatgpt` provider reads.
///
/// # Errors
/// Returns a user-facing message when the credential file cannot be
/// located, the callback listener cannot start, or the sign-in is denied or
/// cannot be stored.
pub(crate) async fn sign_in_chatgpt(
    environment: &BTreeMap<String, String>,
    on_url: ChatGptSignInUrlHandler,
) -> Result<ChatGptSignInOutcome, String> {
    let auth_file = chatgpt_auth_file(environment).ok_or(UNLOCATED_AUTH_FILE)?;
    let sign_in = ChatGptSignIn::from_environment_with_auth_file(lookup(environment), auth_file);
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
