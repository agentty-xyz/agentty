//! Manually executed, real-model compatibility benchmark.

mod summary;

use std::io::Write as _;
use std::time::{Duration, Instant};
use std::{env, fmt};

use ag_harness::{
    Harness, KimiConfig, MUSE_SPARK_1_3, ModelClient, ModelRequestActivity, ModelResponseType,
    MuseConfig, OutputSchema, QwenConfig, Tool, ToolActivity, TurnOutcome,
};
use serde_json::{Value, json};

type DynError = Box<dyn std::error::Error + Send + Sync>;

const MODEL_API_BASE_URL: &str = "https://api.meta.ai/v1";

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let repetitions = env::var("AG_HARNESS_BENCHMARK_REPETITIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let mut results = Vec::new();

    for repetition in 1..=repetitions {
        for provider in Provider::ALL {
            results.push(run_case(provider, repetition, "structured", structured).await);
            results.push(run_case(provider, repetition, "parallel_read", parallel_read).await);
            results.push(run_case(provider, repetition, "read_recovery", read_recovery).await);
            results.push(run_case(provider, repetition, "write", write).await);
            results.push(run_case(provider, repetition, "memory", memory).await);
        }
    }

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    for result in &results {
        writeln!(stdout, "{result}")?;
    }
    let passed = results.iter().filter(|result| result.passed).count();
    let total = results.len();
    writeln!(stdout, "SUMMARY passed={passed} total={total}")?;
    stdout.flush()?;
    summary::ensure_all_passed(passed, total)?;

    Ok(())
}

async fn run_case(
    provider: Provider,
    repetition: usize,
    case: &'static str,
    run: fn(Provider) -> CaseFuture,
) -> ResultLine {
    let started_at = Instant::now();
    let result = run(provider).await;
    let (detail, measurement, passed) = match result {
        Ok(measurement) => (None, measurement, true),
        Err(error) => (Some(error.to_string()), CaseMeasurement::default(), false),
    };

    ResultLine {
        case,
        detail,
        duration: started_at.elapsed(),
        measurement,
        passed,
        provider,
        repetition,
    }
}

type CaseFuture = std::pin::Pin<Box<dyn Future<Output = Result<CaseMeasurement, DynError>>>>;

