//! Server unit tests: malformed input, request dispatch and error mapping,
//! concurrent runs, cancellation, shutdown, and the end of input, against a
//! scripted OpenAI-compatible provider.

use std::fmt::Write as _;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, UNIX_EPOCH};
use std::{env, io};

use ag_harness::catalog::ModelId;
use ag_harness::event::RunEvent;
use ag_harness::model::ReasoningEffort;
use ag_harness::provider::{GPT_6_LUNA, ModelProvider};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader, DuplexStream, Lines,
};
use tokio::task::{JoinHandle, LocalSet};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{event_line, serve};
use crate::serve::protocol::{
    INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, SERVER_ERROR,
};

/// Longest wait for a message the server should write, so a regression
/// fails with its assertion instead of hanging on a delayed provider.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

type Environment = Box<dyn FnMut(&str) -> Result<String, env::VarError> + Send>;

/// Environment configuring only the OpenAI-compatible provider at
/// `base_url`, plus `extra` variables.
fn environment(base_url: &str, extra: &[(&'static str, &'static str)]) -> Environment {
    let base_url = base_url.to_string();
    let extra = extra.to_vec();

    Box::new(move |name| match name {
        "OPENAI_COMPATIBLE_BASE_URL" => Ok(base_url.clone()),
        "OPENAI_COMPATIBLE_CONTEXT_WINDOW" => Ok("40960".to_string()),
        _ => extra
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
            .ok_or(env::VarError::NotPresent),
    })
}

fn message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"message": {"type": "string"}},
        "required": ["message"],
        "additionalProperties": false
    })
}

fn local_model() -> Value {
    json!({"id": "openai-compatible/local"})
}

fn answer(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"message": message}).to_string()}
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
    }))
}

/// Starts a provider that answers every request with `response`.
async fn provider(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(response)
        .mount(&server)
        .await;

    server
}

/// Starts a provider that answers prompts containing `Wait` only after the
/// test ends, and every other prompt with `response` at once.
async fn waiting_provider(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("Wait"))
        .respond_with(answer("late").set_delay(Duration::from_secs(600)))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(response)
        .mount(&server)
        .await;

    server
}

fn request(id: i64, method: &str, params: Value) -> Value {
    let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method});
    request["params"] = params;

    request
}

fn error_kind(response: &Value) -> &Value {
    &response["error"]["data"]["kind"]
}

fn is_completed(message: &Value) -> bool {
    message["method"] == "run/event" && message["params"]["event"]["kind"]["type"] == "completed"
}

/// Serves `input` to the end and returns every written message.
async fn exchange(input: &str, environment: Environment) -> (Vec<Value>, io::Result<()>) {
    let mut output = Vec::new();
    let served = serve(
        input.as_bytes(),
        &mut output,
        environment,
        ReasoningEffort::Low,
    )
    .await;
    let messages = String::from_utf8(output)
        .expect("output should be UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each output line should be JSON"))
        .collect();

    (messages, served)
}

fn lines(messages: &[Value]) -> String {
    messages.iter().fold(String::new(), |mut lines, message| {
        let _ = writeln!(lines, "{message}");

        lines
    })
}

/// A client driving a server task through in-memory pipes. The server runs
/// as a local task, since an open `Session` is not `Sync`; tests using it
/// run inside a [`LocalSet`].
struct Client {
    input: Option<DuplexStream>,
    /// Messages read while waiting for another one.
    pending: Vec<Value>,
    output: Lines<BufReader<DuplexStream>>,
    server: JoinHandle<io::Result<()>>,
}

impl Client {
    fn start(environment: Environment) -> Self {
        let (input, server_input) = tokio::io::duplex(1 << 16);
        let (server_output, output) = tokio::io::duplex(1 << 20);
        let server = tokio::task::spawn_local(serve(
            BufReader::new(server_input),
            server_output,
            environment,
            ReasoningEffort::Low,
        ));

        Self {
            input: Some(input),
            pending: Vec::new(),
            output: BufReader::new(output).lines(),
            server,
        }
    }

    async fn send(&mut self, message: &Value) {
        self.send_raw(&format!("{message}\n")).await;
    }

