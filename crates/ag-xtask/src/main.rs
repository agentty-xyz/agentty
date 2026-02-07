mod check_index;
mod check_migration;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing::error;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    CheckIndexes,
    CheckMigrations,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        None => check_index::run().and(check_migration::run()),
        Some(Command::CheckIndexes) => check_index::run(),
        Some(Command::CheckMigrations) => check_migration::run(),
    };

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
