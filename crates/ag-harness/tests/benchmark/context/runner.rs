//! Live context-management scenarios at a forced small context budget or
//! at the model's declared window.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ag_harness::lifecycle::LifecycleEvent;
use ag_harness::model::{
    ContextBudget, ContextSummaryPolicy, HeuristicContextEstimator, ModelProfile,
};
use ag_harness::{Harness, OutputSchema, Tool};
use tempfile::TempDir;

use super::fixture::{
    FixtureFile, HugeResultFixture, ReadLoopFixture, Score, WriteFixture, answer_schema,
};
use super::record::{Activity, Observed, RunRecord};
use crate::{DynError, GIT_EXECUTABLE_ENV, Provider, benchmark_repository, summary};

const DEFAULT_WINDOW: u64 = 32_000;
const DEFAULT_RESERVED_OUTPUT: u64 = 16_384;
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DEFAULT_RESULTS: &str = "target/ag-harness-context-benchmark.jsonl";
const DECLARED_WINDOW: &str = "declared";
/// Agentty's declared context profiles of the benchmarked models, as
/// `crates/ag-agent/src/agent/model_profile.rs` declares them: the model,
/// its window, maximum output, and estimate margin's under-count percent,
/// and its fitted estimator as an `AG_HARNESS_CONTEXT_ESTIMATE` value.
const DECLARED_PROFILES: [(&str, u64, u64, u64, &str); 5] = [
    (
        "muse-spark-1.3",
        1_048_576,
        131_072,
        18,
        "225,44,339,0,420,1",
    ),
    ("qwen3.8-max", 1_000_000, 131_072, 15, "239,33,373,0,250,1"),
    ("qwen-plus", 1_000_000, 32_768, 16, "236,24,372,0,70,1"),
    ("kimi-k3", 1_048_576, 131_072, 17, "223,54,307,0,55,1"),
    ("kimi-k2.6", 262_144, 32_768, 17, "224,25,312,0,0,1"),
];

type ScenarioFuture = std::pin::Pin<Box<dyn Future<Output = Result<Measured, DynError>>>>;
type ScenarioRunner = fn(Harness) -> ScenarioFuture;

const SCENARIOS: [(&str, ScenarioRunner); 4] = [
    ("long_read_loop", long_read_loop),
    ("doc_comment_writes", doc_comment_writes),
    ("huge_result", huge_result),
    ("sequential_writes", sequential_writes),
];

/// Runs the selected scenarios for exactly one provider and appends one JSON
/// line per run to the results file.
pub(crate) async fn run() -> Result<(), DynError> {
    let config = Config::from_env()?;
    let scenarios = SCENARIOS
        .into_iter()
        .filter(|(name, _)| config.case.as_deref().is_none_or(|case| case == *name))
        .collect::<Vec<_>>();
    if scenarios.is_empty() {
        return Err("benchmark case filter did not match a context scenario".into());
    }
    if let Some(parent) = config.results.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut results = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.results)?;
    let revision = revision().await;
    let mut passed = 0;
    let mut total = 0;

    for repetition in 1..=config.repetitions {
        for &(scenario, runner) in &scenarios {
            let record = config
                .run_scenario(scenario, runner, repetition, revision.clone())
                .await?;
            passed += usize::from(record.success);
            total += 1;
            let line = serde_json::to_string(&record.to_json())?;
            writeln!(results, "{line}")?;
            results.flush()?;
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "CONTEXT {line}")?;
            stdout.flush()?;
        }
    }
    writeln!(
        io::stdout().lock(),
        "SUMMARY passed={passed} total={total} results={}",
        config.results.display()
    )?;
    summary::ensure_all_passed(passed, total)?;

    Ok(())
}

struct Config {
    case: Option<String>,
    context_summaries: bool,
    estimate: Option<String>,
    provider: Provider,
    repetitions: usize,
    results: PathBuf,
    timeout: Duration,
    window: Window,
}

