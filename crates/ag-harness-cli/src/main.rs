//! Interactive command-line chat powered by the `ag-harness` model runtime.

use std::borrow::Cow;
#[cfg(not(test))]
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, io};

use ag_harness::{
    ChatSession, Harness, ModelConfiguration, ModelConfigurationError, ModelProvider, OutputSchema,
    Tool, TurnOutcome,
};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};

const READ_ONLY_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a read-only repository harness. The read tool supports file, list, ",
    "search, diff, and show actions. For change review, call diff first, then use search, file, ",
    "list, or show for evidence. Call the tool immediately and use its result before answering. ",
    "Never narrate, promise, or defer a future tool call. Never claim that you created, ",
    "modified, deleted, or executed files or commands because filesystem mutation and command ",
    "execution are unavailable. If asked to perform an unsupported action, state that it is ",
    "unsupported."
);
const READ_WRITE_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a repository harness with read and write tools. The read tool supports ",
    "file, list, search, diff, and show actions. For change review, call diff first. When a user ",
    "asks about repository contents, call read immediately and use its result before answering. ",
    "When a user asks to create or modify a file, call the write tool ",
    "immediately in the same response. Never narrate, promise, or defer a future tool call. Only ",
    "claim that a file was created or modified after the write tool succeeds. File deletion and ",
    "command execution are unavailable."
);

/// Chats with models through a bounded repository harness.
#[derive(Debug, Parser)]
#[command(
    name = "ag-harness",
    version,
    about = "Chats with models through a repository harness",
    after_help = provider_help()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Supported harness commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Starts an in-memory chat with a model.
    Run(RunArgs),
}

/// Arguments for an in-memory model chat.
#[derive(Debug, Args)]
#[command(after_help = provider_help())]
struct RunArgs {
    /// Model identifier sent to the provider.
    model: String,
    /// Optional first prompt. Further prompts are read from standard input.
    #[arg(value_parser = parse_prompt)]
    prompt: Option<String>,
    /// API base URL, overriding the provider-specific environment variable.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Enables repository writes through the write tool.
    #[arg(long)]
    allow_write: bool,
    /// Model provider.
    #[arg(
        long,
        default_value_t = ModelProvider::Muse,
        value_parser = model_provider_parser()
    )]
    provider: ModelProvider,
    /// Repository directory available to enabled tools.
    #[arg(long, value_name = "DIR", default_value = ".")]
    read_dir: PathBuf,
}

fn model_provider_parser() -> impl TypedValueParser<Value = ModelProvider> {
    PossibleValuesParser::new(
        ModelProvider::all()
            .iter()
            .map(|provider| provider.as_str()),
    )
    .try_map(|provider| provider.parse::<ModelProvider>())
}

fn provider_help() -> String {
    let mut help =
        String::from("Supported models (other endpoint-supported model IDs also work):\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        help.push_str(&provider.known_models().join(", "));
        help.push('\n');
    }
    help.push_str("\nCredentials:\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        help.push_str(provider.api_key_environment());
        if provider.default_base_url().is_some() {
            help.push_str(" (");
            help.push_str(provider.base_url_environment());
            help.push_str(" optional)");
        } else {
            help.push_str(", ");
            help.push_str(provider.base_url_environment());
        }
        help.push('\n');
    }
    help.pop();

    help
}

fn parse_prompt(prompt: &str) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err("prompt must contain a non-whitespace character".to_string());
    }

    Ok(prompt.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatMode {
    Interactive,
    NonInteractive,
    OneShot,
}

impl ChatMode {
    fn detect(cli: &Cli, stdin_is_terminal: bool, stdout_is_terminal: bool) -> Self {
        if stdin_is_terminal && stdout_is_terminal {
            return Self::Interactive;
        }
        let Command::Run(args) = &cli.command;
        if stdin_is_terminal && args.prompt.is_some() {
            return Self::OneShot;
        }

        Self::NonInteractive
    }
}

#[cfg(not(test))]
#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let mode = ChatMode::detect(&cli, stdin_is_terminal, stdout_is_terminal);
    let input = BufReader::new(tokio::io::stdin());
    let output = tokio::io::stdout();

    report_exit(
        execute(cli, |name| env::var(name), input, output, mode).await,
        io::stderr().lock(),
    )
}

