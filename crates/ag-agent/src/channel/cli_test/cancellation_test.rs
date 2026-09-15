use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustix::process::{self, Pid, Signal};
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::support::make_turn_request;
use crate::agent::MockAgentBackend;
use crate::channel::cli::{CliAgentChannel, CliTurnLease};
use crate::channel::{AgentChannel, AgentError, TurnEvent};
use crate::model::agent::AgentKind;

#[tokio::test]
async fn shutdown_cancels_only_the_owned_turn_and_releases_session() {
    // Arrange
    let directory = tempdir().expect("temporary workspace");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exec sleep 60"]);
        Ok(command)
    });
    let channel = Arc::new(CliAgentChannel::with_backend(
        Arc::new(backend),
        AgentKind::Claude,
    ));
    let (sender, mut events) = mpsc::unbounded_channel();
    let task = tokio::spawn(channel.run_turn(
        "session".into(),
        make_turn_request(directory.path().into()),
        sender,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                events.recv().await.expect("running turn event"),
                TurnEvent::PidUpdate(Some(_))
            ) {
                break;
            }
        }
    })
    .await
    .expect("provider starts");
    // Act
    let duplicate = channel
        .run_turn(
            "session".into(),
            make_turn_request(directory.path().into()),
            mpsc::unbounded_channel().0,
        )
        .await;
    channel
        .shutdown_session("other".into())
        .await
        .expect("unrelated shutdown");
    assert!(!task.is_finished());
    channel
        .shutdown_session("session".into())
        .await
        .expect("owned shutdown");
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("bounded cancellation")
        .expect("task joins");
    // Assert
    assert!(matches!(duplicate, Err(AgentError::Runtime(_))));
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
    assert!(channel.active.lock().expect("registry").is_empty());
    assert!(matches!(
        events.recv().await,
        Some(TurnEvent::PidUpdate(None))
    ));
    channel
        .shutdown_session("session".into())
        .await
        .expect("idempotent shutdown");
}

#[tokio::test]
async fn poisoned_ownership_fails_closed_and_drop_remains_safe() {
    // Arrange
    let active = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let lease = CliTurnLease::acquire(
        Arc::clone(&active),
        "session".into(),
        CancellationToken::new(),
    )
    .expect("lease");
    let _ = std::panic::catch_unwind(|| {
        let _guard = active.lock().expect("registry");
        std::panic::resume_unwind(Box::new("poisoned ownership"));
    });
    let channel = CliAgentChannel {
        backend: Arc::new(MockAgentBackend::new()),
        kind: AgentKind::Claude,
        active: Arc::clone(&active),
    };
    // Act
    let acquisition = CliTurnLease::acquire(active, "other".into(), CancellationToken::new());
    let shutdown = channel.shutdown_session("session".into()).await;
    drop(lease);
    // Assert
    assert!(acquisition.is_err());
    assert!(shutdown.is_err());
}

#[tokio::test]
async fn cancellation_and_future_drop_terminate_tool_descendants() {
    for abort_future in [false, true] {
        // Arrange
        let directory = tempdir().expect("temporary workspace");
        let pid_file = directory.path().join("descendant.pid");
        let fixture_pid_file = pid_file.clone();
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().once().return_once(move |_| {
            let mut command = std::process::Command::new("sh");
            command.args([
                "-c",
                "trap '' TERM; sleep 60 & descendant=$!; printf '%s' \"$descendant\" > \
                 \"$DESCENDANT_PID\"; wait",
            ]);
            command.env("DESCENDANT_PID", fixture_pid_file);
            Ok(command)
        });
        let channel = CliAgentChannel::with_backend(Arc::new(backend), AgentKind::Claude);
        let task = tokio::spawn(channel.run_turn(
            "session".into(),
            make_turn_request(directory.path().into()),
            mpsc::unbounded_channel().0,
        ));
        let descendant: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = tokio::fs::read_to_string(&pid_file).await
                    && let Ok(pid) = pid.parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("tool descendant starts");

        // Act
        if abort_future {
            task.abort();
        } else {
            channel
                .shutdown_session("session".into())
                .await
                .expect("shutdown");
        }
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("turn stops");
        let stopped = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = tokio::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &descendant.to_string()])
                    .output()
                    .await
                    .expect("process status");
                let state = String::from_utf8_lossy(&state.stdout);
                if state.trim().is_empty() || state.trim_start().starts_with('Z') {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        // Clean up the owned fixture even if a regression left it running.
        if stopped.is_err() {
            let _ = process::kill_process(
                Pid::from_raw(descendant).expect("owned descendant"),
                Signal::KILL,
            );
        }

        // Assert
        assert!(stopped.is_ok(), "tool descendant survived its owning turn");
        if abort_future {
            assert!(result.expect_err("aborted task").is_cancelled());
        } else {
            assert!(matches!(
                result.expect("joined turn"),
                Err(AgentError::InterruptedByUser(_))
            ));
        }
        assert!(channel.active.lock().expect("registry").is_empty());
    }
}
