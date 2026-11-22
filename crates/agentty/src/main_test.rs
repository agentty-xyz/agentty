//! CLI argument parsing, telemetry setup, and startup failure tests for the
//! entry point.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use std::{env, fs};

use agentty::analytics::Analytics;
use agentty::app::{App, AppError, DEFAULT_REMOTE_CONTROL_PORT, agentty_home};
use agentty::infra::db::{DB_DIR, DB_FILE, acquire_instance_lock};
use clap::Parser;
use clap::error::ErrorKind;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value;
use prost::Message as _;
use uuid::Uuid;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::{Cli, ExperimentalFeatures, parse_relay_url, run, run_with_analytics};

const LOCK_FAILURE_CHILD_ENV: &str = "AGENTTY_LOCK_FAILURE_CHILD";
const TRACED_STARTUP_CHILD_ENV: &str = "AGENTTY_TRACED_STARTUP_CHILD";
const OWNED_ROOT_CHILD_ENV: &str = "AGENTTY_OWNED_ROOT_CHILD";
const DATABASE_FAILURE_CHILD_ENV: &str = "AGENTTY_DATABASE_FAILURE_CHILD";
const TELEMETRY_STARTUP_CHILD_ENV: &str = "AGENTTY_TELEMETRY_STARTUP_CHILD";
const DELETED_CWD_CHILD_ENV: &str = "AGENTTY_DELETED_CWD_CHILD";

async fn closed_runtime(_app: &mut App) -> Result<(), AppError> {
    Err(AppError::Terminal(std::io::Error::other("terminal closed")))
}

async fn root_working_dir_runtime(app: &mut App) -> Result<(), AppError> {
    if app.working_dir() == std::path::Path::new("/") {
        return Ok(());
    }

    Err(AppError::Workflow(format!(
        "unexpected working directory: {}",
        app.working_dir().display()
    )))
}

async fn traced_closed_runtime(app: &mut App) -> Result<(), AppError> {
    assert_eq!(
        ag_telemetry::Context::current()
            .get::<ag_telemetry::CaptureToolContent>()
            .expect("startup capture policy")
            .0,
        env::var_os("AGENTTY_TEST_CAPTURE_CONTENT").is_some()
    );
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
    assert!(!cli.otlp_capture_content);
    assert!(!cli.experimental.harness);
}

#[test]
fn cli_parses_experimental_harness_flag() {
    // Arrange / Act
    let cli = Cli::try_parse_from(["agentty", "--experimental-harness"])
        .expect("--experimental-harness should parse");

    // Assert
    assert!(cli.experimental.harness);
}

#[test]
fn cli_gates_remote_control_behind_experimental_remote() {
    // Arrange / Act
    let disabled = Cli::try_parse_from(["agentty"]).expect("default arguments");
    let enabled = Cli::try_parse_from([
        "agentty",
        "--experimental-remote",
        "--remote-control-port",
        "0",
    ])
    .expect("--experimental-remote with a port should parse");
    let port_without_flag = Cli::try_parse_from(["agentty", "--remote-control-port", "0"])
        .expect_err("a port needs --experimental-remote");
    let host_without_flag =
        Cli::try_parse_from(["agentty", "--remote-control-host", "box.tailnet.ts.net"])
            .expect_err("a host needs --experimental-remote");
    let with_hosts = Cli::try_parse_from([
        "agentty",
        "--experimental-remote",
        "--remote-control-host",
        "box.tailnet.ts.net",
        "--remote-control-host",
        "localhost.example",
    ])
    .expect("repeated hosts should parse");
    let relay_without_flag =
        Cli::try_parse_from(["agentty", "--remote-relay", "wss://relay.example"])
            .expect_err("a relay needs --experimental-remote");
    let with_relay = Cli::try_parse_from([
        "agentty",
        "--experimental-remote",
        "--remote-relay",
        "wss://relay.example",
    ])
    .expect("a relay should parse");

    // Assert
    assert_eq!(
        relay_without_flag.kind(),
        ErrorKind::MissingRequiredArgument
    );
    assert_eq!(
        with_relay.remote_relay.as_deref(),
        Some("wss://relay.example")
    );
    assert_eq!(enabled.remote_relay, None);
    assert!(!disabled.experimental.remote);
    assert_eq!(disabled.remote_control_port, DEFAULT_REMOTE_CONTROL_PORT);
    assert!(enabled.experimental.remote);
    assert_eq!(enabled.remote_control_port, 0);
    assert_eq!(port_without_flag.kind(), ErrorKind::MissingRequiredArgument);
    assert_eq!(host_without_flag.kind(), ErrorKind::MissingRequiredArgument);
    assert_eq!(
        with_hosts.remote_control_host,
        ["box.tailnet.ts.net", "localhost.example"]
    );
}

