//! Live probes of how providers count the input tokens of request content.
//!
//! Each probe sends one fixed request straight through `ag-router`, rendered
//! as `ModelClient` renders it, and records the payload bytes the harness
//! estimator weighs beside the provider-reported input tokens, so per-model
//! estimate parameters can be fitted from the records.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ag_harness::tool::ToolDefinition;
use ag_router::{
    Completion, CompletionMetadata, CompletionUsage, JsonSchemaFormat, ModelMessage, ModelRequest,
    OutputSchema, ProviderConfig, Router, ToolCall,
};
use serde_json::{Value, json};

use crate::context::ReadLoopFixture;
use crate::{DynError, Provider};

const DEFAULT_RESULTS: &str = "target/ag-harness-estimate-probes.jsonl";
/// Spacing between Kimi requests under its three-request-per-minute limit.
const KIMI_REQUEST_INTERVAL: Duration = Duration::from_secs(25);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const SIZES: [usize; 3] = [2_000, 8_000, 32_000];
const PROMPT: &str = "Reply with the JSON object {\"answer\":\"ok\"} and nothing else.";
const PROSE: &str = concat!(
    include_str!("../../README.md"),
    include_str!("../../../../docs/site/content/docs/architecture/ag-harness-design.md"),
);
const CODE: &str = include_str!("../../src/context.rs");

/// Content class of one probe message.
#[derive(Clone, Copy)]
enum Class {
    Code,
    Fixture,
    Json,
    Prose,
    Recorded,
    Short,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Fixture => "fixture",
            Self::Json => "json",
            Self::Prose => "prose",
            Self::Recorded => "recorded",
            Self::Short => "short",
        }
    }
}

/// One fixed request varying one feature from the base request.
struct Probe {
    messages: Vec<(ModelMessage, Class)>,
    name: String,
    schema: Value,
    tools: Vec<ToolDefinition>,
}

impl Probe {
    fn new(name: impl Into<String>, messages: Vec<(ModelMessage, Class)>) -> Self {
        Self {
            messages,
            name: name.into(),
            schema: answer_schema(),
            tools: Vec::new(),
        }
    }

    fn all() -> Result<Vec<Self>, DynError> {
        let prompt = || (ModelMessage::User(PROMPT.into()), Class::Short);
        let mut probes = vec![
            Self::new("base", vec![prompt()]),
            Self::new(
                "system",
                vec![
                    (
                        ModelMessage::System("Follow the user's instructions exactly.".into()),
                        Class::Short,
                    ),
                    prompt(),
                ],
            ),
        ];
        for turns in [2, 4, 8] {
            let mut messages = Vec::new();
            for turn in 0..turns {
                messages.push((
                    ModelMessage::User(format!("Note {turn}: remember the value {turn}.")),
                    Class::Short,
                ));
                messages.push((
                    ModelMessage::Assistant("{\"answer\":\"noted\"}".into()),
                    Class::Short,
                ));
            }
            messages.push(prompt());
            probes.push(Self::new(format!("turns_{turns}"), messages));
        }
        for size in SIZES {
            for (class, text) in [(Class::Prose, PROSE), (Class::Code, CODE)] {
                let content = ModelMessage::User(prefix(text, size).to_string());
                probes.push(Self::new(
                    format!("{}_{size}", class.as_str()),
                    vec![prompt(), (content, class)],
                ));
            }
            let mut messages = vec![prompt()];
            messages.extend(read_round(0, "src/module_0.rs", prefix(CODE, size))?);
            probes.push(Self::new(format!("json_{size}"), messages));
        }
        let fixture = ReadLoopFixture::generate();
        for files in [4, 16] {
            let mut messages = vec![prompt()];
            for (round, file) in fixture.files().iter().take(files).enumerate() {
                let [call, (result, _)] = read_round(round, &file.path, &file.contents)?;
                messages.extend([call, (result, Class::Fixture)]);
            }
            probes.push(Self::new(format!("fixture_{files}"), messages));
        }
        for rounds in [1, 4] {
            let mut messages = vec![prompt()];
            for round in 0..rounds {
                messages.extend(read_round(
                    round,
                    &format!("src/module_{round}.rs"),
                    prefix(CODE, 200),
                )?);
            }
            probes.push(Self::new(format!("rounds_{rounds}"), messages));
        }
        let tools = [
            ToolDefinition::read(),
            ToolDefinition::write(),
            ToolDefinition::bash(),
        ];
        for count in 1..=tools.len() {
            probes.push(Self {
                tools: tools[..count].to_vec(),
                ..Self::new(format!("tools_{count}"), vec![prompt()])
            });
        }
        probes.push(Self {
            schema: large_schema(),
            ..Self::new("schema_large", vec![prompt()])
        });
        probes.push(Self {
            schema: large_schema(),
            tools: tools.to_vec(),
            ..Self::new("tools_3_schema_large", vec![prompt()])
        });

        Ok(probes)
    }

