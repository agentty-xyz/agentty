use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use reqwest::header::AUTHORIZATION;
use rustix::fs::OFlags;
use serde_json::json;

use super::super::error::CodexClientError;
use super::super::test_support::{
    auth_with_fedramp, id_token, id_token_with_account, valid_auth, write_auth,
};
use super::{
    ACCOUNT_ID_HEADER, AUTH_FILE_LIMIT_BYTES, AUTH_FILE_OPEN_FLAGS, CodexAuth, CodexAuthFile,
    FEDRAMP_HEADER, environment_variable, read_chatgpt_auth, resolve_auth_file,
    spawn_auth_file_task,
};

#[test]
fn authentication_path_resolution_has_explicit_and_environment_fallbacks() {
    // Arrange
    let mut environment = HashMap::from([
        ("CODEX_HOME", OsString::from("codex-home")),
        ("HOME", OsString::from("user-home")),
    ]);

    // Act
    let explicit = resolve_auth_file(Some(Path::new("explicit.json")), |_| None);
    let codex_home = resolve_auth_file(None, |name| environment.get(name).cloned());
    environment.remove("CODEX_HOME");
    let home = resolve_auth_file(None, |name| environment.get(name).cloned());
    environment.insert("CODEX_HOME", OsString::new());
    let empty_codex_home = resolve_auth_file(None, |name| environment.get(name).cloned());
    environment.insert("HOME", OsString::new());
    let empty_variables = resolve_auth_file(None, |name| environment.get(name).cloned());
    environment.clear();
    let missing = resolve_auth_file(None, |name| environment.get(name).cloned());

    // Assert
    assert_eq!(
        explicit.expect("explicit path should resolve"),
        Path::new("explicit.json")
    );
    assert_eq!(
        codex_home.expect("Codex home should resolve"),
        Path::new("codex-home/auth.json")
    );
    assert_eq!(
        home.expect("home should resolve"),
        Path::new("user-home/.codex/auth.json")
    );
    assert_eq!(
        empty_codex_home.expect("empty Codex home should fall back to home"),
        Path::new("user-home/.codex/auth.json")
    );
    assert!(matches!(
        empty_variables,
        Err(CodexClientError::AuthFileUnavailable)
    ));
    assert!(matches!(
        missing,
        Err(CodexClientError::AuthFileUnavailable)
    ));
    assert_eq!(environment_variable("HOME"), std::env::var_os("HOME"));
}

#[tokio::test]
async fn authentication_loader_accepts_chatgpt() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let valid_path = write_auth(directory.path(), &valid_auth());
    let fedramp = serde_json::from_value::<CodexAuthFile>(auth_with_fedramp(true))
        .expect("FedRAMP auth should decode")
        .chatgpt_auth()
        .expect("FedRAMP auth should load");

    // Act
    let auth = read_chatgpt_auth(&valid_path)
        .await
        .expect("ChatGPT auth should load");
    let headers = auth.headers().expect("headers should build");
    let fedramp_headers = fedramp.headers().expect("FedRAMP headers should build");

    // Assert
    assert_eq!(auth.account_id, "account-1");
    assert_ne!(auth.account_fingerprint(), auth.account_id);
    assert_eq!(auth.access_token, "access-token");
    assert!(!auth.is_fedramp_account);
    assert_eq!(headers[AUTHORIZATION], "Bearer access-token");
    assert!(headers[AUTHORIZATION].is_sensitive());
    assert!(headers[ACCOUNT_ID_HEADER].is_sensitive());
    assert_eq!(fedramp_headers[FEDRAMP_HEADER], "true");
}

#[test]
fn authentication_accepts_chatgpt_with_compatibility_api_key() {
    // Arrange
    let mut fixture = valid_auth();
    fixture["OPENAI_API_KEY"] = json!("sk-compatibility");

    // Act
    let auth = serde_json::from_value::<CodexAuthFile>(fixture)
        .expect("authentication fixture should decode")
        .chatgpt_auth();

    // Assert
    assert!(auth.is_ok());
}

#[test]
fn authentication_resolves_account_id_from_explicit_field_or_id_token() {
    // Arrange
    let mut explicit = valid_auth();
    explicit["tokens"]["account_id"] = json!("explicit-account");
    let mut missing = valid_auth();
    missing["tokens"]
        .as_object_mut()
        .expect("tokens should be an object")
        .remove("account_id");
    let mut blank = valid_auth();
    blank["tokens"]["account_id"] = json!(" \n");

    // Act
    let account_ids = [explicit, missing, blank].map(|fixture| {
        serde_json::from_value::<CodexAuthFile>(fixture)
            .expect("authentication fixture should decode")
            .chatgpt_auth()
            .expect("account ID should resolve")
            .account_id
    });

    // Assert
    assert_eq!(account_ids, ["explicit-account", "account-1", "account-1"]);
}

