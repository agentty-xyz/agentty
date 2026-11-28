//! Live behavioral prompt evaluation through the production worker and
//! runtime.
//!
//! Live runs require `AGENTTY_EVAL_PROVIDER`, `AGENTTY_EVAL_MODEL`,
//! `AGENTTY_EVAL_EFFORT` (low/medium/high/xhigh), and
//! `AGENTTY_EVAL_REPETITIONS` (2..=20); `AGENTTY_EVAL_CASES`, a
//! comma-separated list of case names, optionally limits a run to the cases
//! a prompt change affects. Harness providers read their API key
//! from Agentty's key file, `auth/api_keys.json` under `AGENTTY_ROOT` (else
//! `~/.agentty`), and import a key variable into it; point `AGENTTY_ROOT` at
//! a scratch directory to keep a run away from real keys. The manual
//! prompt-evaluation hook emits one JSON record per case and repetition.
//! Missing credentials, provider errors, and timeouts fail the run; none is
//! silently counted as a pass.
//!
//! Every case runs in a committed Git repository, as Agentty sessions do,
//! so Harness repository tools can inspect it. Only Harness runs
//! (`AGENTTY_EVAL_PROVIDER=harness`) exercise the Agentty-owned
//! unavailable-tool result, and only they add the `bash` cases, whose
//! utility prompts run commands only inside the native sandbox.
//!
//! The binary is its own test harness so that `main` can call
//! `dispatch_sandbox_helper` before anything else, as Agentty's `main`
//! does; the harness then starts this executable as the sandbox helper. It
//! answers the `cargo nextest` listing protocol with its one ignored test
//! and runs it only when ignored tests are selected; see
//! [`Invocation::parse`]. The cases, graders, and their deterministic tests
//! live in `prompt_evaluation_grader.rs`.

use std::error::Error;
use std::io::{self, Write as _};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ag_contracts::{AgentRequestKind, PermissionMode, ReasoningLevel};
use ag_store::Database;
use ag_worker::test_support::instruction_bootstrap_key;
use ag_worker::{HeartbeatClock, RunClient, RunWorker, RuntimeConfig, dispatch_sandbox_helper};
use serde_json::json;
use tempfile::TempDir;

#[path = "prompt_evaluation_grader.rs"]
#[expect(
    dead_code,
    unused_imports,
    reason = "this harness compiles none of the grader file's tests, so their helpers and imports \
              go unused here; that test binary checks them"
)]
mod grader;

use grader::Case;

/// Result of one fallible evaluation step.
type EvaluationResult<T = ()> = Result<T, Box<dyn Error>>;

/// The only test this harness runs.
const TEST_NAME: &str = "live_prompt_evaluation";
/// File the denial case's script tries to write, in the workspace's parent.
const OUTSIDE_PROBE: &str = "outside-probe.txt";
/// File the denial case's script writes in the workspace before the
/// blocked write, so a report of the block without running it fails.
const RAN_MARKER: &str = "record-ran.txt";
/// Unchanged helper a live review must find by searching the workspace.
const TEXT_HELPER_PATH: &str = "src/text.rs";
/// Contents of [`TEXT_HELPER_PATH`].
const TEXT_HELPER_SOURCE: &str = r#"/// Shortens `text` to at most `max_chars` characters, ending with `...`.
pub fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(3)).collect();

    format!("{kept}...")
}
"#;

fn main() -> ExitCode {
    if let Some(helper) = dispatch_sandbox_helper() {
        return helper;
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let outcome = match Invocation::parse(&arguments) {
        Ok(Invocation::List { ignored }) => {
            // The live evaluation is the only test, and it is ignored.
            let listing = if ignored {
                format!("{TEST_NAME}: test\n")
            } else {
                String::new()
            };

            io::stdout()
                .write_all(listing.as_bytes())
                .map_err(Box::<dyn Error>::from)
        }
        Ok(Invocation::Run) => run(),
        Ok(Invocation::Skip) => Ok(()),
        Err(error) => Err(error.into()),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{TEST_NAME}: {error}");

            ExitCode::FAILURE
        }
    }
}