    /// Loads recorded requests from a JSON Lines corpus. Each line holds a
    /// `name`, an optional `system` prompt and output `schema`, the advertised
    /// built-in `tools` by name, and `messages` as a harness session store
    /// records them: `kind` plus JSON `payload`.
    fn recorded(corpus: &Path) -> Result<Vec<Self>, DynError> {
        let mut probes = Vec::new();
        for line in std::fs::read_to_string(corpus)?.lines() {
            let line: Value = serde_json::from_str(line)?;
            let mut messages = Vec::new();
            if let Some(system) = line["system"].as_str() {
                messages.push((ModelMessage::System(system.into()), Class::Recorded));
            }
            for message in line["messages"].as_array().into_iter().flatten() {
                messages.push((recorded_message(message)?, Class::Recorded));
            }
            let tools = line["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|name| match name.as_str() {
                    Some("read") => Ok(ToolDefinition::read()),
                    Some("write") => Ok(ToolDefinition::write()),
                    Some("bash") => Ok(ToolDefinition::bash()),
                    _ => Err(format!("unknown corpus tool {name}")),
                })
                .collect::<Result<_, _>>()?;
            probes.push(Self {
                messages,
                name: line["name"].as_str().unwrap_or("recorded").to_string(),
                schema: if line["schema"].is_object() {
                    line["schema"].clone()
                } else {
                    answer_schema()
                },
                tools,
            });
        }

        Ok(probes)
    }

    fn request(&self, model: &str) -> Result<ModelRequest, DynError> {
        let tools = self
            .tools
            .iter()
            .map(|tool| {
                ag_router::ToolDefinition::new(
                    tool.name(),
                    tool.description(),
                    tool.parameters().clone(),
                )
            })
            .collect();
        let format =
            JsonSchemaFormat::new("ag_harness_output", OutputSchema::new(self.schema.clone())?)?;

        Ok(ModelRequest::chat(
            model,
            self.messages
                .iter()
                .map(|(message, _)| message.clone())
                .collect(),
            tools,
            format,
        ))
    }

    /// Returns the probe's features in the units the harness estimator
    /// weighs: payload bytes per message and per tool definition.
    fn features(&self) -> Value {
        let messages: Vec<Value> = self
            .messages
            .iter()
            .map(|(message, class)| {
                json!({
                    "bytes": retained_bytes(message),
                    "class": class.as_str(),
                    "reasoning_bytes": reasoning_bytes(message),
                })
            })
            .collect();
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "bytes": tool.name().len()
                        + tool.description().len()
                        + tool.parameters().to_string().len(),
                    "name": tool.name(),
                })
            })
            .collect();

        json!({
            "messages": messages,
            "probe": self.name,
            "schema_bytes": self.schema.to_string().len(),
            "tools": tools,
        })
    }
}

