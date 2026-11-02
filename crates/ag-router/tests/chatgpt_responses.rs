//! External-consumer checks for `OpenAI` models billed to a `ChatGPT` plan:
//! the router asks the host credential for a bearer token before every
//! Responses request.

use std::error::Error;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ag_router::{
    BearerCredential, Completion, ErrorClass, JsonSchemaFormat, ModelError, ModelMessage,
    ModelRequest, OutputSchema, Provider, ProviderConfig, Router,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Credential handing out scripted tokens in order, or failing when none
/// remain.
struct ScriptedCredential {
    tokens: Mutex<Vec<&'static str>>,
}

impl ScriptedCredential {
    fn new(tokens: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(tokens.iter().rev().copied().collect()),
        })
    }
}

#[async_trait]
impl BearerCredential for ScriptedCredential {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        self.tokens
            .lock()
            .map_err(|_| "credential lock poisoned")?
            .pop()
            .map(str::to_string)
            .ok_or_else(|| "the ChatGPT sign-in expired".into())
    }
}

fn request() -> Result<ModelRequest, Box<dyn Error>> {
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
        "additionalProperties": false
    }))?;

    Ok(ModelRequest::chat(
        "chatgpt/gpt-6-luna",
        vec![ModelMessage::User("Extract the name".to_string())],
        vec![],
        JsonSchemaFormat::new("person", schema)?,
    ))
}

fn router(server: &MockServer, credential: Arc<ScriptedCredential>) -> Result<Router, ModelError> {
    Router::new([ProviderConfig {
        provider: Provider::ChatGpt,
        api_key: None,
        base_url: format!("{}/v1", server.uri()),
        credential: Some(credential),
    }])
}

/// Serializes `events` as a server-sent event stream.
fn stream(events: &[Value]) -> String {
    events.iter().fold(String::new(), |mut stream, event| {
        let _ = write!(
            stream,
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap_or("")
        );

        stream
    })
}

fn completed_stream() -> String {
    stream(&[
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": r#"{"name":"Ada"}"#, "annotations": []}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {"id": "resp_1", "model": "gpt-6-luna", "output": [], "status": "completed"}
        }),
    ])
}

async fn mount_for_token(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(bearer_token(token))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(completed_stream(), "text/event-stream"),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn authenticates_each_request_with_a_token_from_the_credential() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let server = MockServer::start().await;
    mount_for_token(&server, "plan-token-1").await;
    mount_for_token(&server, "plan-token-2").await;
    let router = router(
        &server,
        ScriptedCredential::new(&["plan-token-1", "plan-token-2"]),
    )?;

    // Act
    let first = router.execute(request()?).await?;
    let second = router.execute(request()?).await?;

    // Assert
    for completion in [first, second] {
        assert!(matches!(
            completion,
            Completion::Output { ref value, .. } if *value == json!({"name": "Ada"})
        ));
    }

    Ok(())
}

#[tokio::test]
async fn reports_a_credential_failure_without_sending_a_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    let router = router(&server, ScriptedCredential::new(&[]))?;

    // Act
    let Err(error) = router.execute(request()?).await else {
        return Err("the request must fail".into());
    };

    // Assert
    assert_eq!(error.error_class(), ErrorClass::Provider);
    assert_eq!(error.http_status(), None);
    assert!(error.to_string().contains("the ChatGPT sign-in expired"));
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    assert!(requests.is_empty());

    Ok(())
}