fn structured(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let schema = schema(json!({
            "type": "object",
            "properties": {
                "person": {
                    "type": "object",
                    "properties": {
                        "active": { "type": "boolean", "const": true },
                        "name": { "type": "string", "const": "Ada" },
                        "score": { "type": "integer", "const": 17 }
                    },
                    "required": ["active", "name", "score"],
                    "additionalProperties": false
                },
                "tags": {
                    "type": "array",
                    "prefixItems": [
                        { "type": "string", "const": "rust" },
                        { "type": "string", "const": "agent" }
                    ],
                    "items": false,
                    "minItems": 2,
                    "maxItems": 2
                }
            },
            "required": ["person", "tags"],
            "additionalProperties": false
        }))?;
        let outcome = Harness::new(provider.client()?)
            .run_report(
                "Extract this record exactly: Ada has score 17, is active, and has tags rust then \
                 agent.",
                schema,
            )
            .await?;
        if outcome.output()["person"]["name"] != "Ada" {
            return Err("structured output had the wrong name".into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn parallel_read(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let repository = tempfile::tempdir()?;
        std::fs::write(repository.path().join("alpha.txt"), "first=amber\n")?;
        std::fs::write(repository.path().join("beta.txt"), "second=17\n")?;
        let schema = exact_value_schema("code", "amber-17")?;
        let outcome = Harness::new(provider.client()?)
            .repository(repository.path())
            .allow(Tool::Read)
            .run_report(
                "Read both alpha.txt and beta.txt. Combine their values as first-second.",
                schema,
            )
            .await?;
        let mut read_paths = match outcome.report().tool_calls() {
            [
                ToolActivity::Read {
                    path: first_path, ..
                },
                ToolActivity::Read {
                    path: second_path, ..
                },
            ] => [first_path.as_str(), second_path.as_str()],
            activities => {
                return Err(format!(
                    "expected exactly two successful reads, observed activities={activities:?}"
                )
                .into());
            }
        };
        read_paths.sort_unstable();
        let response_types = outcome
            .report()
            .model_requests()
            .iter()
            .map(ModelRequestActivity::response_type)
            .collect::<Vec<_>>();
        if read_paths != ["alpha.txt", "beta.txt"]
            || response_types != [ModelResponseType::ToolCall, ModelResponseType::Output]
            || outcome.output()["code"] != "amber-17"
        {
            return Err(format!(
                "expected one exact read batch and amber-17, observed paths={read_paths:?} \
                 responses={response_types:?}"
            )
            .into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn read_recovery(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let repository = tempfile::tempdir()?;
        std::fs::write(repository.path().join("fallback.txt"), "code=violet-29\n")?;
        let schema = exact_value_schema("code", "violet-29")?;
        let outcome = Harness::new(provider.client()?)
            .repository(repository.path())
            .allow(Tool::Read)
            .run_report(
                "First read missing.txt. When that is rejected, recover by reading fallback.txt \
                 and return its code.",
                schema,
            )
            .await?;
        let rejected = outcome
            .report()
            .tool_calls()
            .iter()
            .any(|activity| matches!(activity, ToolActivity::ReadRejected { .. }));
        let recovered = outcome
            .report()
            .tool_calls()
            .iter()
            .any(|activity| matches!(activity, ToolActivity::Read { path, .. } if path == "fallback.txt"));
        if !rejected || !recovered || outcome.output()["code"] != "violet-29" {
            return Err("model did not follow the rejected-read recovery trajectory".into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn write(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let repository = tempfile::tempdir()?;
        let target = repository.path().join("status.txt");
        std::fs::write(&target, "status=pending\n")?;
        let schema = schema(json!({
            "type": "object",
            "properties": { "changed": { "type": "boolean", "const": true } },
            "required": ["changed"],
            "additionalProperties": false
        }))?;
        let outcome = Harness::new(provider.client()?)
            .repository(repository.path())
            .allow(Tool::Write)
            .run_report(
                "Use the write tool to change status.txt from status=pending to status=complete.",
                schema,
            )
            .await?;
        let wrote = outcome.report().tool_calls().iter().any(
            |activity| matches!(activity, ToolActivity::Write { path, .. } if path == "status.txt"),
        );
        if !wrote || std::fs::read_to_string(target)? != "status=complete\n" {
            return Err("model did not produce the verified file edit".into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn memory(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let schema = schema(json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string", "enum": ["stored", "cobalt-41"] }
            },
            "required": ["answer"],
            "additionalProperties": false
        }))?;
        let harness = Harness::new(provider.client()?);
        let mut chat = harness.chat(schema);
        let stored = chat
            .send("Remember the code cobalt-41 and answer only with stored.")
            .await?;
        let recalled = chat
            .send("What exact code did I ask you to remember?")
            .await?;
        if stored.output()["answer"] != "stored" || recalled.output()["answer"] != "cobalt-41" {
            return Err("chat did not retain the exact code across turns".into());
        }

        Ok(CaseMeasurement::from_outcomes([&stored, &recalled]))
    })
}

fn exact_value_schema(property: &str, value: &str) -> Result<OutputSchema, DynError> {
    schema(json!({
        "type": "object",
        "properties": { property: { "type": "string", "const": value } },
        "required": [property],
        "additionalProperties": false
    }))
}

fn schema(value: Value) -> Result<OutputSchema, DynError> {
    OutputSchema::new(value).map_err(Into::into)
}

#[derive(Clone, Copy)]
enum Provider {
    Kimi,
    Muse,
    Qwen,
}

impl Provider {
    const ALL: [Self; 3] = [Self::Kimi, Self::Muse, Self::Qwen];

    fn client(self) -> Result<ModelClient, DynError> {
        match self {
            Self::Kimi => Ok(ModelClient::kimi(KimiConfig {
                api_key: env::var("KIMI_API_KEY")?,
                base_url: env::var("KIMI_BASE_URL")?,
                model: env::var("KIMI_MODEL")?,
            })?),
            Self::Muse => Ok(ModelClient::muse(MuseConfig {
                api_key: env::var("MODEL_API_KEY")?,
                base_url: env::var("MODEL_API_BASE_URL")
                    .unwrap_or_else(|_| MODEL_API_BASE_URL.to_string()),
                model: env::var("MODEL_API_MODEL").unwrap_or_else(|_| MUSE_SPARK_1_3.to_string()),
            })?),
            Self::Qwen => Ok(ModelClient::qwen(QwenConfig {
                api_key: env::var("DASHSCOPE_API_KEY")?,
                base_url: env::var("DASHSCOPE_BASE_URL")?,
                model: "qwen-plus".to_string(),
            })?),
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kimi => formatter.write_str("kimi"),
            Self::Muse => formatter.write_str("muse"),
            Self::Qwen => formatter.write_str("qwen"),
        }
    }
}

struct ResultLine {
    case: &'static str,
    detail: Option<String>,
    duration: Duration,
    measurement: CaseMeasurement,
    passed: bool,
    provider: Provider,
    repetition: usize,
}

impl fmt::Display for ResultLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "RESULT provider={} case={} repetition={} passed={} duration_ms={} model_requests={} \
             tool_calls={} total_tokens={}",
            self.provider,
            self.case,
            self.repetition,
            self.passed,
            self.duration.as_millis(),
            self.measurement.model_requests,
            self.measurement.tool_calls,
            self.measurement
                .total_tokens
                .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string())
        )?;
        if let Some(detail) = &self.detail {
            write!(formatter, " detail={}", detail.replace(['\n', '\r'], " "))?;
        }

        Ok(())
    }
}

#[derive(Default)]
struct CaseMeasurement {
    model_requests: usize,
    tool_calls: usize,
    total_tokens: Option<u64>,
}

impl CaseMeasurement {
    fn from_outcomes<'a>(outcomes: impl IntoIterator<Item = &'a TurnOutcome>) -> Self {
        let mut measurement = Self::default();
        let mut reported_tokens = false;
        let mut total_tokens = 0_u64;

        for outcome in outcomes {
            measurement.model_requests += outcome.report().model_requests().len();
            measurement.tool_calls += outcome.report().tool_calls().len();
            for request in outcome.report().model_requests() {
                if let Some(tokens) = request
                    .completion()
                    .and_then(|completion| completion.usage())
                    .and_then(|usage| usage.total_tokens())
                {
                    reported_tokens = true;
                    total_tokens = total_tokens.saturating_add(tokens);
                }
            }
        }
        measurement.total_tokens = reported_tokens.then_some(total_tokens);

        measurement
    }
}