/// Sends every probe once to the selected provider and appends one JSON line
/// per probe to the results file.
pub(crate) async fn run() -> Result<(), DynError> {
    let provider = std::env::var("AG_HARNESS_BENCHMARK_PROVIDER")
        .ok()
        .and_then(|name| {
            Provider::ALL
                .into_iter()
                .find(|provider| provider.as_str() == name)
        })
        .ok_or("the estimate suite requires AG_HARNESS_BENCHMARK_PROVIDER=kimi, muse, or qwen")?;
    let results = std::env::var_os("AG_HARNESS_ESTIMATE_RESULTS").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(DEFAULT_RESULTS)
        },
        PathBuf::from,
    );
    if let Some(parent) = results.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&results)?;
    let model = provider.model();
    let router = Router::single(provider_config(provider)?);
    let routed_model = format!("{}/{model}", provider.as_str());
    let filter = std::env::var("AG_HARNESS_BENCHMARK_CASE").ok();
    let revision = crate::context::revision().await;
    let mut failures = 0;
    let probes = match std::env::var_os("AG_HARNESS_ESTIMATE_CORPUS") {
        Some(corpus) => Probe::recorded(Path::new(&corpus))?,
        None => Probe::all()?,
    };
    for (index, probe) in probes
        .into_iter()
        .filter(|probe| filter.as_deref().is_none_or(|case| probe.name == case))
        .enumerate()
    {
        if index > 0 && matches!(provider, Provider::Kimi) {
            tokio::time::sleep(KIMI_REQUEST_INTERVAL).await;
        }
        let request = probe.request(&routed_model)?;
        let usage = match tokio::time::timeout(REQUEST_TIMEOUT, router.execute(request)).await {
            Ok(Ok(
                Completion::Output { metadata, .. } | Completion::ToolCalls { metadata, .. },
            )) => Ok(metadata),
            Ok(Err(error)) => error
                .metadata()
                .cloned()
                .ok_or_else(|| crate::summary::sanitize_detail(&error.to_string())),
            Err(_) => Err(format!("timed out after {}s", REQUEST_TIMEOUT.as_secs())),
        };
        let mut line = probe.features();
        line["provider"] = json!(provider.as_str());
        line["model"] = json!(model);
        line["revision"] = json!(revision);
        line["started_at"] = json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        );
        match usage {
            Ok(metadata) => {
                let usage = metadata.usage().copied();
                line["input_tokens"] = json!(usage.and_then(CompletionUsage::input_tokens));
                line["cache_hit_tokens"] = json!(usage.and_then(CompletionUsage::cache_hit_tokens));
                line["finish_reason"] = json!(CompletionMetadata::finish_reason(&metadata));
            }
            Err(detail) => {
                failures += 1;
                line["detail"] = json!(detail);
            }
        }
        let line = serde_json::to_string(&line)?;
        writeln!(file, "{line}")?;
        file.flush()?;
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "ESTIMATE {line}")?;
        stdout.flush()?;
    }
    if failures > 0 {
        return Err(format!("{failures} estimate probes reported no usage").into());
    }

    Ok(())
}

fn provider_config(provider: Provider) -> Result<ProviderConfig, DynError> {
    let (provider, api_key, base_url) = match provider {
        Provider::Kimi => (
            ag_router::Provider::Kimi,
            std::env::var("KIMI_API_KEY")?,
            std::env::var("KIMI_BASE_URL")?,
        ),
        Provider::Muse => (
            ag_router::Provider::Muse,
            std::env::var("MODEL_API_KEY")?,
            std::env::var("MODEL_API_BASE_URL")
                .unwrap_or_else(|_| crate::MODEL_API_BASE_URL.to_string()),
        ),
        Provider::Qwen => (
            ag_router::Provider::Qwen,
            std::env::var("DASHSCOPE_API_KEY")?,
            std::env::var("DASHSCOPE_BASE_URL")?,
        ),
    };

    Ok(ProviderConfig {
        provider,
        api_key,
        base_url,
    })
}

