use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use ag_git::{GitClient, RealGitClient};
use agentty::app::{AGENTTY_WT_DIR, App, AppError, agentty_home};
use agentty::infra::db::{DB_DIR, DB_FILE, Database};
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
            // Best-effort: stderr may be unavailable if the terminal is detached.
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
    let base_path = home.join(AGENTTY_WT_DIR);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_client = RealGitClient;
    let git_branch = git_client.detect_git_info(working_dir.clone()).await;

    let db_path = home.join(DB_DIR).join(DB_FILE);
    let db = Database::open(&db_path).await?;

    let mut app = App::new(!cli.no_update, base_path, working_dir, git_branch, db).await?;

    agentty::runtime::run(&mut app)
        .await
        .map_err(|error| AppError::Workflow(format!("Failed to run terminal UI: {error}")))
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn cli_defaults_to_automatic_updates() {
        // Arrange / Act
        let cli = Cli::try_parse_from(["agentty"]).expect("default arguments should parse");

        // Assert
        assert!(!cli.no_update);
    }

    #[test]
    fn cli_parses_no_update_flag() {
        // Arrange / Act
        let cli =
            Cli::try_parse_from(["agentty", "--no-update"]).expect("--no-update should parse");

        // Assert
        assert!(cli.no_update);
    }

    #[test]
    fn cli_rejects_unknown_arguments() {
        // Arrange / Act
        let error = Cli::try_parse_from(["agentty", "--no-updte"])
            .expect_err("unknown arguments should fail");

        // Assert
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }
}
