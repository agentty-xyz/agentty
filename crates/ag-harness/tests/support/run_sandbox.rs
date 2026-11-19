//! Public sandbox choice contract: a run whose native sandbox cannot run
//! warns and runs `bash` unsandboxed by default, refuses under
//! `IfUnavailable::Refuse`, and runs unsandboxed silently when the host
//! chose `Sandbox::Unsandboxed`.

use std::error::Error;
use std::future::poll_fn;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::bash::{BashConfig, Enforcement};
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use ag_harness::tool::ToolDefinition;
use ag_harness::{
    ErrorKind, EventStream, Harness, OutputSchema, RunEvent, RunEventKind, RunOutcome, RunRequest,
    ToolSet,
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
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, RunEventKind::SandboxUnavailable(_)))
    );

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

/// Runs `printf unsandboxed` through native `bash` whose helper lies inside
/// the workspace at `root`, so the sandbox is unavailable on every
/// platform, under `sandbox`.
async fn run_native(
    root: &Path,
    sandbox: Sandbox,
    advertised: &Arc<Mutex<Vec<Vec<ToolDefinition>>>>,
) -> Result<(Vec<RunEvent>, RunOutcome), Box<dyn Error>> {
    let root = root.canonicalize()?;
    let configuration = BashConfig::new(
        root.join("ag-harness-sandbox"),
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
    let request = RunRequest::new("printf unsandboxed")
        .output(OutputSchema::new(json!({"type": "object"}))?)
        .tools(ToolSet::empty().with_bash())
        .bash(configuration);
    let (stream, handle) = harness.run(request).into_parts();

    Ok(tokio::join!(drain(stream), handle.outcome()))
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        events.push(event);
    }

    events
}