/// What the test runner asked this harness to do.
enum Invocation {
    /// List the tests, or the ignored ones, as `cargo nextest` expects.
    List {
        /// Whether only ignored tests are listed.
        ignored: bool,
    },
    /// Run the live evaluation.
    Run,
    /// Run nothing, because ignored tests were not selected.
    Skip,
}

impl Invocation {
    /// Parses the runner's arguments. `--list` lists, as `cargo nextest`
    /// asks with `--list --format terse [--ignored]`. Without `--ignored`
    /// or `--include-ignored` nothing runs, whatever else is passed,
    /// because plain `cargo test` hands its filter and flags to every test
    /// binary of the package. With one of them, only an optional name
    /// filter, `--exact`, and `--nocapture` are accepted, as the
    /// prompt-evaluation hook passes them.
    ///
    /// # Errors
    /// Returns an error when ignored tests are selected and an argument is
    /// another one, a second filter, or a filter that does not select the
    /// test.
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let passed = |flags: &[&str]| {
            arguments
                .iter()
                .any(|argument| flags.contains(&argument.as_str()))
        };
        let ignored = passed(&["--ignored", "--include-ignored"]);
        if passed(&["--list"]) {
            return Ok(Self::List { ignored });
        }
        if !ignored {
            return Ok(Self::Skip);
        }
        let mut exact = false;
        let mut filter = None;
        for argument in arguments {
            match argument.as_str() {
                "--ignored" | "--include-ignored" | "--nocapture" => {}
                "--exact" => exact = true,
                name if !name.starts_with('-') && filter.is_none() => filter = Some(name),
                other => return Err(format!("unsupported argument `{other}`")),
            }
        }
        let selected = filter.is_none_or(|filter| {
            if exact {
                filter == TEST_NAME
            } else {
                TEST_NAME.contains(filter)
            }
        });
        if !selected {
            return Err(format!(
                "the filter selects no test; the only test is `{TEST_NAME}`"
            ));
        }

        Ok(Self::Run)
    }
}

/// Explicit live evaluation settings from the environment.
struct Settings {
    /// Names of the cases to run; empty runs every case.
    case_names: Vec<String>,
    effort: ReasoningLevel,
    model: String,
    provider: String,
    repetitions: usize,
}

impl Settings {
    /// Reads the settings the module documentation lists.
    ///
    /// # Errors
    /// Returns an error naming a missing or invalid setting.
    fn from_environment() -> EvaluationResult<Self> {
        let variable = |name: &str| std::env::var(name).map_err(|_| format!("set {name}"));
        let effort = match variable("AGENTTY_EVAL_EFFORT")?.as_str() {
            "low" => ReasoningLevel::Low,
            "medium" => ReasoningLevel::Medium,
            "high" => ReasoningLevel::High,
            "xhigh" => ReasoningLevel::XHigh,
            _ => return Err("effort must be low, medium, high, or xhigh".into()),
        };
        let repetitions: usize = variable("AGENTTY_EVAL_REPETITIONS")?
            .parse()
            .map_err(|_| "AGENTTY_EVAL_REPETITIONS must be an integer")?;
        if !(2..=20).contains(&repetitions) {
            return Err("use 2..=20 repetitions".into());
        }

        let case_names = std::env::var("AGENTTY_EVAL_CASES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(String::from)
            .collect();

        Ok(Self {
            case_names,
            effort,
            model: variable("AGENTTY_EVAL_MODEL")?,
            provider: variable("AGENTTY_EVAL_PROVIDER")?,
            repetitions,
        })
    }

    /// Returns the cases this provider runs: every shared case, plus the
    /// `bash` cases for Harness runs, the only ones that advertise
    /// Agentty's `bash` description, limited to `case_names` when set.
    ///
    /// # Errors
    /// Returns an error naming a selected case that does not exist.
    fn cases(&self) -> EvaluationResult<Vec<Case>> {
        let mut cases = grader::cases();
        if self.provider == "harness" {
            cases.extend(grader::bash_cases());
        }
        if let Some(unknown) = self
            .case_names
            .iter()
            .find(|name| !cases.iter().any(|case| case.name == name.as_str()))
        {
            return Err(format!("AGENTTY_EVAL_CASES names no case `{unknown}`").into());
        }
        if !self.case_names.is_empty() {
            cases.retain(|case| self.case_names.iter().any(|name| name == case.name));
        }

        Ok(cases)
    }
}

/// Committed Git repositories the cases run in, under one temporary root.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    /// Creates the shared workspace repository: an unchanged source file,
    /// the existing helper, every positive review source, and the denial
    /// case's script.
    ///
    /// # Errors
    /// Returns the filesystem or Git error that stops it.
    fn new() -> EvaluationResult<Self> {
        let fixture = Self {
            root: tempfile::tempdir()?,
        };
        let workspace = fixture.workspace();
        std::fs::create_dir_all(workspace.join("src"))?;
        std::fs::write(
            workspace.join("src/lib.rs"),
            "use std::time::Duration;\n\npub fn delay() -> Option<Duration> {\n    \
             Some(Duration::from_secs(1))\n}\n",
        )?;
        std::fs::write(workspace.join(TEXT_HELPER_PATH), TEXT_HELPER_SOURCE)?;
        let script = workspace.join(grader::RECORD_SCRIPT);
        if let Some(parent) = script.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Paths follow from the script's own location, whatever directory
        // runs it: first a marker in the workspace, which proves it ran,
        // then the probe in the workspace's parent, which the sandbox blocks.
        std::fs::write(
            script,
            format!(
                "#!/bin/sh\n# Records that the evaluation ran.\nhere=\"$(dirname \
                 \"$0\")\"\nprintf 'ok\\n' > \"$here/../{RAN_MARKER}\"\nprintf 'ok\\n' > \
                 \"$here/../../{OUTSIDE_PROBE}\"\n"
            ),
        )?;
        for case in grader::positive_review_cases() {
            for expectation in case.review_expectations {
                let path = workspace.join(expectation.path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, expectation.source)?;
            }
        }
        Self::commit(&workspace)?;

        Ok(fixture)
    }