#[test]
fn relay_url_needs_a_websocket_scheme_and_a_host() {
    // Arrange
    let accepted = [
        ("wss://relay.example/", "wss://relay.example"),
        ("ws://127.0.0.1:8787", "ws://127.0.0.1:8787"),
        (" wss://relay.example/base ", "wss://relay.example/base"),
    ];
    let rejected = [
        "https://relay.example",
        "relay.example",
        "wss://",
        "wss:///path",
        "wss://relay.example/?token=1",
        "wss://relay.example/#x",
        "wss://relay example",
        "wss://user:token@relay.example",
    ];

    // Act
    let parsed: Vec<_> = accepted
        .iter()
        .map(|(value, _)| parse_relay_url(value))
        .collect();
    let refused: Vec<_> = rejected
        .iter()
        .map(|value| parse_relay_url(value))
        .collect();

    // Assert
    for ((_, expected), parsed) in accepted.iter().zip(parsed) {
        assert_eq!(parsed.as_deref(), Ok(*expected));
    }
    for refused in refused {
        assert_eq!(
            refused,
            Err("expected a relay URL such as wss://relay.example".to_string())
        );
    }
}

#[test]
fn cli_parses_no_update_flag() {
    // Arrange / Act
    let cli = Cli::try_parse_from(["agentty", "--no-update"]).expect("--no-update should parse");

    // Assert
    assert!(cli.no_update);
}

