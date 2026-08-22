//! Interactive command-line chat for the `ag-harness` model runtime.

use std::env;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use ag_harness::{
    ChatSession, Harness, ModelClient, ModelResponseType, MuseConfig, OutputSchema, Tool,
    ToolActivity, TurnOutcome,
};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};

const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";
const HELP_INSTRUCTIONS: &str = "\
Get started:
  Set MODEL_API_KEY, then run ag-harness run <MODEL>.
  Type a prompt and press Enter. Press Ctrl-D to exit.

Examples:
  ag-harness run muse-spark-1.2
  ag-harness run muse-spark-1.2 \"Summarize Cargo.toml\"

Run `ag-harness run --help` for chat details.";
const MODEL_API_BASE_URL_ENV: &str = "MODEL_API_BASE_URL";
const MODEL_API_KEY_ENV: &str = "MODEL_API_KEY";
const RUN_HELP_INSTRUCTIONS: &str = "\
Chat behavior:
  Prompts share in-memory history until the process exits.
  The model may read files beneath the current directory; writes are disabled.
  Only run it where those files may be sent to the configured provider.
  Each answer includes model, token, timing, tool, and inspected-file metadata.

Environment:
  MODEL_API_KEY       Required provider API key.
  MODEL_API_BASE_URL  Optional provider endpoint override.";

/// Chats with models through a bounded, read-only repository harness.
#[derive(Debug, Parser)]
#[command(
    name = "ag-harness",
    version,
    about = "Chats with models through a read-only repository harness",
    after_help = HELP_INSTRUCTIONS
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Supported harness commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Starts an in-memory chat with a model.
    #[command(after_help = RUN_HELP_INSTRUCTIONS)]
    Run(RunArgs),
}

/// Arguments for an in-memory model chat.
#[derive(Debug, Args)]
struct RunArgs {
    /// Model identifier sent to the provider.
    model: String,
    /// Optional first prompt. Further prompts are read from standard input.
    prompt: Option<String>,
    /// API base URL, overriding `MODEL_API_BASE_URL`.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let interactive = io::stdin().is_terminal();
    let input = BufReader::new(tokio::io::stdin());
    let output = tokio::io::stdout();
    let repository_root = match env::current_dir() {
        Ok(path) => path,
        Err(source) => {
            let _ = writeln!(
                io::stderr().lock(),
                "failed to resolve current directory: {source}"
            );

            return ExitCode::FAILURE;
        }
    };

    match execute(
        Cli::parse(),
        |name| env::var(name),
        repository_root,
        input,
        output,
        interactive,
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");

            ExitCode::FAILURE
        }
    }
}

async fn execute<Input, Output>(
    cli: Cli,
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    repository_root: PathBuf,
    input: Input,
    output: Output,
    interactive: bool,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    let Command::Run(args) = cli.command;
    let client = ModelClient::muse(model_config(&args, environment)?)?;
    let harness = Harness::new(client)
        .repository(repository_root)
        .allow(Tool::Read);
    let mut session = harness.chat(chat_schema()?);

    run_chat(
        &mut session,
        &args.model,
        args.prompt,
        input,
        output,
        interactive,
    )
    .await
}

