//! External-consumer checks for `OpenAI` models billed to a `ChatGPT` plan:
//! the router asks the host credential for a bearer token before every
//! Responses request, and keeps a session's requests on its cached prefix.

use std::error::Error;
use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use ag_router::{
    Completion, CredentialSource, ErrorClass, JsonSchemaFormat, ModelError, ModelMessage,
    ModelRequest, OutputSchema, Router, ToolDefinition, preset,
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
impl CredentialSource for ScriptedCredential {
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

fn router(server: &MockServer, credential: Arc<ScriptedCredential>) -> Router {
    Router::new([preset::chatgpt(
        format!("{}/v1", server.uri()),
        credential,
        "gpt-6-luna",
    )])
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
    );

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

/// Returns a stream answering with `reasoning` and one `read` call.
fn call_stream(reasoning: &str, call_id: &str, file: &str) -> String {
    stream(&[
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": reasoning,
                "summary": [],
                "encrypted_content": format!("gAAAA-{reasoning}")
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "id": format!("fc_{call_id}"),
                "call_id": call_id,
                "name": "read",
                "arguments": json!({"path": file}).to_string(),
                "status": "completed"
            }
        }),
        json!({
            "type": "response.completed",
            "response": {"id": "resp_calls", "model": "gpt-6-luna", "output": [], "status": "completed"}
        }),
    ])
}

/// Returns a request of the session keyed `ag-session-1` over `messages`,
/// advertising one `read` tool.
fn session_request(messages: Vec<ModelMessage>) -> Result<ModelRequest, Box<dyn Error>> {
    let format = request()?
        .response_format
        .ok_or("response format expected")?;
    let read = ToolDefinition::new(
        "read",
        "Read one file.",
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    );
    let mut request = ModelRequest::chat("chatgpt/gpt-6-luna", messages, vec![read], format);
    request.options.prompt_cache_key = Some("ag-session-1".to_string());

    Ok(request)
}

/// Appends the turn of `completion`'s calls, each answered with its path.
fn answer_calls(
    messages: &mut Vec<ModelMessage>,
    completion: Completion,
) -> Result<(), Box<dyn Error>> {
    let Completion::ToolCalls { calls, .. } = completion else {
        return Err("tool calls expected".into());
    };
    let results = calls
        .iter()
        .map(|call| ModelMessage::ToolResult {
            call_id: call.id().to_string(),
            content: format!("contents of {}", call.arguments_json()),
            name: call.name().to_string(),
        })
        .collect::<Vec<_>>();
    messages.push(ModelMessage::AssistantToolCalls(calls));
    messages.extend(results);

    Ok(())
}

#[tokio::test]
async fn keeps_each_session_request_a_byte_prefix_of_the_next() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    for (reasoning, call_id, file) in [("rs_1", "call_1", "a.txt"), ("rs_2", "call_2", "b.txt")] {
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(call_stream(reasoning, call_id, file), "text/event-stream"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(completed_stream(), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let router = router(&server, ScriptedCredential::new(&["plan-token"; 3]));
    let mut messages = vec![ModelMessage::User("Extract the name".to_string())];

    // Act
    let first = router.execute(session_request(messages.clone())?).await?;
    answer_calls(&mut messages, first)?;
    let second = router.execute(session_request(messages.clone())?).await?;
    answer_calls(&mut messages, second)?;
    router.execute(session_request(messages)?).await?;

    // Assert
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    assert_eq!(requests.len(), 3);
    // One session routes every request to the machine holding its prefix.
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("session-id")
            .is_some_and(|value| value == "ag-session-1")
    }));
    // Everything up to the end of a request's input, which the model field
    // follows, is sent again byte for byte, so the cached prefix holds.
    for pair in requests.windows(2) {
        let earlier = std::str::from_utf8(&pair[0].body)?;
        let later = std::str::from_utf8(&pair[1].body)?;
        let input_end = earlier
            .rfind(r#"],"model":"#)
            .ok_or("the model field follows the input")?;
        assert!(later.starts_with(&earlier[..input_end]));
        assert!(later.len() > earlier.len());
    }

    Ok(())
}

#[tokio::test]
async fn reports_a_credential_failure_without_sending_a_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    let router = router(&server, ScriptedCredential::new(&[]));

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

#[tokio::test]
async fn rejects_an_output_bound_without_sending_a_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    let router = router(&server, ScriptedCredential::new(&["plan-token-1"]));
    let mut bounded = request()?;
    bounded.options.max_output_tokens = NonZeroU32::new(512);

    // Act
    let Err(error) = router.execute(bounded).await else {
        return Err("the request must fail".into());
    };

    // Assert
    assert!(matches!(error, ModelError::UnsupportedGeneration { .. }));
    assert_eq!(error.error_class(), ErrorClass::UnsupportedOutput);
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    assert!(requests.is_empty());

    Ok(())
}