impl Config {
    fn from_env() -> Result<Self, DynError> {
        let provider = std::env::var("AG_HARNESS_BENCHMARK_PROVIDER")
            .ok()
            .and_then(|name| {
                Provider::ALL
                    .into_iter()
                    .find(|provider| provider.as_str() == name)
            })
            .ok_or(
                "the context suite requires AG_HARNESS_BENCHMARK_PROVIDER=kimi, muse, or qwen",
            )?;
        let results = std::env::var_os("AG_HARNESS_CONTEXT_RESULTS").map_or_else(
            || {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(DEFAULT_RESULTS)
            },
            PathBuf::from,
        );

        Ok(Self {
            case: std::env::var("AG_HARNESS_BENCHMARK_CASE").ok(),
            context_summaries: std::env::var("AG_HARNESS_CONTEXT_SUMMARIES")
                .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "on")),
            estimate: std::env::var("AG_HARNESS_CONTEXT_ESTIMATE").ok(),
            provider,
            repetitions: number_env("AG_HARNESS_BENCHMARK_REPETITIONS", 1)?,
            results,
            timeout: Duration::from_secs(number_env(
                "AG_HARNESS_CONTEXT_TIMEOUT_SECS",
                DEFAULT_TIMEOUT_SECS,
            )?),
            window: Window::from_env()?,
        })
    }

    /// Resolves the request budget, the declared estimator, and the window
    /// and reservation they came from.
    fn context(&self) -> Result<ContextConfig, DynError> {
        let (window, reserved_output, estimate) = match self.window {
            Window::Fixed {
                reserved_output,
                window,
            } => (window, reserved_output, self.estimate.clone()),
            Window::Declared => {
                let model = self.provider.model();
                let &(_, window, max_output, under_count_percent, declared) = DECLARED_PROFILES
                    .iter()
                    .find(|(name, ..)| *name == model)
                    .ok_or_else(|| format!("no declared context profile for model {model:?}"))?;
                let profile =
                    ModelProfile::new(window).with_estimate_margin(max_output, under_count_percent);
                let estimate = self.estimate.clone().unwrap_or_else(|| declared.into());

                (window, profile.reserved_tokens(), Some(estimate))
            }
        };
        let budget = ContextBudget::new(
            NonZeroU64::new(window).ok_or("AG_HARNESS_CONTEXT_WINDOW must be positive")?,
        )
        .with_reserved_output(reserved_output)?;
        let estimator = estimate
            .as_deref()
            .map_or(Ok(HeuristicContextEstimator::DEFAULT), estimator)?;

        Ok(ContextConfig {
            budget,
            estimate,
            estimator,
            reserved_output,
            window,
        })
    }

    async fn run_scenario(
        &self,
        scenario: &'static str,
        runner: ScenarioRunner,
        repetition: usize,
        revision: Option<String>,
    ) -> Result<RunRecord, DynError> {
        let context = self.context()?;
        let policy = if self.context_summaries {
            ContextSummaryPolicy::Enabled
        } else {
            ContextSummaryPolicy::Disabled
        };
        let observed = Arc::new(Mutex::new(Observed::default()));
        let recorder = Arc::clone(&observed);
        let harness = Harness::new(self.provider.client()?, context.budget)
            .context_estimator(context.estimator)
            .context_summaries(policy)
            .with_lifecycle_observer(move |event: LifecycleEvent| {
                recorder
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .observe(event.kind());
            });
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let started = Instant::now();
        let result = tokio::time::timeout(self.timeout, runner(harness)).await;
        let wall_ms = started.elapsed().as_millis();
        let (activity, score, detail) = match result {
            Ok(Ok(measured)) => {
                let detail = (!measured.score.failures.is_empty())
                    .then(|| measured.score.failures.join("; "));
                (Some(measured.activity), measured.score, detail)
            }
            Ok(Err(error)) => (
                observed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take_failed_report()
                    .map(|report| Activity::from_report(&report)),
                Score::default(),
                Some(summary::sanitize_detail(&error.to_string())),
            ),
            Err(_) => (
                None,
                Score::default(),
                Some(format!("timed out after {}s", self.timeout.as_secs())),
            ),
        };

        Ok(RunRecord {
            activity,
            context_summaries: self.context_summaries,
            correctness: score.correctness(),
            estimate: context.estimate,
            detail,
            model: self.provider.model(),
            observed: std::mem::take(&mut *observed.lock().unwrap_or_else(PoisonError::into_inner)),
            provider: self.provider.as_str(),
            repetition,
            reserved_output: context.reserved_output,
            revision,
            scenario,
            started_at,
            success: score.is_perfect(),
            wall_ms,
            window: context.window,
            window_mode: self.window.mode(),
        })
    }
}

/// How the suite sizes each request's context budget.
#[derive(Clone, Copy)]
enum Window {
    /// The model's declared window, reservation, and estimator.
    Declared,
    /// `AG_HARNESS_CONTEXT_WINDOW` tokens, of which
    /// `AG_HARNESS_CONTEXT_RESERVED_OUTPUT` are reserved.
    Fixed { reserved_output: u64, window: u64 },
}

impl Window {
    fn from_env() -> Result<Self, DynError> {
        if std::env::var("AG_HARNESS_CONTEXT_WINDOW").as_deref() != Ok(DECLARED_WINDOW) {
            return Ok(Self::Fixed {
                reserved_output: number_env(
                    "AG_HARNESS_CONTEXT_RESERVED_OUTPUT",
                    DEFAULT_RESERVED_OUTPUT,
                )?,
                window: number_env("AG_HARNESS_CONTEXT_WINDOW", DEFAULT_WINDOW)?,
            });
        }
        if std::env::var_os("AG_HARNESS_CONTEXT_RESERVED_OUTPUT").is_some() {
            return Err(
                "AG_HARNESS_CONTEXT_RESERVED_OUTPUT cannot be combined with a declared window"
                    .into(),
            );
        }

        Ok(Self::Declared)
    }

    fn mode(self) -> &'static str {
        match self {
            Self::Declared => DECLARED_WINDOW,
            Self::Fixed { .. } => "fixed",
        }
    }
}

/// One run's resolved context configuration.
struct ContextConfig {
    budget: ContextBudget,
    estimate: Option<String>,
    estimator: HeuristicContextEstimator,
    reserved_output: u64,
    window: u64,
}

