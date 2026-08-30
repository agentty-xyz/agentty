use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use crate::OutputSchema;

pub(super) fn person_schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
        "additionalProperties": false
    }))
    .expect("person schema should be valid")
}

pub(super) fn id_token(is_fedramp_account: bool) -> String {
    id_token_with_account(is_fedramp_account, Some("account-1"))
}

pub(super) fn id_token_with_account(is_fedramp_account: bool, account_id: Option<&str>) -> String {
    let payload = json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_account_is_fedramp": is_fedramp_account
        }
    });
    let payload = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).expect("ID token payload should encode"));

    format!("header.{payload}.signature")
}

pub(super) fn auth_with_fedramp(is_fedramp_account: bool) -> Value {
    let mut auth = auth_with_account("account-1", "access-token");
    auth["tokens"]["id_token"] = json!(id_token(is_fedramp_account));

    auth
}

pub(super) fn auth_with_account(account_id: &str, access_token: &str) -> Value {
    json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "access_token": access_token,
            "account_id": account_id,
            "id_token": id_token_with_account(false, Some(account_id))
        }
    })
}

pub(super) fn valid_auth() -> Value {
    auth_with_fedramp(false)
}

pub(super) fn write_auth(directory: &Path, value: &Value) -> PathBuf {
    let path = directory.join("auth.json");
    fs::write(
        &path,
        serde_json::to_vec(value).expect("auth should encode"),
    )
    .expect("auth fixture should be written");

    path
}

pub(super) fn success_sse() -> String {
    concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"{\\\"name\\\":\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"\\\"Ada\\\"}\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{",
        "\"id\":\"response-1\",\"model\":\"gpt-test\",\"status\":\"completed\",",
        "\"usage\":{\"input_tokens\":10,\"output_tokens\":4,\"total_tokens\":14,",
        "\"input_tokens_details\":{\"cached_tokens\":2},",
        "\"output_tokens_details\":{\"reasoning_tokens\":1}}}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string()
}