#[tokio::test]
async fn authentication_loader_bounds_and_validates_the_file() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let missing = directory.path().join("missing.json");
    let oversized = directory.path().join("oversized.json");
    let malformed = directory.path().join("malformed.json");
    let oversized_length = AUTH_FILE_LIMIT_BYTES.saturating_add(1);
    fs::write(&oversized, vec![b'x'; oversized_length])
        .expect("oversized fixture should be written");
    fs::write(&malformed, b"not-json").expect("malformed fixture should be written");

    // Act
    let missing_error = read_chatgpt_auth(&missing).await.err();
    let oversized_error = read_chatgpt_auth(&oversized).await.err();
    let malformed_error = read_chatgpt_auth(&malformed).await.err();
    let non_regular_error = read_chatgpt_auth(directory.path()).await.err();

    // Assert
    assert!(AUTH_FILE_OPEN_FLAGS.contains(OFlags::NONBLOCK));
    assert!(matches!(missing_error, Some(CodexClientError::ReadAuth(_))));
    assert!(matches!(
        oversized_error,
        Some(CodexClientError::AuthFileTooLarge)
    ));
    assert!(matches!(
        malformed_error,
        Some(CodexClientError::ParseAuth(_))
    ));
    assert!(matches!(
        non_regular_error,
        Some(CodexClientError::AuthFileNotRegular)
    ));
}

#[tokio::test]
async fn authentication_file_tasks_use_the_blocking_pool_and_map_panics() {
    // Arrange
    let caller_thread = std::thread::current().id();
    let worker_thread = Arc::new(Mutex::new(None));
    let observed_worker_thread = worker_thread.clone();

    // Act
    let operation_error = spawn_auth_file_task(move || {
        *observed_worker_thread
            .lock()
            .expect("worker thread should lock") = Some(std::thread::current().id());

        Err(CodexClientError::AuthFileNotRegular)
    })
    .await
    .err();
    let task_error = spawn_auth_file_task(|| -> Result<std::fs::File, CodexClientError> {
        std::panic::resume_unwind(Box::new("test blocking task panic"))
    })
    .await
    .err();

    // Assert
    assert_ne!(
        *worker_thread.lock().expect("worker thread should lock"),
        Some(caller_thread)
    );
    assert!(matches!(
        operation_error,
        Some(CodexClientError::AuthFileNotRegular)
    ));
    assert!(matches!(
        task_error,
        Some(CodexClientError::AuthFileTask(_))
    ));
}

#[test]
fn authentication_shape_rejects_api_key_and_missing_token_fields() {
    // Arrange
    let fixtures = [
        json!({ "auth_mode": "api_key", "OPENAI_API_KEY": "key" }),
        json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null }),
        json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {
                "account_id": "account-1", "id_token": id_token(false) } }),
        json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {
                "access_token": "", "account_id": "account-1" } }),
        json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {
                "access_token": "token", "account_id": "" } }),
        json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {
                "access_token": "token", "id_token": id_token_with_account(false, None) } }),
        json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {
                "access_token": "token", "account_id": "account-1" } }),
    ];

    // Act
    let errors = fixtures.map(|fixture| {
        serde_json::from_value::<CodexAuthFile>(fixture)
            .expect("fixture should decode")
            .chatgpt_auth()
            .err()
            .expect("fixture should be rejected")
    });

    // Assert
    assert!(matches!(errors[0], CodexClientError::ChatGptLoginRequired));
    assert!(
        errors[1..]
            .iter()
            .all(|error| matches!(error, CodexClientError::MissingAuthField(_)))
    );

    let invalid_headers = [
        CodexAuth {
            access_token: "invalid\naccess".to_string(),
            account_id: "account-1".to_string(),
            is_fedramp_account: false,
        },
        CodexAuth {
            access_token: "access-token".to_string(),
            account_id: "invalid\naccount".to_string(),
            is_fedramp_account: false,
        },
    ]
    .map(|auth| auth.headers());
    assert!(
        invalid_headers
            .iter()
            .all(|result| matches!(result, Err(CodexClientError::InvalidAuthHeader)))
    );
}

#[test]
fn authentication_rejects_malformed_id_tokens() {
    // Arrange
    let mut malformed = valid_auth();
    malformed["tokens"]["id_token"] = json!("malformed");

    // Act
    let error = serde_json::from_value::<CodexAuthFile>(malformed)
        .expect("authentication fixture should decode")
        .chatgpt_auth()
        .err();

    // Assert
    assert!(matches!(error, Some(CodexClientError::InvalidIdToken)));

    let mut invalid_base64 = valid_auth();
    invalid_base64["tokens"]["id_token"] = json!("header.%%%.signature");
    let error = serde_json::from_value::<CodexAuthFile>(invalid_base64)
        .expect("authentication fixture should decode")
        .chatgpt_auth()
        .err();
    assert!(matches!(error, Some(CodexClientError::InvalidIdToken)));
}
