//! Stored "Sign in with `ChatGPT`" credentials fixture for unit tests.

use std::path::Path;

use ag_router::chatgpt::ChatGptCredentials;

/// Saves a non-expiring `ChatGPT` sign-in with plan usage at `auth_file`.
pub(crate) fn save_chatgpt_sign_in(auth_file: &Path) {
    ChatGptCredentials {
        access_token: "access".to_string(),
        client_id: "oaiapp_test".to_string(),
        email: None,
        expires_at: u64::MAX,
        ext_agent_host_id: "urn:uuid:host".to_string(),
        id_token: String::new(),
        refresh_token: "refresh".to_string(),
        scopes: vec!["chatgpt.tokens.use.direct".to_string()],
    }
    .save(auth_file)
    .expect("credentials should save");
}