    /// Writes `text` as is, which may end mid-line.
    async fn send_raw(&mut self, text: &str) {
        let input = self.input.as_mut().expect("input should be open");
        input
            .write_all(text.as_bytes())
            .await
            .expect("server should read input");
    }

    async fn read(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(READ_TIMEOUT, self.output.next_line())
            .await
            .expect("server should write within the read timeout")
            .expect("output should be readable")?;

        Some(serde_json::from_str(&line).expect("output lines should be JSON"))
    }

    /// Returns the first message `matches` accepts, keeping the others.
    async fn wait(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self.pending.iter().position(&matches) {
            return self.pending.remove(index);
        }
        loop {
            let message = self.read().await.expect("server should answer");
            if matches(&message) {
                return message;
            }
            self.pending.push(message);
        }
    }

    async fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&request(id, method, params)).await;

        self.wait(|message| message["id"] == id).await
    }

    /// Waits for the completed event of `run`.
    async fn completed(&mut self, run: &Value) -> Value {
        self.wait(|message| is_completed(message) && message["params"]["event"]["run"] == *run)
            .await
    }

    /// Closes the input and returns the remaining messages and the
    /// server's result.
    async fn finish(mut self) -> (Vec<Value>, io::Result<()>) {
        self.input = None;
        let mut messages = std::mem::take(&mut self.pending);
        while let Some(message) = self.read().await {
            messages.push(message);
        }

        (
            messages,
            tokio::time::timeout(READ_TIMEOUT, self.server)
                .await
                .expect("server should stop within the read timeout")
                .expect("server task should not panic"),
        )
    }
}

/// Output that accepts `writes` writes, then rejects every later one.
struct ClosedOutput {
    writes: usize,
}

impl AsyncWrite for ClosedOutput {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.writes == 0 {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        self.writes -= 1;

        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn malformed_lines_are_answered_without_stopping_the_server() {
    // Arrange
    let input = concat!(
        "not json\n",
        "\n",
        "[1]\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"session/usage\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/delete\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"run/cancel\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/usage\",\"params\":{\"session\":\"s\"\
         }}\n",
    );

    // Act
    let (messages, served) = exchange(input, environment("http://127.0.0.1:9", &[])).await;

    // Assert
    assert!(served.is_ok());
    let codes: Vec<&Value> = messages
        .iter()
        .map(|message| &message["error"]["code"])
        .collect();
    assert_eq!(
        codes,
        [
            &json!(PARSE_ERROR),
            &json!(INVALID_REQUEST),
            &json!(METHOD_NOT_FOUND),
            &json!(INVALID_PARAMS),
            &json!(SERVER_ERROR),
        ]
    );
    assert_eq!(messages[0]["id"], Value::Null);
    assert_eq!(messages[3]["id"], json!(2));
    assert_eq!(error_kind(&messages[4]), &json!("not_found"));
    assert_eq!(messages[4]["error"]["data"]["retryable"], json!(false));
}

#[tokio::test]
async fn sessions_report_transcripts_and_usage() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = provider(answer("hello")).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let schema = json!({"type": "json", "schema": message_schema()});

            // Act
            let created = client
                .call(
                    1,
                    "session/create",
                    json!({
                        "session": "first",
                        "model": local_model(),
                        "output": schema,
                        "system_prompt": "Be brief."
                    }),
                )
                .await;
            let generated = client
                .call(
                    2,
                    "session/create",
                    json!({"model": local_model(), "output": schema}),
                )
                .await;
            let duplicate = client
                .call(
                    3,
                    "session/create",
                    json!({"session": "first", "model": local_model(), "output": schema}),
                )
                .await;
            let first_run = client
                .call(
                    4,
                    "run/start",
                    json!({
                        "session": "first",
                        "input": "Hello",
                        "instructions": "Answer in one word.",
                        "output": schema,
                        "generation": {"max_output_tokens": 64},
                        "limits": {"max_model_requests": 2, "max_wall_time_ms": 60_000}
                    }),
                )
                .await;
            let second_session = generated["result"]["session"].clone();
            let second_run = client
                .call(
                    5,
                    "run/start",
                    json!({"session": second_session, "input": "Hi"}),
                )
                .await;
            let first_completed = client.completed(&first_run["result"]["run"]).await;
            let second_completed = client.completed(&second_run["result"]["run"]).await;
            let transcript = client
                .call(
                    6,
                    "session/transcript",
                    json!({"session": "first", "range": {"type": "last", "count": 1}}),
                )
                .await;
            let usage = client
                .call(7, "session/usage", json!({"session": "first"}))
                .await;
            client.send(&request(8, "shutdown", json!({}))).await;
            let (rest, served) = client.finish().await;