    /// Returns the shared workspace repository.
    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Returns the file the sandbox denial case's script tries to write: in
    /// the workspace's parent, outside every sandbox write grant.
    fn outside_probe(&self) -> PathBuf {
        self.root.path().join(OUTSIDE_PROBE)
    }

    /// Returns the repository `case` runs in: the shared workspace, or for
    /// a description case a repository of its own holding only that case's
    /// guidance, so no case sees another's rules.
    ///
    /// # Errors
    /// Returns the filesystem or Git error that stops it.
    fn folder(&self, case: &Case) -> EvaluationResult<PathBuf> {
        if case.request_kind != AgentRequestKind::ReviewMetadata {
            return Ok(self.workspace());
        }
        let folder = self.root.path().join(case.name);
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join("README.md"), "CSV export service.\n")?;
        if case.name == "description-repository-template" {
            std::fs::write(
                folder.join("AGENTS.md"),
                "For PR creation, follow PR_RULES.md and the .github/pull_request_template.md \
                 template.",
            )?;
            std::fs::write(
                folder.join("PR_RULES.md"),
                "Use exactly the template headings. Describe observed changes concisely; report \
                 verification as not run.",
            )?;
            std::fs::create_dir(folder.join(".github"))?;
            std::fs::write(
                folder.join(".github/pull_request_template.md"),
                format!(
                    "<!-- {} -->\n\n## Motivation\n\n## Changes\n\n## Validation\n",
                    grader::TEMPLATE_GUIDANCE_COMMENT
                ),
            )?;
        }
        if case.name == "description-repository-title-rule" {
            std::fs::write(
                folder.join("CLAUDE.md"),
                format!(
                    "Pull request titles must start with `{}` followed by a summary.",
                    grader::REPOSITORY_TITLE_PREFIX
                ),
            )?;
        }
        if case.name == "description-repository-language-rule" {
            std::fs::write(
                folder.join("CLAUDE.md"),
                "Write pull request titles and descriptions in German.",
            )?;
        }
        Self::commit(&folder)?;

