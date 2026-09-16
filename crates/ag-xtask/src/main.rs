//! Workspace maintenance command-line tasks.

mod check_instruction;
mod check_migration;

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
    /// Validates instruction references, aliases, and documented hook names.
    CheckInstructions,
    /// Validates SQL migration numbering across workspace crates.
    CheckMigrations,
}

/// Runs the selected maintenance command and returns the process exit code.
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::CheckInstructions) => check_instruction::run(),
        None | Some(Command::CheckMigrations) => check_migration::run(),
    };

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