async fn run_chat<Input, Output>(
    session: &mut ChatSession<'_>,
    requested_model: &str,
    initial_prompt: Option<String>,
    mut input: Input,
    mut output: Output,
    interactive: bool,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if interactive {
        output
            .write_all(format!("Chat with {requested_model}. Ctrl-D to exit.\n").as_bytes())
            .await?;
    }

    let mut pending_prompt = initial_prompt;
    loop {
        let prompt = if let Some(prompt) = pending_prompt.take() {
            prompt
        } else {
            if interactive {
                output.write_all(b">>> ").await?;
                output.flush().await?;
            }
            let mut prompt = String::new();
            if input.read_line(&mut prompt).await? == 0 {
                break;
            }
            trim_line_ending(&mut prompt);

            prompt
        };
        if prompt.trim().is_empty() {
            continue;
        }
        let outcome = session.send(prompt).await?;
        write_outcome(&mut output, requested_model, &outcome).await?;
    }

    Ok(())
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
    output.write_all(message.as_bytes()).await?;
    output.write_all(b"\n---\n").await?;
    output
        .write_all(format!("turn: {}\n", format_duration(outcome.report().duration())).as_bytes())
        .await?;
    output
        .write_all(format!("model calls: {}\n", outcome.report().model_requests().len()).as_bytes())
        .await?;
    for (index, request) in outcome.report().model_requests().iter().enumerate() {
        let response_type = match request.response_type() {
            ModelResponseType::Output => "output",
            ModelResponseType::ToolCall => "tool call",
            _ => "unknown",
        };
        let completion = request.completion();
        let model = completion
            .and_then(|metadata| metadata.response_model())
            .unwrap_or(requested_model);
        let finish_reason =
            completion.map_or("unavailable", ag_harness::CompletionMetadata::finish_reason);
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
                .write_all(format_tool_activity(activity).as_bytes())
                .await?;
        }
    }
    output.flush().await?;

    Ok(())
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

fn format_tool_activity(activity: &ToolActivity) -> String {
    match activity {
        ToolActivity::Read {
            duration,
            end_line,
            path,
            start_line,
            truncated,
        } => {
            let lines = end_line.map_or_else(
                || format!("line {start_line}"),
                |end_line| format!("lines {start_line}-{end_line}"),
            );
            let continuation = if *truncated { ", truncated" } else { "" };

            format!(
                "  read {path} ({lines}{continuation}; {})\n",
                format_duration(*duration)
            )
        }
        ToolActivity::Write {
            bytes_written,
            duration,
            path,
        } => format!(
            "  write {path} ({bytes_written} bytes; {})\n",
            format_duration(*duration)
        ),
        ToolActivity::WriteRejected { duration, path } => format!(
            "  write {path} (rejected; {})\n",
            format_duration(*duration)
        ),
        _ => format!(
            "  {} {} ({})\n",
            activity.name(),
            activity.path(),
            format_duration(activity.duration())
        ),
    }
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

fn chat_schema() -> Result<OutputSchema, CliError> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "message": {"type": "string"}
        },
        "required": ["message"],
        "additionalProperties": false
    }))?)
}

