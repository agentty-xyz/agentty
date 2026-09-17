use std::ffi::OsString;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use rustix::fs::{FileType, Mode, OFlags};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
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
    if let Some(codex_home) = lookup("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(codex_home).join("auth.json"));
    }
    let home = lookup("HOME")
        .filter(|value| !value.is_empty())
        .ok_or(CodexClientError::AuthFileUnavailable)?;

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
    pub(super) fn account_fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.account_id.as_bytes()))
    }

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
#[path = "auth_test.rs"]
mod tests;