            // Assert
            assert_eq!(created["result"], json!({"session": "first"}));
            assert_eq!(second_session.as_str().map(str::len), Some(32));
            assert_eq!(error_kind(&duplicate), &json!("conflict"));
            for completed in [&first_completed, &second_completed] {
                assert_eq!(completed["params"]["event"]["kind"]["error"], Value::Null);
            }
            assert_eq!(first_completed["params"]["session"], json!("first"));
            assert_eq!(
                first_completed["params"]["event"]["kind"]["usage"]["input"],
                json!(5)
            );
            assert_eq!(
                first_completed["params"]["event"]["kind"]["cost"],
                Value::Null
            );
            let turns = transcript["result"]["turns"]
                .as_array()
                .expect("transcript should list turns");
            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0]["instructions"], json!("Answer in one word."));
            assert_eq!(usage["result"]["turns"], json!(1));
            assert_eq!(usage["result"]["usage"]["input"], json!(5));
            assert!(served.is_ok());
            assert_eq!(rest.last().map(|message| &message["id"]), Some(&json!(8)));
        })
        .await;
}

#[tokio::test]
async fn run_events_follow_the_run_start_response_in_order() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = provider(answer("ordered")).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let output = json!({"type": "json", "schema": message_schema()});
            client
                .call(
                    1,
                    "session/create",
                    json!({"session": "s", "model": local_model(), "output": output}),
                )
                .await;

            // Act
            client
                .send(&request(
                    2,
                    "run/start",
                    json!({"session": "s", "input": "Hi"}),
                ))
                .await;
            let mut messages = Vec::new();
            while !messages.last().is_some_and(is_completed) {
                messages.push(client.read().await.expect("server should write the run"));
            }
            let (_, served) = client.finish().await;

            // Assert
            assert_eq!(messages[0]["id"], json!(2));
            let run = &messages[0]["result"]["run"];
            let events: Vec<&Value> = messages[1..]
                .iter()
                .map(|message| &message["params"]["event"])
                .collect();
            assert!(events.iter().all(|event| event["run"] == *run));
            let sequence: Vec<u64> = events
                .iter()
                .filter_map(|event| event["seq"].as_u64())
                .collect();
            assert_eq!(sequence, (0..events.len() as u64).collect::<Vec<_>>());
            assert_eq!(events[0]["kind"]["type"], json!("started"));
            assert!(events.iter().any(|event| event["kind"]["item"]["output"]
                == json!({"type": "json", "value": {"message": "ordered"}})));
            assert!(served.is_ok());
        })
        .await;
}

#[tokio::test]
async fn cancel_and_end_of_input_stop_runs_in_flight() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = provider(answer("late").set_delay(Duration::from_secs(600))).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let output = json!({"type": "json", "schema": message_schema()});
            client
                .call(
                    1,
                    "session/create",
                    json!({"session": "s", "model": local_model(), "output": output}),
                )
                .await;

            // Act
            let cancelled_run = client
                .call(2, "run/start", json!({"session": "s", "input": "Wait"}))
                .await;
            let run = cancelled_run["result"]["run"].clone();
            let cancel = client.call(3, "run/cancel", json!({"run": run})).await;
            let cancelled = client.completed(&run).await;
            let unknown = client
                .call(4, "run/cancel", json!({"run": "missing"}))
                .await;
            let open_run = client
                .call(
                    5,
                    "run/start",
                    json!({"session": "s", "input": "Wait again"}),
                )
                .await;
            let cancel_finished = client.call(6, "run/cancel", json!({"run": run})).await;
            let (rest, served) = client.finish().await;

            // Assert
            assert_eq!(cancel["result"], json!({}));
            assert_eq!(
                cancelled["params"]["event"]["kind"]["error"]["kind"],
                json!("cancelled")
            );
            assert_eq!(error_kind(&unknown), &json!("not_found"));
            assert_eq!(cancel_finished["result"], json!({}));
            let stopped = rest
                .iter()
                .find(|message| is_completed(message))
                .expect("the end of input should complete the open run");
            assert_eq!(stopped["params"]["event"]["run"], open_run["result"]["run"]);
            assert_eq!(
                stopped["params"]["event"]["kind"]["error"]["kind"],
                json!("cancelled")
            );
            assert!(served.is_ok());
        })
        .await;
}