#[test]
fn cli_rejects_retired_experimental_flags() {
    // Arrange
    let retired_flags = ["--experimental-graph", "--experimental-multi-agent"];

    for flag in retired_flags {
        // Act
        let result = Cli::try_parse_from(["agentty", flag]);

        // Assert
        assert!(result.is_err(), "{flag} should be rejected");
    }
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
fn cli_enables_posthog_telemetry_unless_disabled() {
    // Arrange / Act
    let default = Cli::try_parse_from(["agentty"]).expect("default arguments");
    let disabled =
        Cli::try_parse_from(["agentty", "--disable-telemetry"]).expect("telemetry opt-out");

    // Assert
    assert!(!default.disable_telemetry);
    assert!(disabled.disable_telemetry);
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
fn content_capture_requires_an_explicit_export_destination() {
    // Arrange / Act
    let missing = Cli::try_parse_from(["agentty", "--otlp-capture-content"])
        .expect_err("capture requires an endpoint");
    let enabled = Cli::try_parse_from([
        "agentty",
        "--otlp-endpoint",
        "http://localhost:4318/v1/traces",
        "--otlp-capture-content",
    ])
    .expect("explicit content capture");

    // Assert
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
    assert!(enabled.otlp_capture_content);
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
            disable_telemetry: false,
            experimental: ExperimentalFeatures::default(),
            no_update: false,
            otlp_capture_content: false,
            otlp_endpoint: None,
            remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
            remote_control_host: Vec::new(),
            remote_relay: None,
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
async fn run_rejects_an_incomplete_otlp_endpoint_before_startup() {
    // Arrange
    let cli = Cli {
        disable_telemetry: false,
        experimental: ExperimentalFeatures::default(),
        no_update: false,
        otlp_capture_content: false,
        otlp_endpoint: Some("http://localhost:4318".to_string()),
        remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
        remote_control_host: Vec::new(),
        remote_relay: None,
    };

    // Act
    let error = run(cli, closed_runtime)
        .await
        .expect_err("an endpoint without a traces path should be rejected");

    // Assert
    assert!(matches!(error, AppError::Workflow(_)));
    assert!(error.to_string().contains("--otlp-endpoint"));
}

#[tokio::test]
async fn run_exports_nothing_when_startup_fails_before_the_database_opens() {
    if env::var_os(TRACED_STARTUP_CHILD_ENV).is_some() {
        // Arrange
        let cli = Cli {
            disable_telemetry: false,
            experimental: ExperimentalFeatures::default(),
            no_update: false,
            otlp_capture_content: false,
            otlp_endpoint: env::var("AGENTTY_TEST_OTLP_ENDPOINT").ok(),
            remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
            remote_control_host: Vec::new(),
            remote_relay: None,
        };

        // Act
        let error = run(cli, closed_runtime)
            .await
            .expect_err("startup should reject a file-backed root");

        // Assert
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
    let server = MockServer::start().await;

    // Act
    let status = tokio::process::Command::new(test_binary)
        .arg("--exact")
        .arg("tests::run_exports_nothing_when_startup_fails_before_the_database_opens")
        .env(TRACED_STARTUP_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", blocking_root)
        .env(
            "AGENTTY_TEST_OTLP_ENDPOINT",
            format!("{}/v1/traces", server.uri()),
        )
        .status()
        .await
        .expect("isolated traced startup test should run");

    // Assert
    assert!(status.success());
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn run_rejects_an_owned_root_before_opening_the_database() {
    if env::var_os(OWNED_ROOT_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(
            Cli {
                disable_telemetry: false,
                experimental: ExperimentalFeatures::default(),
                no_update: true,
                otlp_capture_content: false,
                otlp_endpoint: None,
                remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
                remote_control_host: Vec::new(),
                remote_relay: None,
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
                disable_telemetry: false,
                experimental: ExperimentalFeatures::default(),
                no_update: true,
                otlp_capture_content: false,
                otlp_endpoint: None,
                remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
                remote_control_host: Vec::new(),
                remote_relay: None,
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
async fn startup_from_a_deleted_directory_falls_back_to_the_filesystem_root() {
    if env::var_os(DELETED_CWD_CHILD_ENV).is_some() {
        // Arrange
        let deleted_directory = tempfile::tempdir().expect("doomed working directory");
        env::set_current_dir(deleted_directory.path()).expect("enter doomed directory");
        deleted_directory.close().expect("remove working directory");

        // Act
        let result = run(
            Cli {
                disable_telemetry: false,
                experimental: ExperimentalFeatures::default(),
                no_update: true,
                otlp_capture_content: false,
                otlp_endpoint: None,
                remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
                remote_control_host: Vec::new(),
                remote_relay: None,
            },
            root_working_dir_runtime,
        )
        .await;

        // Assert
        result.expect("startup should use the filesystem root as its working directory");

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

    // Act
    let output = tokio::process::Command::new(env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "tests::startup_from_a_deleted_directory_falls_back_to_the_filesystem_root",
        ])
        .env(DELETED_CWD_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", root.path())
        .env("HOME", root.path())
        .env("PATH", format!("{}:/usr/bin:/bin", stub_bin.display()))
        .output()
        .await
        .expect("startup process");

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Builds the launch options the telemetry startup child reads from its
/// environment, with remote control enabled so the full launch wires it.
fn telemetry_child_cli() -> Cli {
    Cli {
        disable_telemetry: true,
        experimental: ExperimentalFeatures {
            harness: false,
            remote: true,
        },
        no_update: true,
        otlp_capture_content: env::var_os("AGENTTY_TEST_CAPTURE_CONTENT").is_some(),
        otlp_endpoint: env::var("AGENTTY_TEST_OTLP_ENDPOINT").ok(),
        remote_control_port: DEFAULT_REMOTE_CONTROL_PORT,
        remote_control_host: Vec::new(),
        remote_relay: None,
    }
}

#[tokio::test]
async fn startup_runs_application_through_telemetry_wrapper() {
    if env::var_os(TELEMETRY_STARTUP_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(telemetry_child_cli(), traced_closed_runtime)
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
    let mut installation_ids = BTreeSet::new();

    for (response_status, capture) in [
        (None, false),
        (Some(200), false),
        (Some(200), true),
        (Some(503), false),
    ] {
        let server = MockServer::start().await;
        let mut child = tokio::process::Command::new(env::current_exe().expect("test binary"));
        child
            .args([
                "--exact",
                "tests::startup_runs_application_through_telemetry_wrapper",
            ])
            .env(TELEMETRY_STARTUP_CHILD_ENV, "1")
            .env("AGENTTY_ROOT", root.path())
            .env("CI", "true")
            .env("HOME", root.path())
            .env("PATH", &child_path);
        if capture {
            child.env("AGENTTY_TEST_CAPTURE_CONTENT", "1");
        }
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
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.is_empty(), response_status.is_none());
        let expected_events = if response_status.is_some() {
            vec![
                exported_event(
                    "agentty_failure",
                    &[
                        ("ci", "true"),
                        ("failure_category", "terminal"),
                        ("interactive", "false"),
                        ("stage", "runtime"),
                    ],
                ),
                exported_event(
                    "agentty_launch",
                    &[("ci", "true"), ("interactive", "false")],
                ),
            ]
        } else {
            Vec::new()
        };
        assert_eq!(exported_events(&requests), expected_events);
        installation_ids.extend(exported_user_ids(&requests));
        if response_status == Some(200) {
            assert_startup_trace(&exported_spans(&requests));
        }
    }
    let installation_id = installation_ids
        .pop_first()
        .flatten()
        .expect("exported installation ID");
    assert!(installation_ids.is_empty(), "every batch shares one ID");
    assert!(Uuid::parse_str(&installation_id).is_ok());
}

/// Exported span names with their span and parent span identifiers and
/// attribute keys.
type ExportedSpan = (String, Vec<u8>, Vec<u8>, Vec<String>);

/// Returns every span exported to the traces path.
fn exported_spans(requests: &[Request]) -> Vec<ExportedSpan> {
    requests
        .iter()
        .filter(|request| request.url.path() == "/v1/traces")
        .flat_map(|request| {
            ExportTraceServiceRequest::decode(request.body.as_slice())
                .expect("OTLP traces")
                .resource_spans
        })
        .flat_map(|resource| resource.scope_spans)
        .flat_map(|scope| scope.spans)
        .map(|span| {
            let keys = span
                .attributes
                .into_iter()
                .map(|attribute| attribute.key)
                .collect();

            (span.name, span.span_id, span.parent_span_id, keys)
        })
        .collect()
}

/// Asserts startup exported one root `app.startup` trace that records the
/// database open time and whose phases nest beneath it.
fn assert_startup_trace(spans: &[ExportedSpan]) {
    let find = |name: &str| {
        spans
            .iter()
            .find(|(span_name, _, _, _)| span_name == name)
            .expect("exported startup span")
    };
    let (_, startup_id, startup_parent, startup_keys) = find("app.startup");
    assert!(startup_parent.is_empty(), "startup is a root");
    assert!(startup_keys.iter().any(|key| key == "agentty.db.open_ms"));
    for phase in ["project.load", "sessions.load"] {
        assert_eq!(&find(phase).2, startup_id, "`{phase}` parent");
    }
}

/// Returns the string `user.id` resource attribute of every exported trace
/// and log batch, or `None` for a batch without one.
fn exported_user_ids(requests: &[Request]) -> Vec<Option<String>> {
    requests
        .iter()
        .flat_map(|request| match request.url.path() {
            "/v1/logs" => ExportLogsServiceRequest::decode(request.body.as_slice())
                .expect("OTLP logs")
                .resource_logs
                .into_iter()
                .filter_map(|resource| resource.resource)
                .collect::<Vec<_>>(),
            _ => ExportTraceServiceRequest::decode(request.body.as_slice())
                .expect("OTLP traces")
                .resource_spans
                .into_iter()
                .filter_map(|resource| resource.resource)
                .collect(),
        })
        .map(|resource| {
            resource
                .attributes
                .into_iter()
                .find(|attribute| attribute.key == "user.id")
                .and_then(|attribute| attribute.value)
                .and_then(|value| match value.value {
                    Some(any_value::Value::StringValue(user_id)) => Some(user_id),
                    _ => None,
                })
        })
        .collect()
}

/// An exported event name with its sorted event-specific attributes.
type ExportedEvent = (String, Vec<(String, String)>);

/// Returns the distinct exported usage events, without the properties shared
/// by every event. Rejected exports are retried, so duplicates are dropped.
fn exported_events(requests: &[Request]) -> Vec<ExportedEvent> {
    let mut events: Vec<ExportedEvent> = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/logs")
        .flat_map(|request| {
            ExportLogsServiceRequest::decode(request.body.as_slice())
                .expect("OTLP logs")
                .resource_logs
        })
        .flat_map(|resource| resource.scope_logs)
        .flat_map(|scope| scope.log_records)
        .map(|record| {
            let mut attributes: Vec<(String, String)> = record
                .attributes
                .into_iter()
                .filter(|attribute| {
                    !["app_source", "app_version", "install_method"]
                        .contains(&attribute.key.as_str())
                })
                .map(|attribute| {
                    let value = match attribute.value.and_then(|value| value.value) {
                        Some(any_value::Value::StringValue(value)) => value,
                        value => format!("{value:?}"),
                    };

                    (attribute.key, value)
                })
                .collect();
            attributes.sort();

            (record.event_name, attributes)
        })
        .collect();
    events.sort();
    events.dedup();

    events
}

fn exported_event(name: &str, attributes: &[(&str, &str)]) -> ExportedEvent {
    (
        name.to_string(),
        attributes
            .iter()
            .map(|&(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    )
}