fn report_exit(result: Result<(), CliError>, mut error_output: impl io::Write) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let error = error.to_string();
            let error = single_line_terminal_text(&error);
            let _ = writeln!(error_output, "{error}");

            ExitCode::FAILURE
        }
    }
}

async fn execute<Input, Output>(
    cli: Cli,
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    input: Input,
    output: Output,
    mode: ChatMode,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    let Command::Run(args) = cli.command;
    let mut configuration = ModelConfiguration::new(args.provider, args.model.clone());
    if let Some(base_url) = &args.base_url {
        configuration = configuration.base_url(base_url.clone());
    }
    let client = configuration.client_from_environment(environment)?;
    let mut harness = Harness::new(client)
        .repository(args.read_dir.clone())
        .allow(Tool::Read);
    let system_prompt = if args.allow_write {
        harness = harness.allow(Tool::Write);
        READ_WRITE_SYSTEM_PROMPT
    } else {
        READ_ONLY_SYSTEM_PROMPT
    };
    let mut session = harness
        .chat(chat_schema()?)
        .with_system_prompt(system_prompt);

    run_chat(&mut session, &args.model, args.prompt, input, output, mode).await
}

async fn run_chat<Input, Output>(
    session: &mut ChatSession<'_>,
    requested_model: &str,
    initial_prompt: Option<String>,
    mut input: Input,
    mut output: Output,
    mode: ChatMode,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if mode == ChatMode::Interactive {
        let requested_model = single_line_terminal_text(requested_model);
        output
            .write_all(format!("Chat with {requested_model}. Ctrl-D to exit.\n").as_bytes())
            .await?;
    }

    let mut pending_prompt = initial_prompt;
    let mut turn_failed = false;
    loop {
        let Some(prompt) = read_prompt(&mut pending_prompt, &mut input, &mut output, mode).await?
        else {
            break;
        };
        if prompt.trim().is_empty() {
            continue;
        }
        match session.send(prompt).await {
            Ok(outcome) => write_outcome(&mut output, requested_model, &outcome).await?,
            Err(error) if mode == ChatMode::Interactive => {
                write_turn_error(&mut output, &error).await?;
            }
            Err(error) if mode == ChatMode::OneShot => return Err(error.into()),
            Err(error) => {
                write_turn_error(&mut output, &error).await?;
                turn_failed = true;
            }
        }
        if mode == ChatMode::OneShot {
            break;
        }
    }

    if turn_failed {
        Err(CliError::ChatTurnsFailed)
    } else {
        Ok(())
    }
}

async fn read_prompt<Input, Output>(
    pending_prompt: &mut Option<String>,
    input: &mut Input,
    output: &mut Output,
    mode: ChatMode,
) -> Result<Option<String>, io::Error>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if let Some(prompt) = pending_prompt.take() {
        return Ok(Some(prompt));
    }
    if mode == ChatMode::Interactive {
        output.write_all(b">>> ").await?;
        output.flush().await?;
    }
    let mut prompt = String::new();
    if input.read_line(&mut prompt).await? == 0 {
        return Ok(None);
    }
    trim_line_ending(&mut prompt);

    Ok(Some(prompt))
}

async fn write_outcome(
    output: &mut (impl AsyncWrite + Unpin),
    requested_model: &str,
    outcome: &TurnOutcome,
) -> Result<(), CliError> {
    let message = outcome
        .output()
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or(CliError::MissingMessage)?;
    output.write_all(assistant_text(message).as_bytes()).await?;
    output.write_all(b"---\n").await?;
    output
        .write_all(format!("turn: {}\n", format_duration(outcome.report().duration())).as_bytes())
        .await?;
    output
        .write_all(format!("model calls: {}\n", outcome.report().model_requests().len()).as_bytes())
        .await?;
    for (index, request) in outcome.report().model_requests().iter().enumerate() {
        let response_type = request.response_type();
        let completion = request.completion();
        let model = completion
            .and_then(|metadata| metadata.response_model())
            .unwrap_or(requested_model);
        let finish_reason =
            completion.map_or("unavailable", ag_harness::CompletionMetadata::finish_reason);
        let model = single_line_terminal_text(model);
        let finish_reason = single_line_terminal_text(finish_reason);
        let usage = completion
            .and_then(|metadata| metadata.usage())
            .map_or_else(|| "tokens unavailable".to_string(), format_usage);
        output
            .write_all(
                format!(
                    "  {}. {response_type}; {model}; {finish_reason}; {}; {usage}\n",
                    index + 1,
                    format_duration(request.duration()),
                )
                .as_bytes(),
            )
            .await?;
    }
    if outcome.report().tool_calls().is_empty() {
        output.write_all(b"tools: none\n").await?;
    } else {
        output.write_all(b"tools:\n").await?;
        for activity in outcome.report().tool_calls() {
            output
                .write_all(format!("  {activity}\n").as_bytes())
                .await?;
        }
    }
    output.flush().await?;

    Ok(())
}

