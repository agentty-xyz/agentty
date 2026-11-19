//! Public sandbox choice contract: a run whose native sandbox cannot run
//! warns and runs `bash` unsandboxed by default, refuses under
//! `IfUnavailable::Refuse`, and runs unsandboxed silently when the host
//! chose `Sandbox::Unsandboxed`. Turns without an event stream carry the
//! warning on their report.

use std::error::Error;
use std::future::poll_fn;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::bash::{BashConfig, Enforcement};
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use ag_harness::tool::ToolDefinition;
use ag_harness::{
    ErrorKind, EventStream, Harness, OutputSchema, RunEvent, RunEventKind, RunOutcome, RunRequest,
    ToolSet, TurnOptions,
};
use futures_core::Stream;
use serde_json::json;

use crate::BashDrivingModel;
use crate::context_budget_fixture::unbounded_context_budget;
use crate::repository_fixture::repository_with_host_git;

#[tokio::test]
async fn unavailable_native_sandbox_runs_bash_unsandboxed_with_a_warning()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let advertised = Arc::new(Mutex::new(Vec::new()));

    // Act
    let (events, outcome) = run_native(directory.path(), Sandbox::default(), &advertised).await?;

    // Assert
    let warnings = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::SandboxUnavailable(warning) => Some(warning.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        warnings,
        [SandboxWarning::new(
            SandboxUnavailableReason::HelperUntrusted
        )]
    );
    assert!(matches!(
        events.first().map(|event| &event.kind),
        Some(RunEventKind::Started { .. })
    ));
    assert!(matches!(
        events.get(1).map(|event| &event.kind),
        Some(RunEventKind::SandboxUnavailable(_))
    ));
    let output = outcome.result?.parse::<serde_json::Value>()?;
    assert_eq!(output["stdout"], "unsandboxed");
    assert_eq!(outcome.report.sandbox_warning(), warnings.first());
    assert_eq!(outcome.commands[0].intent.policy["executor"], "unsandboxed");
    assert_eq!(
        advertised.lock().map_err(|_| "advertised tools")?[0],
        [ToolDefinition::bash(Enforcement::None)]
    );

    Ok(())
}

#[tokio::test]
async fn refusing_sandbox_fails_the_run_before_any_model_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let advertised = Arc::new(Mutex::new(Vec::new()));
    let sandbox = Sandbox::Native {
        if_unavailable: IfUnavailable::Refuse,
    };

    // Act
    let (events, outcome) = run_native(directory.path(), sandbox, &advertised).await?;

    // Assert
    let error = outcome.result.expect_err("the sandbox is unavailable");
    assert_eq!(error.kind(), ErrorKind::SandboxUnavailable);
    assert!(!error.is_retryable());
    assert!(
        error
            .to_string()
            .contains(SandboxUnavailableReason::HelperUntrusted.remedy())
    );
    assert!(
        advertised
            .lock()
            .map_err(|_| "advertised tools")?
            .is_empty()
    );
    assert!(matches!(
        events
            .iter()
            .map(|event| &event.kind)
            .collect::<Vec<_>>()
            .as_slice(),
        [RunEventKind::Completed(_)]
    ));

    Ok(())
}

#[tokio::test]
async fn chosen_unsandboxed_execution_runs_bash_without_a_warning() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let advertised = Arc::new(Mutex::new(Vec::new()));

    // Act
    let (events, outcome) = run_native(directory.path(), Sandbox::Unsandboxed, &advertised).await?;

    // Assert
    let output = outcome.result?.parse::<serde_json::Value>()?;
    assert_eq!(output["stdout"], "unsandboxed");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, RunEventKind::SandboxUnavailable(_)))
    );
    assert_eq!(
        advertised.lock().map_err(|_| "advertised tools")?[0],
        [ToolDefinition::bash(Enforcement::None)]
    );

    Ok(())
}

#[tokio::test]
async fn turns_without_an_event_stream_report_the_warning() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let advertised = Arc::new(Mutex::new(Vec::new()));
    let (harness, configuration) =
        native_harness(directory.path(), Sandbox::default(), &advertised)?;
    let options = TurnOptions::from_output(OutputSchema::new(json!({"type": "object"}))?)
        .with_tools(ToolSet::empty().with_bash())
        .with_bash(configuration);

    // Act
    let first = harness.turn("printf unsandboxed", options.clone()).await?;
    let second = harness.turn("printf unsandboxed", options).await?;

    // Assert
    let warning = SandboxWarning::new(SandboxUnavailableReason::HelperUntrusted);
    assert_eq!(first.output()["stdout"], "unsandboxed");
    assert_eq!(first.report().sandbox_warning(), Some(&warning));
    assert_eq!(second.report().sandbox_warning(), Some(&warning));

    Ok(())
}

/// Runs `printf unsandboxed` through [`native_harness`] under `sandbox`.
async fn run_native(
    root: &Path,
    sandbox: Sandbox,
    advertised: &Arc<Mutex<Vec<Vec<ToolDefinition>>>>,
) -> Result<(Vec<RunEvent>, RunOutcome), Box<dyn Error>> {
    let (harness, configuration) = native_harness(root, sandbox, advertised)?;
    let request = RunRequest::new("printf unsandboxed")
        .output(OutputSchema::new(json!({"type": "object"}))?)
        .tools(ToolSet::empty().with_bash())
        .bash(configuration);
    let (stream, handle) = harness.run(request).into_parts();

    Ok(tokio::join!(drain(stream), handle.outcome()))
}

/// Returns a harness under `sandbox` and a native `bash` configuration
/// whose helper is an executable inside the workspace at `root`, so the
/// sandbox is unavailable on every platform.
fn native_harness(
    root: &Path,
    sandbox: Sandbox,
    advertised: &Arc<Mutex<Vec<Vec<ToolDefinition>>>>,
) -> Result<(Harness, BashConfig), Box<dyn Error>> {
    let root = root.canonicalize()?;
    let helper = root.join("ag-harness-sandbox");
    std::fs::write(&helper, "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))?;
    let configuration = BashConfig::new(
        helper,
        "/bin/bash".into(),
        "native-unavailable-1".into(),
        Duration::from_secs(5),
        BashConfig::DEFAULT_CAPTURE_BYTES,
    )?
    .with_host_information();
    let harness = Harness::builder()
        .model(Arc::new(BashDrivingModel {
            advertised: Arc::clone(advertised),
        }))
        .context_budget(unbounded_context_budget())
        .repository(repository_with_host_git(&root))
        .sandbox(sandbox)
        .build()?;

    Ok((harness, configuration))
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        events.push(event);
    }

    events
}
