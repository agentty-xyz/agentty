use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use std::{env, fs};

use agentty::analytics::Analytics;
use agentty::app::{App, AppError, agentty_home};
use agentty::infra::db::{DB_DIR, DB_FILE, acquire_instance_lock};
use clap::Parser;
use clap::error::ErrorKind;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Cli, map_runtime_result, run, run_with_analytics};

const LOCK_FAILURE_CHILD_ENV: &str = "AGENTTY_LOCK_FAILURE_CHILD";
const OWNED_ROOT_CHILD_ENV: &str = "AGENTTY_OWNED_ROOT_CHILD";
const DATABASE_FAILURE_CHILD_ENV: &str = "AGENTTY_DATABASE_FAILURE_CHILD";
const TELEMETRY_STARTUP_CHILD_ENV: &str = "AGENTTY_TELEMETRY_STARTUP_CHILD";

async fn closed_runtime(_app: &mut App) -> std::io::Result<()> {
    Err(std::io::Error::other("terminal closed"))
}

async fn traced_closed_runtime(app: &mut App) -> std::io::Result<()> {
    ag_telemetry::Span::root("startup-test", Vec::new())
        .scope(async {})
        .await;

    closed_runtime(app).await
}

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
    let cli = Cli::try_parse_from(["agentty", "--no-update"]).expect("--no-update should parse");

    // Assert
    assert!(cli.no_update);
}

#[test]
fn cli_requires_an_explicit_otlp_endpoint() {
    // Arrange / Act
    let disabled = Cli::try_parse_from(["agentty"]).expect("default arguments");
    let enabled = Cli::try_parse_from([
        "agentty",
        "--otlp-endpoint",
        "http://localhost:4318/v1/traces",
    ])
    .expect("OTLP arguments");
    let missing = Cli::try_parse_from(["agentty", "--otlp-endpoint"]);

    // Assert
    assert!(disabled.otlp_endpoint.is_none());
    assert_eq!(
        enabled.otlp_endpoint.as_deref(),
        Some("http://localhost:4318/v1/traces")
    );
    assert!(missing.is_err());
}

#[test]
fn cli_rejects_unknown_arguments() {
    // Arrange / Act
    let error =
        Cli::try_parse_from(["agentty", "--no-updte"]).expect_err("unknown arguments should fail");

    // Assert
    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn runtime_errors_keep_the_terminal_context() {
    // Arrange
    let error = std::io::Error::other("terminal closed");

    // Act
    let result = map_runtime_result(Err(error));

    // Assert
    assert!(matches!(&result, Err(AppError::Workflow(_))));
    assert!(
        result
            .expect_err("terminal failure")
            .to_string()
            .contains("Failed to run terminal UI: terminal closed")
    );
}

#[tokio::test]
async fn telemetry_wrapper_preserves_application_results() {
    // Arrange
    let analytics =
        Analytics::new("token", "http://127.0.0.1:0", "installation").expect("enabled telemetry");

    // Act
    let success = run_with_analytics(async { Ok(()) }, Some(&analytics)).await;
    let disabled = run_with_analytics(async { Ok(()) }, None).await;
    let failure = run_with_analytics(
        async { Err(AppError::Workflow("private error".to_string())) },
        Some(&analytics),
    )
    .await;

    // Assert
    assert!(success.is_ok());
    assert!(disabled.is_ok());
    assert!(matches!(failure, Err(AppError::Workflow(_))));
}

#[tokio::test]
async fn startup_failure_does_not_wait_for_unresponsive_telemetry_host() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let host = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let analytics = Analytics::new("token", &host, "installation").expect("configured destination");

    // Act
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        run_with_analytics(
            async { Err(AppError::Workflow("startup failure".to_string())) },
            Some(&analytics),
        ),
    )
    .await;

    // Assert
    assert!(matches!(outcome, Ok(Err(AppError::Workflow(_)))));
}

