//! Agentty command-line entry point and terminal runtime bootstrap.

use std::fs::File;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use ag_git::{GitClient, RealGitClient};
use ag_telemetry::otlp::{OtlpError, OtlpExport, Service};
use ag_telemetry::{CaptureToolContent, Context, FutureExt as _, KeyValue, Outcome, Span};
use agentty::analytics::Analytics;
use agentty::app::{AGENTTY_WT_DIR, App, AppError, DEFAULT_REMOTE_CONTROL_PORT, agentty_home};
use agentty::infra::db::{
    AppRepositories, DB_DIR, DB_FILE, Database, acquire_instance_lock,
    timestamp_source_from_environment as environment_timestamp_source,
};
use clap::Parser;

/// Command-line options for launching Agentty.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Disables `PostHog` usage telemetry; OTLP export to `--otlp-endpoint` is
    /// unaffected.
    #[arg(long)]
    disable_telemetry: bool,
    /// Preview features enabled for this launch.
    #[command(flatten)]
    experimental: ExperimentalFeatures,
    /// Disables automatic application updates.
    #[arg(long)]
    no_update: bool,
    /// Includes bounded tool commands and results, with common credentials
    /// redacted.
    #[arg(long, requires = "otlp_endpoint")]
    otlp_capture_content: bool,
    /// Exports traces using OTLP HTTP/protobuf to this complete traces URL.
    #[arg(long, value_name = "URL")]
    otlp_endpoint: Option<String>,
    /// Loopback port that remote control listens on; `0` picks a free port.
    #[arg(
        long,
        value_name = "PORT",
        default_value_t = DEFAULT_REMOTE_CONTROL_PORT,
        requires = "remote"
    )]
    remote_control_port: u16,
    /// Host name a tunnel uses to reach remote control, such as a
    /// Tailscale machine name; repeat for several. Loopback names are always
    /// accepted.
    #[arg(long, value_name = "HOST", requires = "remote")]
    remote_control_host: Vec<String>,
    /// Relay that remote control registers with, such as
    /// `wss://relay.example`, so devices can pair and connect without a
    /// tunnel. Traffic through it is encrypted end to end.
    #[arg(long, value_name = "URL", requires = "remote", value_parser = parse_relay_url)]
    remote_relay: Option<String>,
}

/// Opt-in preview features, disabled unless their launch flags are present.
#[derive(Debug, Default, clap::Args)]
struct ExperimentalFeatures {
    /// Offers the preview `ag-harness` agent in `/model`, which runs the
    /// built-in agent loop on a local Ollama or OpenAI-compatible model, or a
    /// hosted model when its provider's API key, such as `MODEL_API_KEY`, is
    /// set.
    #[arg(long = "experimental-harness")]
    harness: bool,
    /// Offers the Remote tab, which lets a paired phone or browser follow
    /// and reply to sessions through a loopback listener.
    #[arg(long = "experimental-remote")]
    remote: bool,
}

/// Runs the `agentty` application runtime using the configured workspace and
/// database.
#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
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
    runtime: impl for<'a> AsyncFnOnce(&'a mut App) -> Result<(), AppError>,
) -> Result<(), AppError> {
    if let Some(endpoint) = cli.otlp_endpoint.as_deref() {
        OtlpExport::validate(endpoint).map_err(otlp_startup_error)?;
    }
    let context = Context::current().with_value(CaptureToolContent(cli.otlp_capture_content));

    run_application(cli, runtime).with_context(context).await
}

/// Accepts a relay base URL: `ws://` or `wss://`, a host without
/// credentials, and no query or fragment, since the pairing link appends its
/// own fragment. A trailing slash is dropped.
fn parse_relay_url(value: &str) -> Result<String, String> {
    let url = value.trim().trim_end_matches('/');
    let host = url
        .strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .unwrap_or_default();
    let authority = host.split('/').next().unwrap_or_default();
    // Credentials would end up in the pairing link and its QR code.
    let is_valid = !authority.is_empty()
        && !authority.contains('@')
        && !host.contains(['?', '#'])
        && !host.contains(char::is_whitespace);
    if !is_valid {
        return Err("expected a relay URL such as wss://relay.example".to_string());
    }

    Ok(url.to_string())
}