#[tokio::test]
async fn client_notifications_are_executed_without_answers() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = waiting_provider(answer("unused")).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let output = json!({"type": "json", "schema": message_schema()});
            client
                .call(
                    1,
                    "session/create",
                    json!({"session": "s", "model": local_model(), "output": output}),
                )
                .await;
            let started = client
                .call(2, "run/start", json!({"session": "s", "input": "Wait"}))
                .await;
            let run = started["result"]["run"].clone();

            // Act
            client
                .send(&json!({"jsonrpc": "2.0", "method": "session/delete"}))
                .await;
            client
                .send(&json!({"jsonrpc": "2.0", "method": "run/cancel", "params": {"run": run}}))
                .await;
            let cancelled = client.completed(&run).await;
            client
                .send(&json!({"jsonrpc": "2.0", "method": "shutdown"}))
                .await;
            let (rest, served) = client.finish().await;

            // Assert
            assert_eq!(
                cancelled["params"]["event"]["kind"]["error"]["kind"],
                json!("cancelled")
            );
            assert!(
                rest.iter().all(|message| message.get("id").is_none()),
                "notifications should get no answer: {rest:?}"
            );
            assert!(served.is_ok());
        })
        .await;
}

#[tokio::test]
async fn a_second_run_on_a_busy_session_completes_as_busy() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = waiting_provider(answer("unused")).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let output = json!({"type": "json", "schema": message_schema()});
            client
                .call(
                    1,
                    "session/create",
                    json!({"session": "s", "model": local_model(), "output": output}),
                )
                .await;
            let waiting = client
                .call(2, "run/start", json!({"session": "s", "input": "Wait"}))
                .await;
            client
                .wait(|message| {
                    message["params"]["event"]["run"] == waiting["result"]["run"]
                        && message["params"]["event"]["kind"]["type"] == "started"
                })
                .await;

            // Act
            let second = client
                .call(3, "run/start", json!({"session": "s", "input": "Hello"}))
                .await;
            let second_completed = client.completed(&second["result"]["run"]).await;
            let (_, served) = client.finish().await;

            // Assert
            assert_eq!(
                second_completed["params"]["event"]["kind"]["error"]["kind"],
                json!("busy")
            );
            assert_eq!(
                second_completed["params"]["event"]["kind"]["error"]["retryable"],
                json!(true)
            );
            assert!(served.is_ok());
        })
        .await;
}

#[tokio::test]
async fn a_request_split_by_run_events_is_answered_whole() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = waiting_provider(answer("unused")).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let output = json!({"type": "json", "schema": message_schema()});
            client
                .call(
                    1,
                    "session/create",
                    json!({"session": "s", "model": local_model(), "output": output}),
                )
                .await;
            let started = client
                .call(
                    2,
                    "run/start",
                    json!({"session": "s", "input": "Wait", "limits": {"max_wall_time_ms": 200}}),
                )
                .await;
            let run = started["result"]["run"].clone();
            client
                .wait(|message| {
                    message["params"]["event"]["run"] == run
                        && message["params"]["event"]["kind"]["type"] == "started"
                })
                .await;

            // Act
            client
                .send_raw("{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":")
                .await;
            let completed = client.completed(&run).await;
            client
                .send_raw("\"session/usage\",\"params\":{\"session\":\"s\"}}\n")
                .await;
            let usage = client.wait(|message| message["id"] == 3).await;
            let (_, served) = client.finish().await;

            // Assert
            assert_eq!(
                completed["params"]["event"]["kind"]["error"]["kind"],
                json!("budget_exhausted")
            );
            assert_eq!(usage["result"]["turns"], json!(1));
            assert!(served.is_ok());
        })
        .await;
}