fn model_config(
    args: &RunArgs,
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<MuseConfig, CliError> {
    let api_key = environment(MODEL_API_KEY_ENV).map_err(|source| CliError::Environment {
        name: MODEL_API_KEY_ENV,
        source,
    })?;
    let base_url = if let Some(base_url) = &args.base_url {
        base_url.clone()
    } else {
        match environment(MODEL_API_BASE_URL_ENV) {
            Ok(base_url) => base_url,
            Err(env::VarError::NotPresent) => DEFAULT_BASE_URL.to_string(),
            Err(source) => {
                return Err(CliError::Environment {
                    name: MODEL_API_BASE_URL_ENV,
                    source,
                });
            }
        }
    };

    Ok(MuseConfig {
        api_key,
        base_url,
        model: args.model.clone(),
    })
}

#[derive(Debug, Error)]
enum CliError {
    #[error("{name} is unavailable: {source}")]
    Environment {
        name: &'static str,
        source: env::VarError,
    },
    #[error("model output did not contain a message")]
    MissingMessage,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ModelConfiguration(#[from] ag_harness::ModelMetadataError),
    #[error(transparent)]
    OutputSchema(#[from] ag_harness::OutputSchemaError),
    #[error(transparent)]
    Turn(#[from] ag_harness::TurnError),
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::Value;

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

    fn run_args() -> RunArgs {
        RunArgs {
            base_url: None,
            model: "muse-model".to_string(),
            prompt: None,
        }
    }

    fn missing_environment(_: &str) -> Result<String, env::VarError> {
        Err(env::VarError::NotPresent)
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
            "--base-url",
            "https://models.example/v1",
        ])
        .expect("an initial prompt should parse");

        // Assert
        let Command::Run(without_prompt) = without_prompt.command;
        assert_eq!(without_prompt.prompt, None);
        let Command::Run(with_prompt) = with_prompt.command;
        assert_eq!(with_prompt.model, "muse-custom");
        assert_eq!(with_prompt.prompt.as_deref(), Some("Summarize this change"));
        assert_eq!(
            with_prompt.base_url.as_deref(),
            Some("https://models.example/v1")
        );
    }

    #[test]
    fn model_configuration_uses_environment_defaults() {
        // Arrange
        let args = run_args();

        // Act
        let config = model_config(&args, |name| match name {
            MODEL_API_KEY_ENV => Ok("test-key".to_string()),
            _ => Err(env::VarError::NotPresent),
        })
        .expect("default model configuration should be valid");

        // Assert
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.model, "muse-model");
    }

    #[test]
    fn model_configuration_prefers_base_url_flag() {
        // Arrange
        let mut args = run_args();
        args.base_url = Some("https://cli.example/v1".to_string());

        // Act
        let config = model_config(&args, |name| match name {
            MODEL_API_KEY_ENV => Ok("test-key".to_string()),
            _ => Ok("environment-value".to_string()),
        })
        .expect("CLI overrides should produce valid configuration");

        // Assert
        assert_eq!(config.base_url, "https://cli.example/v1");
    }

    #[test]
    fn model_configuration_reports_missing_api_key() {
        // Arrange
        let args = run_args();

        // Act
        let error = model_config(&args, missing_environment)
            .err()
            .expect("a missing API key should be rejected");

        // Assert
        assert_eq!(
            error.to_string(),
            "MODEL_API_KEY is unavailable: environment variable not found"
        );
    }

    #[test]
    fn model_configuration_reports_non_unicode_optional_environment() {
        // Arrange
        let args = run_args();

        // Act
        let error = model_config(&args, |name| match name {
            MODEL_API_KEY_ENV => Ok("test-key".to_string()),
            MODEL_API_BASE_URL_ENV => Err(env::VarError::NotUnicode("invalid".into())),
            _ => Err(env::VarError::NotPresent),
        })
        .err()
        .expect("non-Unicode configuration should be rejected");

        // Assert
        assert!(
            error
                .to_string()
                .starts_with("MODEL_API_BASE_URL is unavailable:")
        );
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
        run_chat(&mut session, "test-model", None, input, &mut output, true)
            .await
            .expect("interactive chat should finish at EOF");

        // Assert
        let output = String::from_utf8(output).expect("chat output should be UTF-8");
        assert!(output.starts_with("Chat with test-model. Ctrl-D to exit.\n>>> >>> hello\n"));
        assert!(output.contains("output; test-model; unavailable;"));
        assert!(output.contains("tokens unavailable"));
        assert!(output.ends_with("tools: none\n>>> "));
    }

    #[tokio::test]
    async fn chat_rejects_model_output_without_a_message() {
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
            false,
        )
        .await
        .expect_err("missing message output should fail");

        // Assert
        assert!(matches!(error, CliError::MissingMessage));
    }

    #[test]
    fn activity_formatting_covers_read_and_write_outcomes() {
        // Arrange
        let empty_read = ToolActivity::Read {
            duration: std::time::Duration::ZERO,
            end_line: None,
            path: "empty.txt".to_string(),
            start_line: 3,
            truncated: false,
        };
        let write = ToolActivity::Write {
            bytes_written: 12,
            duration: std::time::Duration::from_millis(2),
            path: "output.txt".to_string(),
        };
        let rejected = ToolActivity::WriteRejected {
            duration: std::time::Duration::from_millis(1),
            path: "blocked.txt".to_string(),
        };

        // Act and Assert
        assert_eq!(
            format_tool_activity(&empty_read),
            "  read empty.txt (line 3; <1 ms)\n"
        );
        assert_eq!(
            format_tool_activity(&write),
            "  write output.txt (12 bytes; 2 ms)\n"
        );
        assert_eq!(
            format_tool_activity(&rejected),
            "  write blocked.txt (rejected; 1 ms)\n"
        );
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
