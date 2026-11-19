//! Process-level coverage of the unstable `ag-harness serve` JSON-RPC
//! protocol: a real server process driven over stdio against a scripted
//! OpenAI-compatible provider, with every message checked against the
//! committed protocol schema and each result against its method's result
//! definition. Dispatch, error mapping, and shutdown are proven in process.

use std::collections::HashMap;
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

const PROTOCOL_SCHEMA: &str = include_str!("../../schema/serve_protocol.json");

/// Longest wait for a message or exit the server should produce, so a
/// regression fails instead of hanging on the delayed provider.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

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

fn is_event(message: &Value, kind: &str) -> bool {
    message["method"] == "run/event" && message["params"]["event"]["kind"]["type"] == kind
}

/// Compiles the committed schema, and one validator per method that accepts
/// only that method's result, from `x-method-results`.
fn validators() -> TestResult<(Validator, HashMap<String, Validator>)> {
    let schema: Value = serde_json::from_str(PROTOCOL_SCHEMA)?;
    let mut results = HashMap::new();
    for (method, result) in schema["x-method-results"]
        .as_object()
        .ok_or("schema should name method results")?
    {
        let mut result_schema = schema.clone();
        let fields = result_schema
            .as_object_mut()
            .ok_or("schema should be an object")?;
        fields.remove("anyOf");
        fields.insert("$ref".to_string(), result["$ref"].clone());
        results.insert(method.clone(), Validator::new(&result_schema)?);
    }

    Ok((Validator::new(&schema)?, results))
}

/// A running `ag-harness serve` process.
struct Server {
    child: Child,
    input: Option<ChildStdin>,
    output: Lines<BufReader<ChildStdout>>,
    /// Messages read while waiting for another one.
    pending: Vec<Value>,
    /// Result validator of each method.
    results: HashMap<String, Validator>,
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
        let (validator, results) = validators()?;

        Ok(Self {
            child,
            input,
            output: BufReader::new(output).lines(),
            pending: Vec::new(),
            results,
            storage,
            validator,
        })
    }

    fn database(&self) -> Value {
        json!({"type": "sqlite", "path": self.storage.path().join("serve.db")})
    }

    async fn send(&mut self, message: &Value) -> TestResult<()> {
        assert!(
            self.validator.is_valid(message),
            "request violates the schema: {message}"
        );
        let input = self.input.as_mut().ok_or("stdin should be open")?;
        input.write_all(format!("{message}\n").as_bytes()).await?;

        Ok(())
    }

    /// Reads the next message and checks it against the schema.
    async fn read(&mut self) -> TestResult<Option<Value>> {
        let Some(line) = tokio::time::timeout(READ_TIMEOUT, self.output.next_line()).await?? else {
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

    /// Sends a request and returns its response, whose result must match
    /// the method's result definition.
    async fn call(&mut self, id: i64, method: &str, params: Value) -> TestResult<Value> {
        self.send(&request(id, method, params)).await?;
        let response = self.wait(|message| message["id"] == id).await?;
        let validator = self
            .results
            .get(method)
            .ok_or("method should have a result")?;
        assert!(
            validator.is_valid(&response["result"]),
            "{method} result has another method's shape: {response}"
        );

        Ok(response)
    }

    /// Returns the first `run/event` of `run` whose kind is `kind`.
    async fn event(&mut self, run: &Value, kind: &str) -> TestResult<Value> {
        self.wait(|message| is_event(message, kind) && message["params"]["event"]["run"] == *run)
            .await
    }

    /// Closes stdin, reads the remaining messages, and waits for the exit.
    async fn finish(mut self) -> TestResult<(Vec<Value>, bool)> {
        self.input = None;
        let mut messages = std::mem::take(&mut self.pending);
        while let Some(message) = self.read().await? {
            messages.push(message);
        }
        let status = tokio::time::timeout(READ_TIMEOUT, self.child.wait()).await??;

        Ok((messages, status.success()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_runs_sessions_and_stops_runs_over_stdio() -> TestResult<()> {
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
    // A run cancelled before `started` may record no turn, so cancel only
    // once the durable turn is admitted.
    server.event(&waiting_run, "started").await?;
    let cancel = server
        .call(5, "run/cancel", json!({"run": waiting_run}))
        .await?;
    let answered_completed = server.event(&answered_run, "completed").await?;
    let cancelled = server.event(&waiting_run, "completed").await?;
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
    server
        .call(8, "session/usage", json!({"session": "memory"}))
        .await?;
    let open = server
        .call(
            9,
            "run/start",
            json!({"session": "memory", "input": "Wait again"}),
        )
        .await?;
    let (rest, exited) = server.finish().await?;

    // Assert
    assert_eq!(memory["result"], json!({"session": "memory"}));
    assert_eq!(durable["result"], json!({"session": "durable"}));
    let answered_kind = &answered_completed["params"]["event"]["kind"];
    assert_eq!(answered_kind["error"], Value::Null);
    assert_eq!(answered_kind["usage"]["input"], json!(6));
    assert_eq!(answered_kind["cost"], Value::Null);
    assert_eq!(cancel["result"], json!({}));
    assert_eq!(
        cancelled["params"]["event"]["kind"]["error"]["kind"],
        json!("cancelled")
    );
    assert_eq!(
        transcript["result"]["turns"][0]["status"],
        json!("completed")
    );
    let durable_turns = durable_transcript["result"]["turns"]
        .as_array()
        .ok_or("the durable transcript should list turns")?;
    assert_eq!(durable_turns.len(), 1);
    assert_eq!(durable_turns[0]["status"], json!("interrupted"));
    let stopped: Vec<&Value> = rest
        .iter()
        .filter(|message| is_event(message, "completed"))
        .collect();
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["params"]["event"]["run"], open["result"]["run"]);
    assert_eq!(
        stopped[0]["params"]["event"]["kind"]["error"]["kind"],
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
    assert!(help.contains("--print-schema"));

    Ok(())
}