/// Reports an OTLP setup failure using its bounded, credential-free message.
fn otlp_startup_error(error: OtlpError) -> AppError {
    AppError::Workflow(error.to_string())
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

/// Opens the Agentty root, then starts OTLP export after the database so its
/// resource carries the stored installation ID, and flushes it after the
/// runtime ends.
///
/// One independent `app.startup` trace covers startup through application
/// assembly, failing when assembly fails; the runtime runs outside it. Export
/// only starts after the database opens, so the root is backdated to the
/// launch and records the database open time as `agentty.db.open_ms`;
/// failures before export starts are not traced.
async fn run_application(
    cli: Cli,
    runtime: impl for<'a> AsyncFnOnce(&'a mut App) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let launch_started = SystemTime::now();
    let Launch {
        base_path,
        db,
        db_open,
        git_branch,
        instance_lock: _instance_lock,
        working_dir,
    } = Launch::open().await?;
    // Debug builds, including tests, never report to the public project.
    let reports_to_posthog = cfg!(not(debug_assertions)) && !cli.disable_telemetry;
    let installation_id = if reports_to_posthog || cli.otlp_endpoint.is_some() {
        Some(Analytics::installation_id(&AppRepositories::from(db.clone())).await)
    } else {
        None
    };
    let posthog = installation_id
        .as_deref()
        .filter(|_| reports_to_posthog)
        .and_then(Analytics::posthog);
    let telemetry = OtlpExport::start_with_events(
        cli.otlp_endpoint.as_deref(),
        Service {
            name: "agentty",
            user_id: installation_id,
            version: env!("CARGO_PKG_VERSION"),
        },
    )
    .await
    .map_err(otlp_startup_error)?;
    if let Some(telemetry) = &telemetry {
        telemetry.install();
    }
    let startup = Span::root_at(
        "app.startup",
        vec![KeyValue::new(
            "agentty.db.open_ms",
            i64::try_from(db_open.as_millis()).unwrap_or(i64::MAX),
        )],
        launch_started,
    );
    let trace = startup.context();
    let analytics = Analytics::mirror(posthog, telemetry.as_ref().and_then(OtlpExport::event_log));

    let result = run_with_analytics(
        Box::pin(async {
            let app = App::new(
                !cli.no_update,
                cli.experimental.harness,
                base_path,
                working_dir,
                git_branch,
                db,
            )
            .with_context(trace)
            .await;
            startup.finish(app.as_ref().map_or(Outcome::Failed, |_| Outcome::Completed));
            let mut app = app?;
            app.set_analytics(analytics.clone());
            if cli.experimental.remote {
                app.enable_remote_control(
                    cli.remote_control_port,
                    cli.remote_control_host,
                    cli.remote_relay,
                )
                .await;
            }

            runtime(&mut app).await
        }),
        analytics.as_ref(),
    )
    .await;
    if let Some(telemetry) = telemetry {
        let warnings = telemetry.shutdown().await;
        if warnings > 0 {
            let _ = writeln!(
                io::stderr().lock(),
                "OTLP export reported {warnings} warnings or failures; some traces or usage \
                 events may be missing."
            );
        }
    }

    result
}

/// Startup state acquired before the application is assembled.
struct Launch {
    base_path: PathBuf,
    db: Database,
    /// Time spent opening and migrating the database.
    db_open: Duration,
    git_branch: Option<String>,
    /// Held until the application stops; see [`acquire_instance_lock`].
    instance_lock: File,
    working_dir: PathBuf,
}

impl Launch {
    /// Acquires the instance lock, resolves the launch repository, and opens
    /// the migrated database, timing the database open.
    ///
    /// # Errors
    /// Returns an error when another instance owns the Agentty root, or the
    /// lock or database cannot be opened.
    async fn open() -> Result<Self, AppError> {
        let home = agentty_home();
        let instance_lock = acquire_instance_lock(&home).await.map_err(|error| {
            let message = if error.kind() == io::ErrorKind::WouldBlock {
                "Another Agentty instance is already using this Agentty root. Close it before \
                 starting another instance."
                    .to_string()
            } else {
                format!("Failed to acquire the Agentty instance lock: {error}")
            };

            AppError::Workflow(message)
        })?;
        let base_path = home.join(AGENTTY_WT_DIR);
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let git_client = RealGitClient;
        let working_dir = git_client
            .find_git_repo_root(working_dir.clone())
            .await
            .unwrap_or(working_dir);
        let git_branch = git_client.detect_git_info(working_dir.clone()).await;

        let db_path = home.join(DB_DIR).join(DB_FILE);
        let db_open_started = Instant::now();
        let db =
            Database::open_with_timestamp_source(&db_path, environment_timestamp_source()).await?;
        let db_open = db_open_started.elapsed();

        Ok(Self {
            base_path,
            db,
            db_open,
            git_branch,
            instance_lock,
            working_dir,
        })
    }
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