#[tokio::test]
async fn shutdown_answers_after_the_last_event_of_every_run() {
    LocalSet::new()
        .run_until(async {
            // Arrange
            let provider = provider(answer("late").set_delay(Duration::from_secs(600))).await;
            let mut client = Client::start(environment(&provider.uri(), &[]));
            let output = json!({"type": "json", "schema": message_schema()});
            client
                .call(
                    1,
                    "session/create",
                    json!({"session": "s", "model": local_model(), "output": output}),
                )
                .await;
            client
                .call(2, "run/start", json!({"session": "s", "input": "Wait"}))
                .await;

            // Act
            client.send(&request(3, "shutdown", json!({}))).await;
            let shutdown = client.wait(|message| message["id"] == 3).await;
            let pending = std::mem::take(&mut client.pending);
            let (rest, served) = client.finish().await;

            // Assert
            assert_eq!(shutdown["result"], json!({}));
            assert!(pending.iter().any(is_completed));
            assert_eq!(rest, Vec::<Value>::new());
            assert!(served.is_ok());
        })
        .await;
}

#[tokio::test]
async fn model_and_output_failures_map_to_classified_errors() {
    // Arrange
    let output = json!({"type": "json", "schema": message_schema()});
    let create = |id, model: Value| {
        request(
            id,
            "session/create",
            json!({"model": model, "output": output}),
        )
    };
    let input = lines(&[
        create(1, json!({"role": "smart"})),
        create(2, json!({"id": "unknown-model"})),
        create(3, json!({"id": "muse/muse-custom"})),
        request(
            4,
            "session/create",
            json!({"model": local_model(), "output": {"type": "json", "schema": {"type": 5}}}),
        ),
        request(
            5,
            "session/resume",
            json!({"session": "missing", "model": local_model()}),
        ),
        request(6, "run/start", json!({"session": "missing", "input": "Hi"})),
        request(7, "session/transcript", json!({"session": "missing"})),
        request(8, "session/resume", json!({"session": "missing"})),
    ]);

    // Act
    let (messages, served) = exchange(&input, environment("http://127.0.0.1:9", &[])).await;

    // Assert
    assert!(served.is_ok());
    let kinds: Vec<&Value> = messages.iter().map(error_kind).collect();
    assert_eq!(
        kinds,
        [
            &json!("config"),
            &json!("config"),
            &json!("config"),
            &json!("config"),
            &json!("not_found"),
            &json!("not_found"),
            &json!("not_found"),
            &json!("not_found"),
        ]
    );
    assert!(
        messages[1]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unknown-model"))
    );
}

#[tokio::test]
async fn catalog_roles_and_identifiers_select_configured_models() {
    // Arrange
    let output = json!({"type": "json", "schema": message_schema()});
    let cataloged = ModelId::cataloged(ModelProvider::OpenAi, GPT_6_LUNA);
    let input = lines(&[
        request(
            1,
            "session/create",
            json!({"model": {"role": "smart"}, "output": output}),
        ),
        request(
            2,
            "session/create",
            json!({"model": {"id": cataloged.as_str()}, "output": output}),
        ),
    ]);
    let broken = Box::new(|name: &str| match name {
        "OPENAI_API_KEY" => Err(env::VarError::NotUnicode("\u{fffd}".into())),
        _ => Err(env::VarError::NotPresent),
    });

    // Act
    let (configured, configured_served) = exchange(
        &input,
        environment("http://127.0.0.1:9", &[("OPENAI_API_KEY", "test-key")]),
    )
    .await;
    let (failed, failed_served) = exchange(
        &lines(&[request(
            1,
            "session/create",
            json!({"model": {"role": "smart"}, "output": output}),
        )]),
        broken,
    )
    .await;

    // Assert
    assert!(configured_served.is_ok() && failed_served.is_ok());
    assert!(configured[0]["result"]["session"].is_string());
    assert!(configured[1]["result"]["session"].is_string());
    assert_eq!(error_kind(&failed[0]), &json!("config"));
}

