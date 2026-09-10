//! Language-agnostic command-line front end for the `testty` framework.
//!
//! This binary ships inside the `testty` crate so non-Rust projects can drive
//! TUI end-to-end scenarios without writing Rust test harnesses. Installing the
//! crate (`cargo install testty`) provides the `testty` executable, which Cargo
//! auto-detects from this `src/main.rs` alongside the library `src/lib.rs`.
//!
//! The binary exposes the full `testty` verb tree (`run`, `schema`,
//! `proof open`, `proof gallery`, `update`).
//!
//! `run` is implemented: it loads a YAML scenario, lowers it onto the engine
//! via [`testty::spec`], drives the binary under test, and reports pass/fail
//! through the process exit code. The remaining verbs are still stubbed — each
//! reports "not yet implemented" on stderr and exits non-zero so callers and CI
//! can detect that the behavior is not wired up yet. Later tasks replace each
//! remaining [`Command::dispatch`] arm with real logic that delegates into the
//! `testty` library API.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use testty::spec::model::ScenarioSpec;

/// Top-level command-line interface for the `testty` runner.
#[derive(Parser)]
#[command(name = "testty", about, version)]
struct Cli {
    /// Selected verb to execute.
    #[command(subcommand)]
    command: Command,
}

/// Supported `testty` verbs.
///
/// Every variant is dispatched through [`Command::dispatch`], which currently
/// returns [`ExitCode::FAILURE`] for all verbs until the behavior is filled in.
#[derive(Subcommand)]
enum Command {
    /// Runs a scenario file against a TUI binary.
    Run {
        /// Path to the scenario definition (for example, `scenario.yaml`).
        scenario: PathBuf,
        /// Path to the binary under test, overriding the scenario default.
        #[arg(long)]
        bin: Option<PathBuf>,
        /// Output directory for generated proof artifacts.
        #[arg(long)]
        proof: Option<PathBuf>,
    },
    /// Prints the JSON schema for scenario files.
    Schema,
    /// Inspects proof artifacts produced by a run.
    Proof {
        /// Selected proof subcommand.
        #[command(subcommand)]
        command: ProofCommand,
    },
    /// Updates stored scenario snapshots to match current output.
    Update,
}

impl Command {
    /// Executes the selected verb and returns the process exit code.
    ///
    /// `run` executes a YAML scenario via [`run_scenario`]; the remaining arms
    /// are stubbed and return [`ExitCode::FAILURE`] with a "not yet
    /// implemented" notice. Replace the stubbed arms as the verbs are
    /// implemented.
    fn dispatch(self) -> ExitCode {
        match self {
            Command::Run {
                scenario,
                bin,
                proof,
            } => run_scenario(&scenario, bin.as_deref(), proof.as_deref()),
            Command::Schema => not_implemented("schema"),
            Command::Proof {
                command: ProofCommand::Open { .. },
            } => not_implemented("proof open"),
            Command::Proof {
                command: ProofCommand::Gallery { .. },
            } => not_implemented("proof gallery"),
            Command::Update => not_implemented("update"),
        }
    }
}

/// Subcommands for inspecting proof artifacts.
#[derive(Subcommand)]
enum ProofCommand {
    /// Opens a single proof report in the browser.
    Open {
        /// Path to the proof HTML report.
        html: PathBuf,
    },
    /// Builds a gallery from a directory of proof reports.
    Gallery {
        /// Directory containing proof reports.
        dir: PathBuf,
    },
}

/// Parses arguments and dispatches the selected verb.
fn main() -> ExitCode {
    let cli = Cli::parse();

    cli.command.dispatch()
}

/// Loads a YAML scenario, runs it against the binary, and reports the outcome.
///
/// Binds the production diagnostic sink (locked stderr) and delegates to
/// [`run_scenario_reporting`]. The process exit code is the pass/fail signal
/// for non-Rust CI.
fn run_scenario(
    scenario_path: &Path,
    bin_override: Option<&Path>,
    proof: Option<&Path>,
) -> ExitCode {
    let mut stderr = io::stderr().lock();

    run_scenario_reporting(scenario_path, bin_override, proof, &mut stderr)
}

/// Runs a YAML scenario and writes every diagnostic to `out`.
///
/// `bin_override` replaces the scenario's `session.bin` (the `--bin` flag).
/// Routing diagnostics through an injected writer (instead of writing to
/// stderr directly) keeps the messages observable in tests while still
/// avoiding the `print_stderr` lint. The exit code is `SUCCESS` when every
/// expectation passes, `FAILURE` on any failed expectation, parse error, or
/// run error.
fn run_scenario_reporting(
    scenario_path: &Path,
    bin_override: Option<&Path>,
    proof: Option<&Path>,
    out: &mut dyn Write,
) -> ExitCode {
    let text = match fs::read_to_string(scenario_path) {
        Ok(text) => text,
        Err(err) => {
            let _ = writeln!(
                out,
                "testty: cannot read {}: {err}",
                scenario_path.display()
            );

            return ExitCode::FAILURE;
        }
    };

    let mut spec = match ScenarioSpec::from_yaml(&text) {
        Ok(spec) => spec,
        Err(err) => {
            let _ = writeln!(out, "testty: {err}");

            return ExitCode::FAILURE;
        }
    };

    if let Some(bin) = bin_override {
        spec.session.bin = bin.to_path_buf();
    }

    if proof.is_some() {
        let _ = writeln!(
            out,
            "testty: --proof is not yet supported; running without proof output"
        );
    }

    let (_frame, failures) = match spec.lower().run() {
        Ok(result) => result,
        Err(err) => {
            let _ = writeln!(out, "testty: scenario failed to run: {err}");

            return ExitCode::FAILURE;
        }
    };

    if failures.is_empty() {
        let _ = writeln!(out, "testty: scenario passed");

        return ExitCode::SUCCESS;
    }

    for failure in &failures {
        let _ = writeln!(out, "FAIL: {}", failure.message);
    }
    let _ = writeln!(out, "testty: {} expectation(s) failed", failures.len());

    ExitCode::FAILURE
}

/// Reports an unimplemented verb on stderr and returns a failing exit code.
///
/// Writing directly to stderr (instead of `eprintln!`) keeps the stub free of
/// the `print_stderr` lint while still surfacing the notice to callers.
fn not_implemented(verb: &str) -> ExitCode {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "testty: `{verb}` is not yet implemented");

    ExitCode::FAILURE
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
