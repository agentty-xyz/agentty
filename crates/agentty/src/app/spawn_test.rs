use ag_telemetry::Context;

use super::{spawn_blocking_detached, spawn_detached, spawn_traced};

/// Context value that marks the caller's trace context.
#[derive(Debug, PartialEq)]
struct CallerMarker;

/// Calls `spawn` while the caller's marked context is attached, then detaches
/// it so the returned task is awaited from an unmarked context.
fn spawn_in_caller_context<Task>(spawn: impl FnOnce() -> Task) -> Task {
    let _caller = Context::current_with_value(CallerMarker).attach();

    spawn()
}

/// Returns whether the current context carries the caller's marker.
fn sees_caller_marker() -> bool {
    Context::current().get::<CallerMarker>().is_some()
}

#[tokio::test]
async fn spawn_traced_runs_the_task_in_the_caller_context() {
    // Arrange
    let task = spawn_in_caller_context(|| spawn_traced(async { sees_caller_marker() }));

    // Act
    let saw_marker = task.await.expect("traced task should finish");

    // Assert
    assert!(saw_marker);
}

#[tokio::test]
async fn spawn_detached_runs_the_task_without_the_caller_context() {
    // Arrange
    let task = spawn_in_caller_context(|| spawn_detached(async { sees_caller_marker() }));

    // Act
    let saw_marker = task.await.expect("detached task should finish");

    // Assert
    assert!(!saw_marker);
}

#[tokio::test]
async fn spawn_blocking_detached_runs_the_work_without_the_caller_context() {
    // Arrange
    let task = spawn_in_caller_context(|| spawn_blocking_detached(sees_caller_marker));

    // Act
    let saw_marker = task.await.expect("detached blocking work should finish");

    // Assert
    assert!(!saw_marker);
}
