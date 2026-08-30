use std::ffi::OsString;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use rustix::fs::{FileType, Mode, OFlags};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};

use super::error::CodexClientError;

pub(super) const ACCOUNT_ID_HEADER: &str = "ChatGPT-Account-Id";
pub(super) const AUTH_FILE_LIMIT_BYTES: usize = 64 * 1024;
pub(super) const AUTH_FILE_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
pub(super) const FEDRAMP_HEADER: &str = "X-OpenAI-Fedramp";
const ORIGINATOR_HEADER: &str = "originator";
pub(super) const ORIGINATOR_VALUE: &str = "ag-harness";

pub(super) async fn request_auth(configured: Option<&Path>) -> Result<CodexAuth, CodexClientError> {
    let path = resolve_auth_file(configured, environment_variable)?;

    read_chatgpt_auth(&path).await
}

pub(super) fn environment_variable(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

pub(super) fn resolve_auth_file(
    configured: Option<&Path>,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, CodexClientError> {
    if let Some(configured) = configured {
        return Ok(configured.to_path_buf());
    }
    if let Some(codex_home) = lookup("CODEX_HOME") {
        return Ok(PathBuf::from(codex_home).join("auth.json"));
    }
    let home = lookup("HOME").ok_or(CodexClientError::AuthFileUnavailable)?;

    Ok(PathBuf::from(home).join(".codex/auth.json"))
}

#[derive(Deserialize)]
pub(super) struct CodexAuthFile {
    auth_mode: Option<String>,
    tokens: Option<CodexAuthTokens>,
}

impl CodexAuthFile {
    pub(super) fn chatgpt_auth(self) -> Result<CodexAuth, CodexClientError> {
        if self.auth_mode.as_deref() != Some("chatgpt") {
            return Err(CodexClientError::ChatGptLoginRequired);
        }
        let tokens = self
            .tokens
            .ok_or(CodexClientError::MissingAuthField("tokens"))?;
        let access_token = required_auth_field(tokens.access_token, "tokens.access_token")?;
        let id_token = required_auth_field(tokens.id_token, "tokens.id_token")?;
        let id_token_auth = id_token_auth(&id_token)?;
        let account_id = optional_auth_field(tokens.account_id)
            .or_else(|| optional_auth_field(id_token_auth.chatgpt_account_id))
            .ok_or(CodexClientError::MissingAuthField(
                "tokens.account_id or ID-token chatgpt_account_id",
            ))?;

        Ok(CodexAuth {
            access_token,
            account_id,
            is_fedramp_account: id_token_auth.chatgpt_account_is_fedramp,
        })
    }
}

#[derive(Deserialize)]
struct CodexAuthTokens {
    access_token: Option<String>,
    account_id: Option<String>,
    id_token: Option<String>,
}

pub(super) struct CodexAuth {
    pub(super) access_token: String,
    pub(super) account_id: String,
    pub(super) is_fedramp_account: bool,
}

impl CodexAuth {
    pub(super) fn headers(&self) -> Result<HeaderMap, CodexClientError> {
        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", self.access_token))
            .map_err(|_| CodexClientError::InvalidAuthHeader)?;
        bearer.set_sensitive(true);
        let mut account_id = HeaderValue::from_str(&self.account_id)
            .map_err(|_| CodexClientError::InvalidAuthHeader)?;
        account_id.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(ACCOUNT_ID_HEADER, account_id);
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ORIGINATOR_HEADER,
            HeaderValue::from_static(ORIGINATOR_VALUE),
        );
        if self.is_fedramp_account {
            headers.insert(FEDRAMP_HEADER, HeaderValue::from_static("true"));
        }

        Ok(headers)
    }
}

#[derive(Deserialize)]
struct IdTokenClaims {
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<IdTokenAuthClaims>,
}

#[derive(Default, Deserialize)]
struct IdTokenAuthClaims {
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    chatgpt_account_is_fedramp: bool,
}

fn required_auth_field(
    value: Option<String>,
    field: &'static str,
) -> Result<String, CodexClientError> {
    optional_auth_field(value).ok_or(CodexClientError::MissingAuthField(field))
}

fn optional_auth_field(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn id_token_auth(id_token: &str) -> Result<IdTokenAuthClaims, CodexClientError> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or(CodexClientError::InvalidIdToken)?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| CodexClientError::InvalidIdToken)?;
    let claims: IdTokenClaims =
        serde_json::from_slice(&payload).map_err(|_| CodexClientError::InvalidIdToken)?;

    Ok(claims.auth.unwrap_or_default())
}

pub(super) async fn read_chatgpt_auth(path: &Path) -> Result<CodexAuth, CodexClientError> {
    let path = path.to_path_buf();
    let file = spawn_auth_file_task(move || open_auth_file(&path)).await?;
    let bytes = read_auth_file(tokio::fs::File::from_std(file)).await?;
    let auth: CodexAuthFile =
        serde_json::from_slice(&bytes).map_err(CodexClientError::ParseAuth)?;

    auth.chatgpt_auth()
}

async fn spawn_auth_file_task(
    operation: impl FnOnce() -> Result<std::fs::File, CodexClientError> + Send + 'static,
) -> Result<std::fs::File, CodexClientError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(CodexClientError::AuthFileTask)?
}

fn open_auth_file(path: &Path) -> Result<std::fs::File, CodexClientError> {
    let descriptor = rustix::fs::open(path, AUTH_FILE_OPEN_FLAGS, Mode::empty())
        .map_err(std::io::Error::from)
        .map_err(CodexClientError::ReadAuth)?;
    let metadata = rustix::fs::fstat(&descriptor)
        .map_err(std::io::Error::from)
        .map_err(CodexClientError::ReadAuth)?;
    if !FileType::from_raw_mode(metadata.st_mode).is_file() {
        return Err(CodexClientError::AuthFileNotRegular);
    }

    Ok(std::fs::File::from(descriptor))
}

async fn read_auth_file(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, CodexClientError> {
    let mut bytes = Vec::new();
    let read_limit = u64::try_from(AUTH_FILE_LIMIT_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut reader = reader.take(read_limit);
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(CodexClientError::ReadAuth)?;
    if bytes.len() > AUTH_FILE_LIMIT_BYTES {
        return Err(CodexClientError::AuthFileTooLarge);
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
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
    use super::*;

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
    }
}