/// Returns one `read` call of `path` and its result holding `content`, as
/// the `read` tool serializes it.
fn read_round(
    round: usize,
    path: &str,
    content: &str,
) -> Result<[(ModelMessage, Class); 2], DynError> {
    let call_id = format!("call_{round}");
    let arguments = json!({"action": "file", "path": path}).to_string();
    let call = ToolCall::from_json(
        call_id.clone(),
        "read",
        &arguments,
        Some("I will read the file.".into()),
    )?;
    let result = json!({
        "content": content,
        "end_line": content.lines().count(),
        "next_offset": null,
        "path": path,
        "start_line": 1,
        "truncated": false,
    });

    Ok([
        (ModelMessage::AssistantToolCall(call), Class::Short),
        (
            ModelMessage::ToolResult {
                call_id,
                content: result.to_string(),
                name: "read".into(),
            },
            Class::Json,
        ),
    ])
}

/// Decodes one stored session message into the message it was sent as.
fn recorded_message(message: &Value) -> Result<ModelMessage, DynError> {
    let payload = &message["payload"];
    let text = || payload.as_str().map(str::to_string).ok_or("text payload");

    Ok(match message["kind"].as_str() {
        Some("user") => ModelMessage::User(text()?),
        Some("assistant") => ModelMessage::Assistant(text()?),
        Some("system") => ModelMessage::System(text()?),
        Some("assistant_tool_call") => ModelMessage::AssistantToolCall(recorded_call(payload)?),
        Some("assistant_tool_calls") => ModelMessage::AssistantToolCalls(
            payload
                .as_array()
                .into_iter()
                .flatten()
                .map(recorded_call)
                .collect::<Result<_, _>>()?,
        ),
        Some("tool_result") => ModelMessage::ToolResult {
            call_id: payload["call_id"].as_str().unwrap_or_default().into(),
            content: payload["content"].as_str().unwrap_or_default().into(),
            name: payload["name"].as_str().unwrap_or_default().into(),
        },
        kind => return Err(format!("unsupported corpus message kind {kind:?}").into()),
    })
}

fn recorded_call(call: &Value) -> Result<ToolCall, DynError> {
    Ok(ToolCall::from_json(
        call["id"].as_str().unwrap_or_default().into(),
        call["name"].as_str().unwrap_or_default(),
        &call["arguments"].to_string(),
        call["reasoning_content"].as_str().map(str::to_string),
    )?)
}

/// Payload bytes of `message` as `ag_harness::model::ModelMessage` retains
/// them.
fn retained_bytes(message: &ModelMessage) -> usize {
    match message {
        ModelMessage::Assistant(content)
        | ModelMessage::System(content)
        | ModelMessage::User(content) => content.len(),
        ModelMessage::AssistantToolCall(call) => {
            call.id().len()
                + call.name().len()
                + call.arguments_json().len()
                + call.reasoning_content().map_or(0, str::len)
        }
        ModelMessage::AssistantToolCalls(calls) => calls
            .iter()
            .map(|call| {
                call.id().len()
                    + call.name().len()
                    + call.arguments_json().len()
                    + call.reasoning_content().map_or(0, str::len)
            })
            .sum(),
        ModelMessage::ToolResult {
            call_id,
            content,
            name,
        } => call_id.len() + content.len() + name.len(),
        _ => 0,
    }
}

/// Reasoning bytes of `message`'s tool calls, which only some providers
/// receive.
fn reasoning_bytes(message: &ModelMessage) -> usize {
    match message {
        ModelMessage::AssistantToolCall(call) => call.reasoning_content().map_or(0, str::len),
        ModelMessage::AssistantToolCalls(calls) => calls
            .iter()
            .map(|call| call.reasoning_content().map_or(0, str::len))
            .sum(),
        _ => 0,
    }
}

/// Returns the longest prefix of `text` within `size` bytes.
fn prefix(text: &str, size: usize) -> &str {
    &text[..text.floor_char_boundary(size.min(text.len()))]
}

fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    })
}

/// Returns an answer schema padded with 24 optional described properties.
fn large_schema() -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("answer".into(), json!({"type": "string"}));
    for index in 0..24 {
        properties.insert(
            format!("detail_{index}"),
            json!({
                "type": ["string", "null"],
                "description": format!(
                    "Optional supporting detail number {index}; leave null unless asked."
                ),
            }),
        );
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": ["answer"],
        "additionalProperties": false
    })
}
