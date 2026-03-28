mod check_migration;
mod roadmap;
mod workspace_map;

use std::io::{self, Write};
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
    /// Validates SQL migration numbering across workspace crates.
    CheckMigrations,
    /// Writes the generated workspace map used by tooling and agents.
    WorkspaceMap,
    /// Runs roadmap maintenance commands.
    Roadmap {
        #[command(subcommand)]
        command: RoadmapCommand,
    },
}

/// Roadmap maintenance commands.
#[derive(Subcommand)]
enum RoadmapCommand {
    /// Validates roadmap structure and queue rules.
    Lint,
    /// Prints a roadmap-oriented digest of the current repository context.
    ContextDigest,
}

/// Runs the selected maintenance command and returns the process exit code.
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        None => check_migration::run().and(roadmap::lint()),
        Some(Command::WorkspaceMap) => workspace_map::run(),
        Some(Command::CheckMigrations) => check_migration::run(),
        Some(Command::Roadmap {
            command: RoadmapCommand::Lint,
        }) => roadmap::lint(),
        Some(Command::Roadmap {
            command: RoadmapCommand::ContextDigest,
        }) => roadmap::context_digest().and_then(|output| write_stdout(&output)),
    };

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Writes command output to stdout without using `println!`.
///
/// # Errors
/// Returns an error when stdout cannot be written.
fn write_stdout(output: &str) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(output.as_bytes())
        .map_err(|err| err.to_string())?;
    stdout.write_all(b"\n").map_err(|err| err.to_string())
}