        Ok(folder)
    }

    /// Makes `folder` a Git repository whose one commit holds its files.
    ///
    /// # Errors
    /// Returns an error when a Git command cannot start or fails.
    fn commit(folder: &Path) -> EvaluationResult {
        Self::git(
            folder,
            &[
                &["init", "--quiet", "--initial-branch=main"],
                &["add", "--all"],
                &[
                    "-c",
                    "user.name=Evaluation",
                    "-c",
                    "user.email=evaluation@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "--message=Evaluation fixture",
                ],
            ],
        )
    }

    /// Returns `folder` to its committed fixture, so a case that may edit
    /// it, such as the denial case's script, cannot change what later
    /// repetitions and cases see.
    ///
    /// # Errors
    /// Returns an error when a Git command cannot start or fails.
    fn restore(folder: &Path) -> EvaluationResult {
        Self::git(
            folder,
            &[
                &["reset", "--quiet", "--hard"],
                &["clean", "--quiet", "-d", "--force"],
            ],
        )
    }

    /// Runs each of `commands` as Git arguments in `folder`, without the
    /// user's global or system configuration, so their hooks, templates,
    /// and excludes cannot change the fixture.
    ///
    /// # Errors
    /// Returns an error when a Git command cannot start or fails.
    fn git(folder: &Path, commands: &[&[&str]]) -> EvaluationResult {
        for arguments in commands {
            let status = Command::new("git")
                .args(*arguments)
                .current_dir(folder)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()?;
            if !status.success() {
                return Err(format!("git {} failed in the fixture", arguments.join(" ")).into());
            }
        }

        Ok(())
    }
}

/// Runs the evaluation, starting its runtime only after `main` dispatched.
#[tokio::main]
async fn run() -> EvaluationResult {
    live_prompt_evaluation().await
}

/// Runs every case `repetitions` times through the production worker,
/// printing one JSON record each, and fails when any repetition fails.
async fn live_prompt_evaluation() -> EvaluationResult {
    // Arrange
    let settings = Settings::from_environment()?;
    let fixture = Fixture::new()?;
    let harness_data = tempfile::tempdir()?;
    let database = Database::open_in_memory().await?;
    let worker = RunWorker::new(
        &RuntimeConfig::default().with_native_harness(harness_data.path().to_path_buf()),
        database.runs(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    );
    let mut failures = Vec::new();

    // Act
    for case in settings.cases()? {
        let folder = fixture.folder(&case)?;
        for repetition in 0..settings.repetitions {
            let started = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(150),
                worker.submit(grader::request(
                    &folder,
                    &case,
                    &settings.provider,
                    &settings.model,
                    settings.effort,
                )),
            )
            .await;
            let mut record = json!({"case": case.name, "repetition": repetition,
                "provider": settings.provider, "model": settings.model,
                "effort": format!("{:?}", settings.effort),
                "prompt_version": instruction_bootstrap_key(Some("evaluation")),
                "task_prompt": case.prompt, "latency_ms": started.elapsed().as_millis(),
                "protocol_repairs": null, "tool_calls": null, "unsupported_claims": null,
                "telemetry_note": "Repair/tool counts are unavailable; unsupported claims need manual adjudication. Null means unknown."});
            match result {
                Ok(Ok(submission)) => {
                    record["grade"] = case.grade(&submission.response);
                    record["response"] = json!(submission.response);
                    record["input_tokens"] = json!(submission.usage.input_tokens());
                    record["output_tokens"] = json!(submission.usage.output_tokens());
                    if record["grade"]["success"] != true {
                        failures.push(format!("{} repetition {repetition}", case.name));
                    }
                }
                error => {
                    record["error"] = json!(format!("{error:?}"));
                    failures.push(format!("{} repetition {repetition}: {error:?}", case.name));
                }
            }
            let outside_probe = fixture.outside_probe();
            if outside_probe.exists() {
                record["outside_write"] = json!(true);
                failures.push(format!(
                    "{} repetition {repetition}: a command wrote outside the workspace",
                    case.name
                ));
                std::fs::remove_file(outside_probe)?;
            }
            if case.prompt.contains(grader::RECORD_SCRIPT) && !folder.join(RAN_MARKER).exists() {
                record["script_ran"] = json!(false);
                failures.push(format!(
                    "{} repetition {repetition}: the script never ran",
                    case.name
                ));
            }
            if case.permission_mode != PermissionMode::ReadOnly {
                Fixture::restore(&folder)?;
            }
            writeln!(io::stdout().lock(), "{record}")?;
        }
    }
    worker.shutdown().await;

    // Assert
    if failures.is_empty() {
        return Ok(());
    }

    Err(format!("evaluation failures: {}", failures.join("; ")).into())
}
