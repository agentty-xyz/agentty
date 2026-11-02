use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::{ChatGptCredentials, unix_now};
use crate::chatgpt::error::ChatGptAuthError;
use crate::chatgpt::token::TokenResponse;

fn credentials() -> ChatGptCredentials {
    ChatGptCredentials {
        access_token: "secret-access".to_string(),
        client_id: "client-1".to_string(),
        email: Some("old@example.com".to_string()),
        expires_at: unix_now() + 3600,
        ext_agent_host_id: "urn:uuid:host".to_string(),
        id_token: "secret-id".to_string(),
        refresh_token: "secret-refresh".to_string(),
        scopes: vec![
            "openid".to_string(),
            "chatgpt.tokens.use.direct".to_string(),
        ],
    }
}

fn id_token(claims: &serde_json::Value) -> String {
    format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn grant() -> TokenResponse {
    TokenResponse {
        access_token: "new-access".to_string(),
        expires_in: 3600,
        id_token: None,
        refresh_token: None,
        scope: None,
    }
}

#[test]
fn saves_and_loads_credentials_readable_only_by_the_owner() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("nested").join("chatgpt.json");
    let credentials = credentials();

    // Act
    credentials.save(&path).expect("credentials save");
    let loaded = ChatGptCredentials::load(&path).expect("credentials load");

    // Assert
    assert_eq!(loaded, credentials);
    #[cfg(unix)]
    {
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn reports_missing_unreadable_and_invalid_credential_files() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.json");
    let invalid = directory.path().join("invalid.json");
    fs::write(&invalid, "not json").expect("write invalid file");

    // Act
    let missing_result = ChatGptCredentials::load(&missing);
    let directory_result = ChatGptCredentials::load(directory.path());
    let invalid_result = ChatGptCredentials::load(&invalid);

    // Assert
    assert!(matches!(
        missing_result,
        Err(ChatGptAuthError::NotSignedIn { ref path }) if *path == missing
    ));
    assert!(matches!(directory_result, Err(ChatGptAuthError::Io { .. })));
    assert!(matches!(
        invalid_result,
        Err(ChatGptAuthError::InvalidCredentials { ref path, .. }) if *path == invalid
    ));
}

#[test]
fn save_reports_an_unwritable_parent_directory() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let file = directory.path().join("file");
    fs::write(&file, "").expect("write blocking file");
    let path = file.join("chatgpt.json");

    // Act
    let result = credentials().save(&path);

    // Assert
    assert!(
        matches!(result, Err(ChatGptAuthError::Io { ref path, .. }) if *path == file.join("chatgpt.json"))
    );
}

#[test]
fn is_fresh_only_outside_the_refresh_margin() {
    // Arrange
    let cases = [
        (unix_now() + 3600, true),
        (unix_now() + 60, false),
        (0, false),
    ];

    // Act / Assert
    for (expires_at, fresh) in cases {
        let credentials = ChatGptCredentials {
            expires_at,
            ..credentials()
        };
        assert_eq!(credentials.is_fresh(), fresh, "{expires_at}");
    }
}

#[test]
fn from_grant_keeps_previous_values_the_grant_omits() {
    // Arrange
    let previous = credentials();

    // Act
    let refreshed = ChatGptCredentials::from_grant(
        grant(),
        "client-1".to_string(),
        "urn:uuid:host".to_string(),
        Some(&previous),
    )
    .expect("grant keeps the plan scope");

    // Assert
    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token, previous.refresh_token);
    assert_eq!(refreshed.id_token, previous.id_token);
    assert_eq!(refreshed.scopes, previous.scopes);
    assert_eq!(refreshed.email, previous.email);
    assert!(refreshed.expires_at >= unix_now() + 3599);
}

#[test]
fn from_grant_uses_new_tokens_and_reads_the_id_token_email() {
    // Arrange
    let grant = TokenResponse {
        id_token: Some(id_token(&serde_json::json!({"email": "ada@example.com"}))),
        refresh_token: Some("new-refresh".to_string()),
        scope: Some("openid chatgpt.tokens.use.direct".to_string()),
        ..grant()
    };

    // Act
    let credentials = ChatGptCredentials::from_grant(
        grant,
        "client-2".to_string(),
        "urn:uuid:other".to_string(),
        Some(&credentials()),
    )
    .expect("grant has the plan scope");

    // Assert
    assert_eq!(credentials.client_id, "client-2");
    assert_eq!(credentials.ext_agent_host_id, "urn:uuid:other");
    assert_eq!(credentials.refresh_token, "new-refresh");
    assert_eq!(credentials.email.as_deref(), Some("ada@example.com"));
    assert_eq!(credentials.scopes, ["openid", "chatgpt.tokens.use.direct"]);
}

#[test]
fn from_grant_without_previous_credentials_tolerates_missing_claims() {
    // Arrange
    let cases = [
        "no-dots",
        "header.!!!.signature",
        &id_token(&serde_json::json!({})),
    ];

    for id_token in cases {
        let grant = TokenResponse {
            id_token: Some(id_token.to_string()),
            scope: Some("chatgpt.tokens.use.direct".to_string()),
            ..grant()
        };

        // Act
        let credentials = ChatGptCredentials::from_grant(
            grant,
            "client-1".to_string(),
            "urn:uuid:host".to_string(),
            None,
        )
        .expect("grant has the plan scope");

        // Assert
        assert_eq!(credentials.email, None, "{id_token}");
        assert_eq!(credentials.refresh_token, "", "{id_token}");
    }
}

#[test]
fn from_grant_requires_the_plan_scope() {
    // Arrange
    let narrow = TokenResponse {
        scope: Some("openid profile".to_string()),
        ..grant()
    };

    // Act
    let narrow_result = ChatGptCredentials::from_grant(
        narrow,
        "client-1".to_string(),
        "urn:uuid:host".to_string(),
        Some(&credentials()),
    );
    let unscoped_result = ChatGptCredentials::from_grant(
        grant(),
        "client-1".to_string(),
        "urn:uuid:host".to_string(),
        None,
    );

    // Assert
    assert!(matches!(
        narrow_result,
        Err(ChatGptAuthError::MissingPlanScope)
    ));
    assert!(matches!(
        unscoped_result,
        Err(ChatGptAuthError::MissingPlanScope)
    ));
}

#[test]
fn debug_redacts_every_token() {
    // Arrange
    let credentials = credentials();

    // Act
    let debug = format!("{credentials:?}");

    // Assert
    assert!(debug.contains("client-1"));
    assert!(debug.contains("old@example.com"));
    assert!(!debug.contains("secret"));
}