async fn write_turn_error(
    output: &mut (impl AsyncWrite + Unpin),
    error: &ag_harness::TurnError,
) -> Result<(), io::Error> {
    let error = error.to_string();
    let error = single_line_terminal_text(&error);
    output
        .write_all(format!("error: {error}\n").as_bytes())
        .await?;
    output.flush().await
}

fn format_usage(usage: &ag_harness::CompletionUsage) -> String {
    let input = usage
        .input_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());
    let output = usage
        .output_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());
    let total = usage
        .total_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());

    format!("tokens {input} in, {output} out, {total} total")
}

fn format_duration(duration: std::time::Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

fn assistant_text(text: &str) -> String {
    let text = terminal_text(text);
    let mut framed = String::new();
    for (index, line) in text.split('\n').enumerate() {
        framed.push_str(if index == 0 {
            "assistant> "
        } else {
            "           "
        });
        framed.push_str(line);
        framed.push('\n');
    }

    framed
}

fn terminal_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_terminal_safe) {
        return Cow::Borrowed(text);
    }

    Cow::Owned(
        text.chars()
            .map(|character| {
                if is_terminal_safe(character) {
                    character
                } else {
                    '\u{fffd}'
                }
            })
            .collect(),
    )
}

fn single_line_terminal_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(|character| !character.is_control()) {
        return Cow::Borrowed(text);
    }

    Cow::Owned(
        text.chars()
            .map(|character| {
                if character.is_control() {
                    '\u{fffd}'
                } else {
                    character
                }
            })
            .collect(),
    )
}

fn is_terminal_safe(character: char) -> bool {
    !character.is_control() || matches!(character, '\n' | '\t')
}

fn chat_schema() -> Result<OutputSchema, CliError> {
    let message = Value::Object(Map::from_iter([(
        "type".to_string(),
        Value::String("string".to_string()),
    )]));
    let properties = Value::Object(Map::from_iter([("message".to_string(), message)]));
    let schema = Value::Object(Map::from_iter([
        ("type".to_string(), Value::String("object".to_string())),
        ("properties".to_string(), properties),
        (
            "required".to_string(),
            Value::Array(vec![Value::String("message".to_string())]),
        ),
        ("additionalProperties".to_string(), Value::Bool(false)),
    ]));

    OutputSchema::new(schema).map_err(CliError::from)
}

