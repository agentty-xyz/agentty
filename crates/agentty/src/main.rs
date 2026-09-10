//! Agentty command-line entry point and terminal runtime bootstrap.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use ag_git::{GitClient, RealGitClient};
use agentty::app::{AGENTTY_WT_DIR, App, AppError, agentty_home};
use agentty::infra::db::{
    DB_DIR, DB_FILE, Database, acquire_instance_lock,
    timestamp_source_from_environment as environment_timestamp_source,
};
use clap::Parser;

/// Command-line options for launching Agentty.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Disables automatic application updates.
    #[arg(long)]
    no_update: bool,
}

/// Runs the `agentty` application runtime using the configured workspace and
/// database.
#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");

            ExitCode::FAILURE
        }
    }
}

/// Builds startup dependencies, then launches the `agentty` runtime.
///
/// # Errors
/// Returns an error if database startup, app construction, or runtime
/// execution fails.
async fn run(cli: Cli) -> Result<(), AppError> {
    let home = agentty_home();
    let _instance_lock = acquire_instance_lock(&home).await.map_err(|error| {
        let message = if error.kind() == io::ErrorKind::WouldBlock {
            "Another Agentty instance is already using this Agentty root. Close it before starting \
             another instance."
                .to_string()
        } else {
            format!("Failed to acquire the Agentty instance lock: {error}")
        };

        AppError::Workflow(message)
    })?;
    let base_path = home.join(AGENTTY_WT_DIR);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_client = RealGitClient;
    let git_branch = git_client.detect_git_info(working_dir.clone()).await;

    let db_path = home.join(DB_DIR).join(DB_FILE);
    let db = Database::open_with_timestamp_source(&db_path, environment_timestamp_source()).await?;

    let mut app = App::new(!cli.no_update, base_path, working_dir, git_branch, db).await?;

    agentty::runtime::run(&mut app)
        .await
        .map_err(|error| AppError::Workflow(format!("Failed to run terminal UI: {error}")))
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
