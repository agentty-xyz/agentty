//! Embedding `ag-harness` in a host program: one model served through
//! `ModelClient::from_spec`, one host tool behind an approval handler, and a
//! run whose events the host consumes as they arrive.
//!
//! Run it against any OpenAI-compatible Chat Completions server, such as a
//! local llama.cpp, vLLM, or Ollama server:
//!
//! ```text
//! AG_HARNESS_EXAMPLE_ENDPOINT=http://127.0.0.1:8080/v1 \
//! AG_HARNESS_EXAMPLE_MODEL=qwen3 \
//! cargo run -p ag-harness --example embed
//! ```

use std::env;
use std::error::Error;
use std::future::poll_fn;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;

use ag_harness::event::Item;
use ag_harness::model::{ContextBudget, ModelClient};
use ag_harness::provider::{ProviderSpec, WireProtocol};
use ag_harness::tool::{
    ToolContext, ToolDefinition, ToolDefinitionError, ToolEffects, ToolError, ToolOutput,
};
use ag_harness::{
    ApprovalHandler, ApprovalRequest, Decision, Harness, OutputSchema, RunEventKind, RunRequest,
    ToolHandler, ToolSet,
};
use futures_core::Stream;
use serde_json::{Value, json};

/// Base URL of the Chat Completions server, ending before
/// `/chat/completions`.
const ENDPOINT: &str = "AG_HARNESS_EXAMPLE_ENDPOINT";

/// Model the server serves; `local-model` when unset.
const MODEL: &str = "AG_HARNESS_EXAMPLE_MODEL";

fn main() -> Result<ExitCode, Box<dyn Error>> {
    // First, before any runtime or thread: when the harness starts this
    // executable as its native sandbox helper, `dispatch` runs the helper
    // and returns its exit code. Hosts offering `bash` through
    // `BashConfig::native` need no separate `ag-harness-sandbox` binary.
    ag_harness::sandbox::dispatch().map_or_else(host, Ok)
}

/// The host program, which starts its runtime only after `dispatch`.
#[tokio::main]
async fn host() -> Result<ExitCode, Box<dyn Error>> {
    run(
        env::var(ENDPOINT).ok(),
        env::var(MODEL).ok(),
        &mut io::stdout(),
    )
    .await
}

/// Files one ticket through the model at `endpoint` and reports the run's
/// events to `output`.
async fn run(
    endpoint: Option<String>,
    model: Option<String>,
    output: &mut impl Write,
) -> Result<ExitCode, Box<dyn Error>> {
    let endpoint =
        endpoint.ok_or_else(|| format!("set {ENDPOINT} to an OpenAI-compatible base URL"))?;
    let spec = ProviderSpec::new("local", WireProtocol::ChatCompletions, endpoint)
        .for_model(model.as_deref().unwrap_or("local-model"));
    let tools = ToolSet::builder().handler(Arc::new(FileTicket::new()?));
    let tools = tools.build()?;
    let harness = Harness::builder()
        .model(Arc::new(ModelClient::from_spec(spec)?))
        // A specification declares no context window; state the model's.
        .context_budget(ContextBudget::new(NonZeroU64::new(32_000).ok_or("budget")?))
        .tools(tools)
        .approval(Arc::new(ApproveTickets));
    let harness = harness.build()?;
    let request = RunRequest::new("The login page times out. File a ticket and answer its id.")
        .output(OutputSchema::new(json!({
            "type": "object",
            "properties": {"ticket": {"type": "string"}},
            "required": ["ticket"],
        }))?);
    // The run makes progress while its outcome is awaited, so await it on
    // its own task and read the events here.
    let (mut events, handle) = harness.run(request).into_parts();
    let outcome = tokio::spawn(handle.outcome());
    while let Some(event) = poll_fn(|context| Pin::new(&mut events).poll_next(context)).await {
        match event.kind {
            RunEventKind::ApprovalRequested { request, .. } => {
                writeln!(output, "approval requested: {}", request.tool)?;
            }
            RunEventKind::ItemCompleted {
                item: Item::ToolCall(call),
                ..
            } => writeln!(output, "tool {}: {}", call.name, call.output)?,
            // A run offering native-sandboxed `bash` on a host without the
            // sandbox reports `RunEventKind::SandboxUnavailable` here.
            _ => {}
        }
    }
    let outcome = outcome.await?;
    let answer = outcome.result?.parse::<Value>()?;
    writeln!(output, "ticket: {}", answer["ticket"])?;

    Ok(ExitCode::SUCCESS)
}

/// Host tool that files a ticket; it changes state, so the approval handler
/// decides each call.
struct FileTicket(ToolDefinition);

impl FileTicket {
    fn new() -> Result<Self, ToolDefinitionError> {
        ToolDefinition::new(
            "file_ticket",
            "File an issue ticket and return its id.",
            json!({
                "type": "object",
                "properties": {"title": {"type": "string"}},
                "required": ["title"],
            }),
        )
        .map(Self)
    }
}

#[async_trait::async_trait]
impl ToolHandler for FileTicket {
    fn definition(&self) -> ToolDefinition {
        self.0.clone()
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::Writes
    }

    async fn call(&self, arguments: Value, _context: ToolContext) -> Result<ToolOutput, ToolError> {
        let title = arguments["title"].as_str().unwrap_or_default();

        Ok(ToolOutput::new(
            json!({"id": format!("TICKET-{}", title.len())}).to_string(),
        ))
    }
}

/// Allows tickets and denies every other state-changing call.
struct ApproveTickets;

#[async_trait::async_trait]
impl ApprovalHandler for ApproveTickets {
    async fn decide(&self, request: &ApprovalRequest) -> Decision {
        if request.tool == "file_ticket" {
            Decision::Allow
        } else {
            Decision::Deny {
                feedback: "only tickets may be filed".to_string(),
            }
        }
    }
}

#[cfg(test)]
#[path = "embed_test.rs"]
mod tests;