#[derive(Debug, Error)]
enum CliError {
    #[error("--base-url or {name} is required")]
    BaseUrlRequired { name: &'static str },
    #[error("one or more chat turns failed")]
    ChatTurnsFailed,
    #[error("model output did not contain a message")]
    MissingMessage,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ModelConfiguration(ModelConfigurationError),
    #[error(transparent)]
    OutputSchema(#[from] ag_harness::OutputSchemaError),
    #[error(transparent)]
    Turn(#[from] ag_harness::TurnError),
}

impl From<ModelConfigurationError> for CliError {
    fn from(error: ModelConfigurationError) -> Self {
        match error {
            ModelConfigurationError::BaseUrl { name } => Self::BaseUrlRequired { name },
            error => Self::ModelConfiguration(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    struct FixedModel(Value);

    #[async_trait]
    impl ag_harness::Model for FixedModel {
        async fn complete(
            &self,
            _request: ag_harness::ModelRequest,
        ) -> Result<ag_harness::ModelResponse, ag_harness::ModelError> {
            Ok(ag_harness::ModelResponse::Output(self.0.clone()))
        }
    }

    struct FailOnceModel {
        requests: AtomicUsize,
    }

    #[async_trait]
    impl ag_harness::Model for FailOnceModel {
        async fn complete(
            &self,
            _request: ag_harness::ModelRequest,
        ) -> Result<ag_harness::ModelResponse, ag_harness::ModelError> {
            if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ag_harness::ModelError::InvalidResponse);
            }

            Ok(ag_harness::ModelResponse::Output(
                json!({"message": "recovered"}),
            ))
        }
    }

    fn provider_response(message: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": json!({"message": message}).to_string()}
            }]
        }))
    }

    #[test]
    fn cli_accepts_chat_with_or_without_an_initial_prompt() {
        // Arrange and Act
        let without_prompt = Cli::try_parse_from(["ag-harness", "run", "muse-custom"])
            .expect("chat arguments should parse");
        let with_prompt = Cli::try_parse_from([
            "ag-harness",
            "run",
            "muse-custom",
            "Summarize this change",
            "--provider",
            "qwen",
            "--base-url",
            "https://models.example/v1",
            "--read-dir",
            "repo",
            "--allow-write",
        ])
        .expect("an initial prompt should parse");
        let blank_prompt = Cli::try_parse_from(["ag-harness", "run", "muse-custom", "  "])
            .expect_err("a blank initial prompt should be rejected");
        let unknown_provider =
            Cli::try_parse_from(["ag-harness", "run", "muse-custom", "--provider", "unknown"])
                .expect_err("an unknown provider should be rejected");

        // Assert
        let Command::Run(without_prompt) = without_prompt.command;
        assert_eq!(without_prompt.prompt, None);
        assert!(!without_prompt.allow_write);
        assert_eq!(without_prompt.provider, ModelProvider::Muse);
        assert_eq!(without_prompt.read_dir, PathBuf::from("."));
        let Command::Run(with_prompt) = with_prompt.command;
        assert_eq!(with_prompt.model, "muse-custom");
        assert_eq!(with_prompt.prompt.as_deref(), Some("Summarize this change"));
        assert_eq!(with_prompt.provider, ModelProvider::Qwen);
        assert_eq!(
            with_prompt.base_url.as_deref(),
            Some("https://models.example/v1")
        );
        assert_eq!(with_prompt.read_dir, PathBuf::from("repo"));
        assert!(with_prompt.allow_write);
        assert!(
            blank_prompt
                .to_string()
                .contains("prompt must contain a non-whitespace character")
        );
        assert!(
            unknown_provider
                .to_string()
                .contains("invalid value 'unknown'")
        );
    }

    #[test]
    fn cli_accepts_every_catalog_provider() {
        // Arrange and Act
        let providers = ModelProvider::all()
            .iter()
            .map(|provider| {
                Cli::try_parse_from([
                    "ag-harness",
                    "run",
                    "model-id",
                    "--provider",
                    provider.as_str(),
                ])
                .expect("catalog provider should parse")
            })
            .collect::<Vec<_>>();

        // Assert
        for (cli, expected) in providers.into_iter().zip(ModelProvider::all()) {
            let Command::Run(args) = cli.command;
            assert_eq!(args.provider, *expected);
        }
    }

    #[test]
    fn chat_mode_accounts_for_both_terminal_streams_and_initial_prompt() {
        // Arrange
        let with_prompt = Cli::try_parse_from(["ag-harness", "run", "muse", "hello"])
            .expect("chat arguments should parse");
        let without_prompt = Cli::try_parse_from(["ag-harness", "run", "muse"])
            .expect("chat arguments should parse");

        // Act and Assert
        assert_eq!(
            ChatMode::detect(&with_prompt, true, true),
            ChatMode::Interactive
        );
        assert_eq!(
            ChatMode::detect(&with_prompt, true, false),
            ChatMode::OneShot
        );
        assert_eq!(
            ChatMode::detect(&with_prompt, false, false),
            ChatMode::NonInteractive
        );
        assert_eq!(
            ChatMode::detect(&without_prompt, true, false),
            ChatMode::NonInteractive
        );
    }

    #[test]
    fn exit_reporting_sanitizes_errors_and_preserves_success() {
        // Arrange
        let mut success_output = Vec::new();
        let mut error_output = Vec::new();
        let error = CliError::Turn(ag_harness::TurnError::Model(
            ag_harness::ModelError::IncompleteResponse {
                reason: "stop\u{1b}]52;c;Y2xpcGJvYXJk\u{7}".to_string(),
            },
        ));

        // Act
        let success = report_exit(Ok(()), &mut success_output);
        let failure = report_exit(Err(error), &mut error_output);

        // Assert
        assert_eq!(success, ExitCode::SUCCESS);
        assert_eq!(failure, ExitCode::FAILURE);
        assert_eq!(success_output, [] as [u8; 0]);
        assert!(!error_output.contains(&0x1b));
        assert!(!error_output.contains(&0x07));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_advertises_repository_reads_by_default() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_string_contains(r#""name":"read""#))
            .and(body_string_contains(r#""content":"Hello","role":"user""#))
            .respond_with(provider_response("hello"))
            .expect(1)
            .mount(&server)
            .await;
        let cli = Cli::try_parse_from([
            "ag-harness",
            "run",
            "muse-test",
            "Hello",
            "--base-url",
            &server.uri(),
        ])
        .expect("chat arguments should parse");
        let input = BufReader::new(&b""[..]);
        let mut output = Vec::new();

        // Act
        execute(
            cli,
            |_| Ok("test-key".to_string()),
            input,
            &mut output,
            ChatMode::OneShot,
        )
        .await
        .expect("chat with default repository reads should succeed");

        // Assert
        assert!(
            String::from_utf8(output)
                .expect("chat output should be UTF-8")
                .starts_with("assistant> hello\n---\n")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_advertises_writes_only_when_explicitly_enabled() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_string_contains(READ_WRITE_SYSTEM_PROMPT))
            .and(body_string_contains(r#""name":"read""#))
            .and(body_string_contains(r#""name":"write""#))
            .respond_with(provider_response("ready"))
            .expect(1)
            .mount(&server)
            .await;
        let cli = Cli::try_parse_from([
            "ag-harness",
            "run",
            "muse-test",
            "Hello",
            "--base-url",
            &server.uri(),
            "--allow-write",
        ])
        .expect("write-enabled chat arguments should parse");
        let input = BufReader::new(&b""[..]);
        let mut output = Vec::new();

        // Act
        execute(
            cli,
            |_| Ok("test-key".to_string()),
            input,
            &mut output,
            ChatMode::OneShot,
        )
        .await
        .expect("write-enabled chat should succeed");

        // Assert
        assert!(
            String::from_utf8(output)
                .expect("chat output should be UTF-8")
                .starts_with("assistant> ready\n---\n")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_advertises_read_only_with_an_explicit_directory() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_string_contains(r#""name":"read""#))
            .and(body_string_contains(r#""content":"Hello","role":"user""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call-read",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"input.txt"}"#
                            }
                        }]
                    }
                }]
            })))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_string_contains(r#""tool_call_id":"call-read""#))
            .respond_with(provider_response("hello"))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        let repository = tempfile::TempDir::new().expect("temporary repository should exist");
        std::fs::write(repository.path().join("input.txt"), "contents")
            .expect("read fixture should be written");
        let cli = Cli::try_parse_from([
            "ag-harness",
            "run",
            "muse-test",
            "Hello",
            "--base-url",
            &server.uri(),
            "--read-dir",
            &repository.path().to_string_lossy(),
        ])
        .expect("chat arguments should parse");
        let input = BufReader::new(&b""[..]);
        let mut output = Vec::new();

        // Act
        execute(
            cli,
            |_| Ok("test-key".to_string()),
            input,
            &mut output,
            ChatMode::OneShot,
        )
        .await
        .expect("chat with explicit read access should succeed");

        // Assert
        let output = String::from_utf8(output).expect("chat output should be UTF-8");
        assert!(output.starts_with("assistant> hello\n---\n"));
        assert!(output.contains("tools:\n  read input.txt (lines 1-1;"));
    }

    #[test]
    fn cli_configuration_errors_preserve_cli_specific_guidance() {
        // Arrange
        let base_url = ModelConfigurationError::BaseUrl {
            name: "KIMI_BASE_URL",
        };
        let api_key = ModelConfigurationError::ApiKey {
            name: "MODEL_API_KEY",
        };

        // Act
        let base_url = CliError::from(base_url);
        let api_key = CliError::from(api_key);

        // Assert
        assert_eq!(
            base_url.to_string(),
            "--base-url or KIMI_BASE_URL is required"
        );
        assert_eq!(api_key.to_string(), "MODEL_API_KEY is unavailable");
    }

    #[test]
    fn chat_schema_requires_one_message_string() {
        // Arrange and Act
        let schema = chat_schema().expect("chat schema should compile");

        // Assert
        assert_eq!(schema.value()["required"], json!(["message"]));
        assert_eq!(schema.value()["additionalProperties"], json!(false));
    }

    #[test]
    fn line_endings_are_trimmed_without_changing_prompt_content() {
        // Arrange
        let mut unix = "hello\n".to_string();
        let mut windows = "hello\r\n".to_string();
        let mut unchanged = "hello".to_string();

        // Act
        trim_line_ending(&mut unix);
        trim_line_ending(&mut windows);
        trim_line_ending(&mut unchanged);

        // Assert
        assert_eq!(unix, "hello");
        assert_eq!(windows, "hello");
        assert_eq!(unchanged, "hello");
    }

    #[test]
    fn durations_have_compact_terminal_formatting() {
        // Arrange and Act
        let short = format_duration(std::time::Duration::ZERO);
        let measured = format_duration(std::time::Duration::from_millis(12));

        // Assert
        assert_eq!(short, "<1 ms");
        assert_eq!(measured, "12 ms");
    }

    #[tokio::test]
    async fn interactive_chat_prints_prompts_and_handles_blank_input() {
        // Arrange
        let harness = Harness::new(FixedModel(json!({"message": "hello"})))
            .repository(".")
            .allow(Tool::Read);
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b"\nquestion\n"[..]);
        let mut output = Vec::new();

        // Act
        run_chat(
            &mut session,
            "test-model",
            None,
            input,
            &mut output,
            ChatMode::Interactive,
        )
        .await
        .expect("interactive chat should finish at EOF");

        // Assert
        let output = String::from_utf8(output).expect("chat output should be UTF-8");
        assert!(
            output.starts_with("Chat with test-model. Ctrl-D to exit.\n>>> >>> assistant> hello\n")
        );
        assert!(output.contains("output; test-model; unavailable;"));
        assert!(output.contains("tokens unavailable"));
        assert!(output.ends_with("tools: none\n>>> "));
    }

    #[tokio::test]
    async fn interactive_chat_continues_after_a_failed_turn() {
        // Arrange
        let harness = Harness::new(FailOnceModel {
            requests: AtomicUsize::new(0),
        });
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b"first\nretry\n"[..]);
        let mut output = Vec::new();

        // Act
        run_chat(
            &mut session,
            "test-model",
            None,
            input,
            &mut output,
            ChatMode::Interactive,
        )
        .await
        .expect("interactive chat should recover and finish at EOF");

        // Assert
        let output = String::from_utf8(output).expect("chat output should be UTF-8");
        assert!(output.contains("error: model returned no response content\n"));
        assert!(output.contains(">>> assistant> recovered\n---\n"));
        assert!(output.ends_with("tools: none\n>>> "));
    }

    #[tokio::test]
    async fn noninteractive_chat_reports_a_failure_before_retrying() {
        // Arrange
        let harness = Harness::new(FailOnceModel {
            requests: AtomicUsize::new(0),
        });
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b"first\nretry\n"[..]);
        let mut output = Vec::new();

        // Act
        let error = run_chat(
            &mut session,
            "test-model",
            None,
            input,
            &mut output,
            ChatMode::NonInteractive,
        )
        .await
        .expect_err("a recovered chat should retain its failed exit status");

        // Assert
        assert!(matches!(error, CliError::ChatTurnsFailed));
        let output = String::from_utf8(output).expect("chat output should be UTF-8");
        assert!(output.starts_with("error: model returned no response content\n"));
        assert!(output.contains("assistant> recovered\n---\n"));
    }

    #[tokio::test]
    async fn noninteractive_chat_returns_the_last_failure_at_eof() {
        // Arrange
        let harness = Harness::new(FailOnceModel {
            requests: AtomicUsize::new(0),
        });
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b"first\n"[..]);
        let mut output = Vec::new();

        // Act
        let error = run_chat(
            &mut session,
            "test-model",
            None,
            input,
            &mut output,
            ChatMode::NonInteractive,
        )
        .await
        .expect_err("the final failed turn should be returned at EOF");

        // Assert
        assert!(matches!(error, CliError::ChatTurnsFailed));
        assert_eq!(
            String::from_utf8(output).expect("chat output should be UTF-8"),
            "error: model returned no response content\n"
        );
    }

    #[tokio::test]
    async fn chat_rejects_model_output_that_violates_schema() {
        // Arrange
        let harness = Harness::new(FixedModel(json!({"unexpected": true})));
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b""[..]);
        let mut output = Vec::new();

        // Act
        let error = run_chat(
            &mut session,
            "test-model",
            Some("question".to_string()),
            input,
            &mut output,
            ChatMode::OneShot,
        )
        .await
        .expect_err("schema-invalid output should fail");

        // Assert
        assert!(matches!(
            error,
            CliError::Turn(ag_harness::TurnError::Model(
                ag_harness::ModelError::SchemaViolation { path, .. }
            )) if path == "$"
        ));
        assert_eq!(output, [] as [u8; 0]);
    }

    #[tokio::test]
    async fn one_shot_chat_does_not_read_follow_up_terminal_input() {
        // Arrange
        let harness = Harness::new(FixedModel(json!({"message": "hello"})));
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b"unexpected follow-up\n"[..]);
        let mut output = Vec::new();

        // Act
        run_chat(
            &mut session,
            "test-model",
            Some("question".to_string()),
            input,
            &mut output,
            ChatMode::OneShot,
        )
        .await
        .expect("one-shot chat should finish after the initial prompt");

        // Assert
        let output = String::from_utf8(output).expect("chat output should be UTF-8");
        assert_eq!(output.matches("assistant> hello\n---\n").count(), 1);
        assert!(!output.contains("Chat with"));
        assert!(!output.contains(">>>"));
    }

    #[tokio::test]
    async fn one_shot_chat_returns_turn_failures() {
        // Arrange
        let harness = Harness::new(FailOnceModel {
            requests: AtomicUsize::new(0),
        });
        let mut session = harness.chat(chat_schema().expect("chat schema should compile"));
        let input = BufReader::new(&b""[..]);
        let mut output = Vec::new();

        // Act
        let error = run_chat(
            &mut session,
            "test-model",
            Some("question".to_string()),
            input,
            &mut output,
            ChatMode::OneShot,
        )
        .await
        .expect_err("one-shot chat should return its failed turn");

        // Assert
        assert!(matches!(error, CliError::Turn(_)));
        assert_eq!(output, [] as [u8; 0]);
    }

    #[test]
    fn terminal_text_replaces_control_sequences_and_preserves_safe_whitespace() {
        // Arrange
        let text = "before\n\t\u{1b}]52;c;Y2xpcGJvYXJk\u{7}after\r";

        // Act
        let sanitized = terminal_text(text);

        // Assert
        assert_eq!(
            sanitized,
            "before\n\t\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}after\u{fffd}"
        );
        assert!(
            sanitized
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        );
    }

    #[test]
    fn assistant_text_indents_continuation_lines_and_sanitizes_them() {
        // Arrange
        let text = "answer\n---\nturn: forged\u{1b}";

        // Act
        let framed = assistant_text(text);

        // Assert
        assert_eq!(
            framed,
            "assistant> answer\n           ---\n           turn: forged\u{fffd}\n"
        );
    }

    #[test]
    fn single_line_terminal_text_replaces_all_control_characters() {
        // Arrange
        let text = "model\nname\t\u{1b}";

        // Act
        let sanitized = single_line_terminal_text(text);

        // Assert
        assert_eq!(sanitized, "model\u{fffd}name\u{fffd}\u{fffd}");
        assert!(sanitized.chars().all(|character| !character.is_control()));
    }

    #[test]
    fn usage_format_marks_missing_counts() {
        // Arrange
        let usage = ag_harness::CompletionUsage::new(None, None, None, Some(4), None, None);

        // Act
        let formatted = format_usage(&usage);

        // Assert
        assert_eq!(formatted, "tokens ? in, 4 out, ? total");
    }
}
