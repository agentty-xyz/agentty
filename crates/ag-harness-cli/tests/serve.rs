//! Process-level coverage of the unstable `ag-harness serve` JSON-RPC
//! protocol: a real server process driven over stdio against a scripted
//! OpenAI-compatible provider, with every message checked against the
//! committed protocol schema.

use std::error::Error;
use std::process::Stdio;
use std::time::Duration;

use ag_harness::provider::ModelProvider;
use assert_cmd::cargo::cargo_bin;
use jsonschema::Validator;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROTOCOL_SCHEMA: &str = include_str!("../schema/serve_protocol.json");

type TestResult<T> = Result<T, Box<dyn Error>>;

fn message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"message": {"type": "string"}},
        "required": ["message"],
        "additionalProperties": false
    })
}

fn answer(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"message": message}).to_string()}
        }],
        "usage": {"prompt_tokens": 6, "completion_tokens": 3, "total_tokens": 9}
    }))
}

/// Starts a provider that answers prompts containing `Wait` only after
/// the test ends, and every other prompt at once.
async fn provider() -> MockServer {
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
        .respond_with(answer("hello"))
        .mount(&server)
        .await;

    server
}

fn request(id: i64, method: &str, params: Value) -> Value {
    let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method});
    request["params"] = params;

    request
}

fn is_completed(message: &Value) -> bool {
    message["method"] == "run/event" && message["params"]["event"]["kind"]["type"] == "completed"
}

/// A running `ag-harness serve` process.
struct Server {
    child: Child,
    input: Option<ChildStdin>,
    output: Lines<BufReader<ChildStdout>>,
    /// Messages read while waiting for another one.
    pending: Vec<Value>,
    storage: tempfile::TempDir,
    validator: Validator,
}

impl Server {
    /// Spawns the server with only the OpenAI-compatible provider at
    /// `provider` configured.
    fn spawn(provider: &MockServer) -> TestResult<Self> {
        let storage = tempfile::tempdir()?;
        let mut command = Command::new(cargo_bin!("ag-harness"));
        for provider in ModelProvider::all() {
            command
                .env_remove(provider.api_key_environment())
                .env_remove(provider.base_url_environment());
        }
        let mut child = command
            .arg("serve")
            .env("AG_HARNESS_ROOT", storage.path())
            .env("CHATGPT_AUTH_FILE", storage.path().join("chatgpt.json"))
            .env("OPENAI_COMPATIBLE_BASE_URL", provider.uri())
            .env("OPENAI_COMPATIBLE_CONTEXT_WINDOW", "40960")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take();
        let output = child.stdout.take().ok_or("serve stdout should be piped")?;
        let validator = Validator::new(&serde_json::from_str(PROTOCOL_SCHEMA)?)?;

        Ok(Self {
            child,
            input,
            output: BufReader::new(output).lines(),
            pending: Vec::new(),
            storage,
            validator,
        })
    }

    fn database(&self) -> Value {
        json!({"type": "sqlite", "path": self.storage.path().join("serve.db")})
    }

    async fn send_line(&mut self, line: &str) -> TestResult<()> {
        let input = self.input.as_mut().ok_or("stdin should be open")?;
        input.write_all(format!("{line}\n").as_bytes()).await?;

        Ok(())
    }

    async fn send(&mut self, message: &Value) -> TestResult<()> {
        assert!(
            self.validator.is_valid(message),
            "request violates the schema: {message}"
        );

        self.send_line(&message.to_string()).await
    }

    /// Reads the next message and checks it against the schema.
    async fn read(&mut self) -> TestResult<Option<Value>> {
        let Some(line) = self.output.next_line().await? else {
            return Ok(None);
        };
        let message: Value = serde_json::from_str(&line)?;
        let errors: Vec<String> = self
            .validator
            .iter_errors(&message)
            .map(|error| error.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{message} violates the schema: {errors:?}"
        );

        Ok(Some(message))
    }

    /// Returns the first message `matches` accepts, keeping the others.
    async fn wait(&mut self, matches: impl Fn(&Value) -> bool) -> TestResult<Value> {
        if let Some(index) = self.pending.iter().position(&matches) {
            return Ok(self.pending.remove(index));
        }
        loop {
            let message = self.read().await?.ok_or("serve should answer")?;
            if matches(&message) {
                return Ok(message);
            }
            self.pending.push(message);
        }
    }

    async fn call(&mut self, id: i64, method: &str, params: Value) -> TestResult<Value> {
        self.send(&request(id, method, params)).await?;

        self.wait(|message| message["id"] == id).await
    }

    async fn completed(&mut self, run: &Value) -> TestResult<Value> {
        self.wait(|message| is_completed(message) && message["params"]["event"]["run"] == *run)
            .await
    }