#[tokio::test]
async fn sqlite_sessions_resume_in_a_later_server() {
    // Arrange
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let store = json!({"type": "sqlite", "path": storage.path().join("serve.db")});
    let output = json!({"type": "json", "schema": message_schema()});
    let create = lines(&[request(
        1,
        "session/create",
        json!({"session": "kept", "model": local_model(), "output": output, "store": store}),
    )]);
    let resume = |id| {
        request(
            id,
            "session/resume",
            json!({"session": "kept", "model": local_model(), "store": store}),
        )
    };
    let resume_stored = |session: &str| {
        request(
            1,
            "session/resume",
            json!({"session": session, "store": store}),
        )
    };

    // Act
    let (created, created_served) = exchange(&create, environment("http://127.0.0.1:9", &[])).await;
    let (resumed, resumed_served) = exchange(
        &lines(&[resume(1), resume(2)]),
        environment("http://127.0.0.1:9", &[]),
    )
    .await;
    let (stored, stored_served) = exchange(
        &lines(&[resume_stored("kept")]),
        environment("http://127.0.0.1:9", &[]),
    )
    .await;
    let (missing, missing_served) = exchange(
        &lines(&[resume_stored("missing")]),
        environment("http://127.0.0.1:9", &[]),
    )
    .await;

    // Assert
    assert!(created_served.is_ok() && resumed_served.is_ok());
    assert!(stored_served.is_ok() && missing_served.is_ok());
    assert_eq!(created[0]["result"], json!({"session": "kept"}));
    assert_eq!(resumed[0]["result"], json!({"session": "kept"}));
    assert_eq!(error_kind(&resumed[1]), &json!("conflict"));
    assert_eq!(stored[0]["result"], json!({"session": "kept"}));
    assert_eq!(error_kind(&missing[0]), &json!("not_found"));
}

#[tokio::test]
async fn a_failed_write_stops_the_server_with_its_error() {
    // Arrange
    let input = "not json\n";

    // Act
    let served = serve(
        input.as_bytes(),
        ClosedOutput { writes: 0 },
        environment("http://127.0.0.1:9", &[]),
        ReasoningEffort::Low,
    )
    .await;

    // Assert
    assert_eq!(
        served.map_err(|error| error.kind()),
        Err(io::ErrorKind::BrokenPipe)
    );
}

#[tokio::test]
async fn a_failed_event_write_still_stops_every_run() {
    // Arrange
    let provider = waiting_provider(answer("unused")).await;
    let output = json!({"type": "json", "schema": message_schema()});
    let input = lines(&[
        request(
            1,
            "session/create",
            json!({"session": "s", "model": local_model(), "output": output}),
        ),
        request(2, "run/start", json!({"session": "s", "input": "Wait"})),
    ]);

    // Act
    let served = tokio::time::timeout(
        READ_TIMEOUT,
        serve(
            input.as_bytes(),
            ClosedOutput { writes: 2 },
            environment(&provider.uri(), &[]),
            ReasoningEffort::Low,
        ),
    )
    .await
    .expect("a failed write should not leave a run hanging");

    // Assert
    assert_eq!(
        served.map_err(|error| error.kind()),
        Err(io::ErrorKind::BrokenPipe)
    );
}

#[test]
fn an_event_time_before_the_epoch_is_sent_as_the_epoch() {
    // Arrange
    let mut event: RunEvent = serde_json::from_value(json!({
        "at": 1_700_000_000_000_u64,
        "kind": {
            "type": "completed",
            "cost": null,
            "error": null,
            "usage": {
                "input": 1, "output": 1, "cache_read": 0, "cache_write": null,
                "reasoning": 0, "unreported_requests": 0
            }
        },
        "run": "run-1",
        "seq": 3
    }))
    .expect("completed event should decode");
    let on_time = event_line("s".to_string(), event.clone());
    event.at = UNIX_EPOCH - Duration::from_secs(1);

    // Act
    let early = event_line("s".to_string(), event);

    // Assert
    let on_time: Value = serde_json::from_str(&on_time).expect("event line should be JSON");
    let early: Value = serde_json::from_str(&early).expect("event line should be JSON");
    assert_eq!(
        on_time["params"]["event"]["at"],
        json!(1_700_000_000_000_u64)
    );
    assert_eq!(early["params"]["event"]["at"], json!(0));
    assert_eq!(early["params"]["event"]["kind"]["type"], json!("completed"));
    assert_eq!(early["params"]["event"]["seq"], json!(3));
}
