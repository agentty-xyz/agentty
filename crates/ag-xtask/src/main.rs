//! Workspace maintenance command-line tasks.

mod check_feature_artifact;
mod check_migration;

use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing::error;

#[cfg(not(test))]
const FEATURE_CONTENT_DIR: &str = "docs/site/content/features";
#[cfg(not(test))]
const FEATURE_STATIC_DIR: &str = "docs/site/static/features";

/// Command-line entry point for workspace maintenance tasks.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Supported maintenance subcommands.
#[derive(Subcommand)]
enum Command {
    /// Validates feature pages and their GIF, poster, and hash artifacts.
    CheckFeatureArtifacts,
    /// Validates SQL migration numbering across workspace crates.
    CheckMigrations,
}

/// Runs the selected maintenance command and returns the process exit code.
#[cfg(not(test))]
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    run_cli(
        &Cli::parse(),
        Path::new(FEATURE_CONTENT_DIR),
        Path::new(FEATURE_STATIC_DIR),
    )
}

/// Dispatch one parsed CLI invocation and map its result to a process code.
fn run_cli(cli: &Cli, content_dir: &Path, static_dir: &Path) -> ExitCode {
    let result = run_command(cli.command.as_ref(), content_dir, static_dir);

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Run one parsed maintenance command against the configured feature paths.
fn run_command(
    command: Option<&Command>,
    content_dir: &Path,
    static_dir: &Path,
) -> Result<(), String> {
    match command {
        None | Some(Command::CheckMigrations) => check_migration::run(),
        Some(Command::CheckFeatureArtifacts) => {
            check_feature_artifact::run(content_dir, static_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_feature_artifacts_command_uses_configured_directories() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&content_dir).expect("create content dir");
        std::fs::create_dir_all(&static_dir).expect("create static dir");

        // Act
        let exit_code = run_cli(
            &Cli {
                command: Some(Command::CheckFeatureArtifacts),
            },
            &content_dir,
            &static_dir,
        );

        // Assert
        assert_eq!(exit_code, ExitCode::SUCCESS);
    }

    #[test]
    fn check_feature_artifacts_failure_returns_failure_exit_code() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("missing-content");
        let static_dir = temp.path().join("missing-static");
        let cli = Cli {
            command: Some(Command::CheckFeatureArtifacts),
        };

        // Act
        let exit_code = run_cli(&cli, &content_dir, &static_dir);

        // Assert
        assert_eq!(exit_code, ExitCode::FAILURE);
    }

    #[test]
    fn default_command_checks_migrations() {
        // Arrange
        let content_dir = Path::new("unused-content");
        let static_dir = Path::new("unused-static");

        // Act
        let result = run_command(None, content_dir, static_dir);

        // Assert
        assert!(result.is_ok());
    }
}