    /// Closes stdin, reads the remaining messages, and waits for the exit.
    async fn finish(mut self) -> TestResult<(Vec<Value>, bool)> {
        self.input = None;
        let mut messages = std::mem::take(&mut self.pending);
        while let Some(message) = self.read().await? {
            messages.push(message);
        }
        let status = self.child.wait().await?;

        Ok((messages, status.success()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_runs_sessions_concurrently_and_cancels_over_stdio() -> TestResult<()> {
    // Arrange
    let provider = provider().await;
    let mut server = Server::spawn(&provider)?;
    let output = json!({"type": "json", "schema": message_schema()});
    let model = json!({"id": "openai-compatible/local"});
    let database = server.database();

    // Act
    let memory = server
        .call(
            1,
            "session/create",
            json!({"session": "memory", "model": model, "output": output}),
        )
        .await?;
    let durable = server
        .call(
            2,
            "session/create",
            json!({"session": "durable", "model": model, "output": output, "store": database}),
        )
        .await?;
    let answered = server
        .call(
            3,
            "run/start",
            json!({"session": "memory", "input": "Hello"}),
        )
        .await?;
    let waiting = server
        .call(
            4,
            "run/start",
            json!({"session": "durable", "input": "Wait"}),
        )
        .await?;
    let answered_run = answered["result"]["run"].clone();
    let waiting_run = waiting["result"]["run"].clone();
    let answered_completed = server.completed(&answered_run).await?;
    let cancel = server
        .call(5, "run/cancel", json!({"run": waiting_run}))
        .await?;
    let cancelled = server.completed(&waiting_run).await?;
    server.send_line("{not json").await?;
    let parse_error = server
        .wait(|message| message.get("error").is_some() && message["id"].is_null())
        .await?;
    let transcript = server
        .call(
            6,
            "session/transcript",
            json!({"session": "memory", "range": {"type": "all"}}),
        )
        .await?;
    let durable_transcript = server
        .call(7, "session/transcript", json!({"session": "durable"}))
        .await?;
    let usage = server
        .call(8, "session/usage", json!({"session": "memory"}))
        .await?;
    let missing = server
        .call(9, "session/usage", json!({"session": "missing"}))
        .await?;
    server.send(&request(10, "shutdown", json!({}))).await?;
    let (rest, exited) = server.finish().await?;

    // Assert
    assert_eq!(memory["result"], json!({"session": "memory"}));
    assert_eq!(durable["result"], json!({"session": "durable"}));
    assert_eq!(
        answered_completed["params"]["event"]["kind"]["error"],
        Value::Null
    );
    assert_eq!(cancel["result"], json!({}));
    assert_eq!(
        cancelled["params"]["event"]["kind"]["error"]["kind"],
        json!("cancelled")
    );
    assert_eq!(parse_error["error"]["code"], json!(-32_700));
    assert_eq!(
        transcript["result"]["turns"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        transcript["result"]["turns"][0]["status"],
        json!("completed")
    );
    assert_eq!(
        durable_transcript["result"]["turns"][0]["status"],
        json!("interrupted")
    );
    assert_eq!(usage["result"]["turns"], json!(1));
    assert_eq!(usage["result"]["usage"]["input"], json!(6));
    assert_eq!(missing["error"]["code"], json!(-32_000));
    assert_eq!(missing["error"]["data"]["kind"], json!("not_found"));
    assert_eq!(missing["error"]["data"]["retryable"], json!(false));
    assert_eq!(rest.last().map(|message| &message["id"]), Some(&json!(10)));
    assert!(exited);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_stdin_cancels_runs_in_flight_and_exits() -> TestResult<()> {
    // Arrange
    let provider = provider().await;
    let mut server = Server::spawn(&provider)?;
    let output = json!({"type": "json", "schema": message_schema()});
    server
        .call(
            1,
            "session/create",
            json!({"session": "s", "model": {"id": "openai-compatible/local"}, "output": output}),
        )
        .await?;
    let started = server
        .call(2, "run/start", json!({"session": "s", "input": "Wait"}))
        .await?;

    // Act
    let (rest, exited) = server.finish().await?;

    // Assert
    let completed: Vec<&Value> = rest
        .iter()
        .filter(|message| is_completed(message))
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0]["params"]["event"]["run"],
        started["result"]["run"]
    );
    assert_eq!(
        completed[0]["params"]["event"]["kind"]["error"]["kind"],
        json!("cancelled")
    );
    assert!(exited);

    Ok(())
}

#[test]
fn serve_help_marks_the_protocol_unstable() -> TestResult<()> {
    // Arrange
    let mut command = std::process::Command::new(cargo_bin!("ag-harness"));

    // Act
    let output = command.args(["serve", "--help"]).output()?;

    // Assert
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout)?;
    assert!(help.contains("UNSTABLE JSON-RPC 2.0"));
    assert!(help.contains("session/create"));

    Ok(())
}
