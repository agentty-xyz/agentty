//! Agentty command-line entry point and terminal runtime bootstrap.

use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ag_git::{GitClient, RealGitClient};
use agentty::analytics::Analytics;
#[cfg(not(debug_assertions))]
use agentty::analytics::TELEMETRY_ENABLED_ENV;
use agentty::app::{AGENTTY_WT_DIR, App, AppError, agentty_home};
#[cfg(not(debug_assertions))]
use agentty::infra::db::AppRepositories;
use agentty::infra::db::{
    DB_DIR, DB_FILE, Database, acquire_instance_lock,
    timestamp_source_from_environment as environment_timestamp_source,
};
use agentty::infra::telemetry::Telemetry;
use clap::Parser;

/// Command-line options for launching Agentty.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Disables automatic application updates.
    #[arg(long)]
    no_update: bool,
    /// Exports traces using OTLP HTTP/protobuf to this complete traces URL.
    #[arg(long, value_name = "URL")]
    otlp_endpoint: Option<String>,
}

/// Runs the `agentty` application runtime using the configured workspace and
/// database.
#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli, agentty::runtime::run).await {
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
async fn run(
    cli: Cli,
    runtime: impl for<'a> AsyncFnOnce(&'a mut App) -> io::Result<()>,
) -> Result<(), AppError> {
    let telemetry = Telemetry::start(cli.otlp_endpoint.as_deref())
        .await
        .map_err(AppError::Workflow)?;
    let result = async {
        if let Some(telemetry) = &telemetry {
            telemetry.install().map_err(AppError::Workflow)?;
        }
        run_application(cli, runtime).await
    }
    .await;
    if let Some(telemetry) = telemetry {
        let warnings = telemetry.shutdown().await;
        if warnings > 0 {
            let _ = writeln!(
                io::stderr().lock(),
                "OTLP export reported {warnings} warnings or failures; some traces may be missing."
            );
        }
    }

    result
}

async fn run_with_analytics(
    application: impl Future<Output = Result<(), AppError>>,
    analytics: Option<&Analytics>,
) -> Result<(), AppError> {
    let launch = async {
        if let Some(analytics) = analytics {
            analytics.record_launch().await;
        }
    };
    tokio::pin!(application);
    tokio::pin!(launch);
    let mut launch_completed = false;
    let result = tokio::select! {
        result = &mut application => result,
        () = &mut launch => {
            launch_completed = true;
            application.await
        }
    };

    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        let finish_launch = async {
            if !launch_completed {
                launch.await;
            }
        };
        let report_failure = async {
            if let Err(error) = &result
                && let Some(analytics) = analytics
            {
                analytics.record_failure(error).await;
            }
        };
        tokio::join!(finish_launch, report_failure);
    })
    .await;

    result
}

fn map_runtime_result(result: io::Result<()>) -> Result<(), AppError> {
    result.map_err(|error| AppError::Workflow(format!("Failed to run terminal UI: {error}")))
}

async fn run_application(
    cli: Cli,
    runtime: impl for<'a> AsyncFnOnce(&'a mut App) -> io::Result<()>,
) -> Result<(), AppError> {
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
    // Debug builds, including tests, never report to the public project.
    #[cfg(debug_assertions)]
    let analytics: Option<Analytics> = None;
    #[cfg(not(debug_assertions))]
    let analytics = if Analytics::is_enabled(std::env::var_os(TELEMETRY_ENABLED_ENV).as_deref()) {
        Analytics::posthog(&AppRepositories::from(db.clone())).await
    } else {
        None
    };

    run_with_analytics(
        Box::pin(async {
            let mut app = App::new(!cli.no_update, base_path, working_dir, git_branch, db).await?;
            app.set_analytics(analytics.clone());

            map_runtime_result(runtime(&mut app).await)
        }),
        analytics.as_ref(),
    )
    .await
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
