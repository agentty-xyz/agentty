//! Workspace maintenance command-line tasks.

mod affected_clippy;
mod check_execution_boundary;
mod check_instruction;
mod check_migration;
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
    /// Runs strict Clippy for the packages changed files can affect.
    Clippy(affected_clippy::Options),
    /// Validates that model execution flows only through worker-owned crates.
    #[command(name = "check-execution-boundary")]
    ExecutionBoundary,
    /// Validates instruction references, aliases, and documented hook names.
    #[command(name = "check-instructions")]
    Instructions,
    /// Validates SQL migration numbering across workspace crates.
    #[command(name = "check-migrations")]
    Migrations,
    /// Compares repeated CI timing artifacts with matching provenance.
    CompareTimings(timing::Options),
    /// Runs cataloged checks with package selection and target coordination.
    Validate(validation::Options),
}

/// Runs the selected maintenance command and returns the process exit code.
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Clippy(options)) => affected_clippy::run(&options),
        Some(Command::ExecutionBoundary) => check_execution_boundary::run(),
        Some(Command::Instructions) => check_instruction::run(),
        None | Some(Command::Migrations) => check_migration::run(),
        Some(Command::CompareTimings(options)) => timing::run(&options),
        Some(Command::Validate(options)) => validation::run(options),
    };

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
