//! Workspace maintenance command-line tasks.

mod affected_clippy;
mod cargo_environment;
mod check_coverage;
mod check_execution_boundary;
mod check_instruction;
mod check_manifest;
mod check_migration;
mod check_module_router;
mod e2e_container;
mod format_changed;
mod slow_test;
mod timing;
mod validation;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing::error;

/// Command-line entry point for workspace maintenance tasks.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Supported maintenance subcommands.
#[derive(Subcommand)]
enum Command {
    /// Runs strict Clippy, or `cargo check` with `--check`, for the packages
    /// changed files can affect.
    Clippy(affected_clippy::Options),
    /// Reports every uncovered line and function in an LCOV report.
    #[command(name = "check-coverage")]
    Coverage(check_coverage::Options),
    /// Validates that model execution flows only through worker-owned crates.
    #[command(name = "check-execution-boundary")]
    ExecutionBoundary,
    /// Validates instruction references, aliases, and documented hook names.
    #[command(name = "check-instructions")]
    Instructions,
    /// Validates that member manifests inherit workspace dependencies, do not
    /// enable Tokio's `full` feature, and avoid banned dependencies.
    #[command(name = "check-manifests")]
    Manifests,
    /// Validates SQL migration names and numbering across workspace crates and
    /// rejects edits to migrations that exist at the branch fork point.
    #[command(name = "check-migrations")]
    Migrations,
    /// Validates that module files paired with a same-named directory only
    /// declare child modules and re-exports.
    #[command(name = "check-module-routers")]
    ModuleRouters,
    /// Compares repeated CI timing artifacts with matching provenance.
    CompareTimings(timing::Options),
    /// Runs Agentty E2E tests in the pinned image, one local run at a time.
    E2eContainer,
    /// Formats Rust files changed since the branch fork point.
    Fmt(format_changed::Options),
    /// Reports slow tests in a nextest `JUnit` report and checks a duration
    /// threshold against an allowlist.
    SlowTests(slow_test::Options),
    /// Runs cataloged checks with package selection and target coordination.
    Validate(validation::Options),
}

/// Runs the selected maintenance command and returns the process exit code.
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Clippy(options)) => affected_clippy::run(&options),
        Some(Command::Coverage(options)) => check_coverage::run(&options),
        Some(Command::ExecutionBoundary) => check_execution_boundary::run(),
        Some(Command::Instructions) => check_instruction::run(),
        Some(Command::Manifests) => check_manifest::run(),
        None | Some(Command::Migrations) => check_migration::run(),
        Some(Command::ModuleRouters) => check_module_router::run(),
        Some(Command::CompareTimings(options)) => timing::run(&options),
        Some(Command::E2eContainer) => e2e_container::run(),
        Some(Command::Fmt(options)) => format_changed::run(&options),
        Some(Command::SlowTests(options)) => slow_test::run(&options),
        Some(Command::Validate(options)) => validation::run(options),
    };

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