#[tokio::test]
async fn run_reports_instance_lock_parent_creation_failure() {
    if env::var_os(LOCK_FAILURE_CHILD_ENV).is_some() {
        // Arrange
        let cli = Cli {
            no_update: false,
            otlp_endpoint: None,
        };

        // Act
        let error = run(cli, closed_runtime)
            .await
            .expect_err("startup should reject a file-backed root");

        // Assert
        assert!(matches!(error, AppError::Workflow(_)));
        assert!(
            error
                .to_string()
                .contains("Failed to acquire the Agentty instance lock")
        );

        return;
    }

    // Arrange
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let blocking_root = temp_dir.path().join("agentty-root");
    tokio::fs::write(&blocking_root, b"")
        .await
        .expect("blocking root file should be created");
    let test_binary = env::current_exe().expect("test binary path should resolve");

    // Act
    let status = tokio::process::Command::new(test_binary)
        .arg("--exact")
        .arg("tests::run_reports_instance_lock_parent_creation_failure")
        .env(LOCK_FAILURE_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", blocking_root)
        .status()
        .await
        .expect("isolated startup test should run");

    // Assert
    assert!(status.success());
}

#[tokio::test]
async fn run_rejects_an_owned_root_before_opening_the_database() {
    if env::var_os(OWNED_ROOT_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(
            Cli {
                no_update: true,
                otlp_endpoint: None,
            },
            closed_runtime,
        )
        .await
        .expect_err("root is owned");

        // Assert
        assert!(
            error
                .to_string()
                .contains("Another Agentty instance is already using")
        );

        return;
    }

    // Arrange
    let root = tempfile::tempdir().expect("root");
    let owner = acquire_instance_lock(root.path())
        .await
        .expect("owner lock");
    let database_path = root.path().join(DB_DIR).join(DB_FILE);

    // Act
    let status = tokio::process::Command::new(env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("tests::run_rejects_an_owned_root_before_opening_the_database")
        .env(OWNED_ROOT_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", root.path())
        .status()
        .await
        .expect("contending process");

    // Assert
    assert!(status.success());
    assert!(
        !database_path.exists(),
        "contention must precede database creation"
    );
    drop(owner);
}

#[tokio::test]
async fn run_releases_instance_lock_after_database_open_failure() {
    if env::var_os(DATABASE_FAILURE_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(
            Cli {
                no_update: true,
                otlp_endpoint: None,
            },
            closed_runtime,
        )
        .await
        .expect_err("database path is a directory");

        // Assert
        assert!(matches!(error, AppError::Db(_)));
        let _owner = acquire_instance_lock(&agentty_home())
            .await
            .expect("failed startup must release ownership");

        return;
    }

    // Arrange
    let root = tempfile::tempdir().expect("root");
    tokio::fs::create_dir_all(root.path().join(DB_DIR).join(DB_FILE))
        .await
        .expect("block database opening");

    // Act
    let status = tokio::process::Command::new(env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("tests::run_releases_instance_lock_after_database_open_failure")
        .env(DATABASE_FAILURE_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", root.path())
        .status()
        .await
        .expect("startup process");

    // Assert
    assert!(status.success());
}

#[tokio::test]
async fn startup_runs_application_through_telemetry_wrapper() {
    if env::var_os(TELEMETRY_STARTUP_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(
            Cli {
                no_update: true,
                otlp_endpoint: env::var("AGENTTY_TEST_OTLP_ENDPOINT").ok(),
            },
            traced_closed_runtime,
        )
        .await
        .expect_err("injected runtime failure");

        // Assert
        assert!(
            error
                .to_string()
                .contains("Failed to run terminal UI: terminal closed")
        );

        return;
    }

    // Arrange
    let root = tempfile::tempdir().expect("root");
    let stub_bin = root.path().join("stub-bin");
    fs::create_dir_all(&stub_bin).expect("stub bin directory");
    let codex_stub = stub_bin.join("codex");
    fs::write(&codex_stub, "#!/bin/sh\nexit 0\n").expect("codex stub");
    fs::set_permissions(&codex_stub, fs::Permissions::from_mode(0o750))
        .expect("executable codex stub");
    let child_path = format!("{}:/usr/bin:/bin", stub_bin.display());

    for response_status in [None, Some(200), Some(503)] {
        let server = MockServer::start().await;
        let mut child = tokio::process::Command::new(env::current_exe().expect("test binary"));
        child
            .args([
                "--exact",
                "tests::startup_runs_application_through_telemetry_wrapper",
            ])
            .env(TELEMETRY_STARTUP_CHILD_ENV, "1")
            .env("AGENTTY_ROOT", root.path())
            .env("HOME", root.path())
            .env("PATH", &child_path);
        if let Some(status) = response_status {
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            child.env(
                "AGENTTY_TEST_OTLP_ENDPOINT",
                format!("{}/v1/traces", server.uri()),
            );
        }

        // Act
        let output = child.output().await.expect("startup process");

        // Assert
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert_eq!(
            stderr.contains("OTLP export reported"),
            response_status == Some(503)
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            response_status.is_none()
        );
    }
}