struct Measured {
    activity: Activity,
    score: Score,
}

fn long_read_loop(harness: Harness) -> ScenarioFuture {
    Box::pin(async move {
        let fixture = ReadLoopFixture::generate();
        let workspace = Workspace::create(fixture.files()).await?;
        let outcome = harness
            .repository(benchmark_repository(workspace.root())?)
            .allow(Tool::Read)
            .run_once(
                fixture.prompt(),
                OutputSchema::new(ReadLoopFixture::schema())?,
            )
            .await?;

        Ok(Measured {
            activity: Activity::from_outcome(&outcome),
            score: fixture.score(outcome.output()),
        })
    })
}

fn doc_comment_writes(harness: Harness) -> ScenarioFuture {
    write_scenario(harness, WriteFixture::generate())
}

fn sequential_writes(harness: Harness) -> ScenarioFuture {
    write_scenario(harness, WriteFixture::sequential())
}

fn write_scenario(harness: Harness, fixture: WriteFixture) -> ScenarioFuture {
    Box::pin(async move {
        let workspace = Workspace::create(fixture.files()).await?;
        let outcome = harness
            .repository(benchmark_repository(workspace.root())?)
            .allow(Tool::Read)
            .allow(Tool::Write)
            .run_once(fixture.prompt(), OutputSchema::new(answer_schema())?)
            .await?;
        let mut contents = BTreeMap::new();
        for file in fixture.files() {
            contents.insert(
                file.path.clone(),
                tokio::fs::read_to_string(workspace.root().join(&file.path)).await?,
            );
        }
        let status = workspace.git(&["status", "--porcelain"]).await?;

        Ok(Measured {
            activity: Activity::from_outcome(&outcome),
            score: fixture.score(&contents, &status),
        })
    })
}

fn huge_result(harness: Harness) -> ScenarioFuture {
    Box::pin(async move {
        let fixture = HugeResultFixture::generate();
        let workspace = Workspace::create(fixture.files()).await?;
        let outcome = harness
            .repository(benchmark_repository(workspace.root())?)
            .allow(Tool::Read)
            .run_once(
                HugeResultFixture::prompt(),
                OutputSchema::new(answer_schema())?,
            )
            .await?;

        Ok(Measured {
            activity: Activity::from_outcome(&outcome),
            score: HugeResultFixture::score(outcome.output()),
        })
    })
}

/// A committed Git repository holding one scenario's fixture files.
struct Workspace {
    directory: TempDir,
}

impl Workspace {
    async fn create(files: &[FixtureFile]) -> Result<Self, DynError> {
        let workspace = Self {
            directory: tempfile::tempdir()?,
        };
        for file in files {
            let path = workspace.root().join(&file.path);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(path, &file.contents).await?;
        }
        workspace.git(&["init", "--quiet"]).await?;
        workspace.git(&["add", "--all"]).await?;
        workspace
            .git(&[
                "-c",
                "user.name=benchmark",
                "-c",
                "user.email=benchmark@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--no-verify",
                "--message",
                "fixture",
            ])
            .await?;

        Ok(workspace)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    async fn git(&self, arguments: &[&str]) -> Result<String, DynError> {
        let git = std::env::var_os(GIT_EXECUTABLE_ENV)
            .ok_or_else(|| format!("{GIT_EXECUTABLE_ENV} must select the host Git executable"))?;
        let output = tokio::process::Command::new(git)
            .args(arguments)
            .current_dir(self.root())
            .output()
            .await?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

/// Returns the benchmarked checkout's commit, when Git can report it.
pub(crate) async fn revision() -> Option<String> {
    let git = std::env::var_os(GIT_EXECUTABLE_ENV)?;
    let output = tokio::process::Command::new(git)
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .await
        .ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parses `AG_HARNESS_CONTEXT_ESTIMATE`: message tokens per 1,000 bytes,
/// tokens per message, tool-definition tokens per 1,000 bytes, tokens per
/// definition, request tokens, and `1` when the provider renders the output
/// schema into the prompt.
fn estimator(spec: &str) -> Result<HeuristicContextEstimator, DynError> {
    let values = spec
        .split(',')
        .map(|value| value.trim().parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("AG_HARNESS_CONTEXT_ESTIMATE must hold numbers, got {spec:?}"))?;
    let [
        message_rate,
        message_tokens,
        tool_rate,
        tool_tokens,
        request_tokens,
        schema,
    ] = values[..]
    else {
        return Err(format!("AG_HARNESS_CONTEXT_ESTIMATE needs six values, got {spec:?}").into());
    };
    let estimator = HeuristicContextEstimator::DEFAULT
        .with_messages(message_rate, message_tokens)
        .with_tool_definitions(tool_rate, tool_tokens)
        .with_request_tokens(request_tokens);

    Ok(if schema == 1 {
        estimator.with_output_schema_in_prompt()
    } else {
        estimator
    })
}

fn number_env<T: std::str::FromStr>(name: &str, default: T) -> Result<T, DynError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be a number, got {value:?}").into()),
        Err(_) => Ok(default),
    }
}
