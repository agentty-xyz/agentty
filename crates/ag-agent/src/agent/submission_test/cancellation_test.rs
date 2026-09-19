use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, OneShotClient, OneShotRequest, PermissionMode, ReasoningLevel, SpeedMode,
};
use ag_session::AgentKind;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::submission::RealOneShotClient;
use crate::app_server::{AppServerTurnResponse, MockAppServerClient};

fn request() -> OneShotRequest {
    OneShotRequest {
        child_pid: Some(Arc::new(Mutex::new(Some(42)))),
        folder: "repository".into(),
        harness: AgentKind::Codex.to_string(),
        model: "model".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "title".into(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::UtilityPrompt,
        speed_mode: SpeedMode::Normal,
    }
}

fn response(text: &str) -> AppServerTurnResponse {
    AppServerTurnResponse {
        assistant_message: text.into(),
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        pid: None,
        provider_conversation_id: None,
    }
}

#[tokio::test]
async fn canceled_one_shots_shutdown_active_and_repair_turns_before_returning() {
    for repair in [false, true] {
        for drop_caller in [false, true] {
            // Arrange
            let (started_tx, mut started_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
            let stopped = CancellationToken::new();
            let finish = CancellationToken::new();
            let completed = CancellationToken::new();
            let mut mock = MockAppServerClient::new();
            let mut count = 0;
            mock.expect_run_turn()
                .times(if repair { 2 } else { 1 })
                .returning({
                    let stopped = stopped.clone();
                    let finish = finish.clone();
                    let completed = completed.clone();
                    move |turn, _| {
                        count += 1;
                        let initial = repair && count == 1;
                        let started_tx = started_tx.clone();
                        let stopped = stopped.clone();
                        let finish = finish.clone();
                        let completed = completed.clone();
                        Box::pin(async move {
                            if initial {
                                return Ok(response("invalid JSON"));
                            }
                            started_tx.send(turn.session_id).expect("turn started");
                            stopped.cancelled().await;
                            finish.cancelled().await;
                            completed.cancel();
                            Ok(response(r#"{"answer":"done"}"#))
                        })
                    }
                });
            mock.expect_shutdown_session().once().returning(move |id| {
                let stopped = stopped.clone();
                let shutdown_tx = shutdown_tx.clone();
                Box::pin(async move {
                    shutdown_tx.send(id).expect("shutdown observed");
                    stopped.cancel();
                })
            });
            let client = RealOneShotClient::new(Some(Arc::new(mock)));
            let request = request();
            let pid = request.child_pid.clone().expect("pid slot");
            let cancellation = CancellationToken::new();
            let task = {
                let cancellation = cancellation.clone();
                tokio::spawn(async move { client.submit_cancellable(request, cancellation).await })
            };
            let id = started_rx.recv().await.expect("active turn");
            // Act
            if drop_caller {
                task.abort();
            } else {
                cancellation.cancel();
            }
            let shutdown_id = shutdown_rx.recv().await.expect("provider shutdown");
            // Assert
            assert_eq!(shutdown_id, id);
            assert!(!completed.is_cancelled());
            if !drop_caller {
                assert!(!task.is_finished());
            }
            finish.cancel();
            completed.cancelled().await;
            if drop_caller {
                assert!(task.await.expect_err("aborted caller").is_cancelled());
            } else {
                assert!(
                    task.await
                        .expect("caller")
                        .expect_err("canceled")
                        .to_string()
                        .contains("[Stopped]")
                );
            }
            assert_eq!(*pid.lock().expect("pid slot"), None);
        }
    }
}

#[tokio::test]
async fn precanceled_submissions_do_not_start_cli_or_app_server_turns() {
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        // Arrange
        let mut mock = MockAppServerClient::new();
        mock.expect_run_turn().never();
        mock.expect_shutdown_session()
            .times(usize::from(kind == AgentKind::Codex))
            .returning(|_| Box::pin(async {}));
        let client = RealOneShotClient::new(Some(Arc::new(mock)));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let request = OneShotRequest {
            harness: kind.to_string(),
            ..request()
        };
        // Act
        let error = client
            .submit_cancellable(request, cancellation)
            .await
            .expect_err("canceled");
        // Assert
        assert!(error.to_string().contains("[Stopped]"));
    }
}

#[tokio::test]
async fn provider_panics_shutdown_initial_and_repair_sessions_before_returning() {
    for repair in [false, true] {
        for synchronous in [false, true] {
            // Arrange
            let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
            let finish = CancellationToken::new();
            let mut mock = MockAppServerClient::new();
            let mut count = 0;
            mock.expect_run_turn()
                .times(if repair { 2 } else { 1 })
                .returning(move |turn, _| {
                    count += 1;
                    if repair && count == 1 {
                        return Box::pin(async { Ok(response("invalid JSON")) });
                    }
                    turn_tx.send(turn.session_id).expect("panicking turn");
                    if synchronous {
                        std::panic::resume_unwind(Box::new("provider panicked during call"));
                    }
                    Box::pin(async {
                        std::panic::resume_unwind(Box::new("provider panicked during poll"))
                    })
                });
            mock.expect_shutdown_session().once().returning({
                let finish = finish.clone();
                move |id| {
                    shutdown_tx.send(id).expect("shutdown observed");
                    let finish = finish.clone();
                    Box::pin(async move { finish.cancelled().await })
                }
            });
            let client = RealOneShotClient::new(Some(Arc::new(mock)));
            let request = request();
            let pid = request.child_pid.clone().expect("pid");
            // Act
            let task = tokio::spawn(async move { client.submit(request).await });
            let turn = turn_rx.recv().await.expect("turn");
            let shutdown = shutdown_rx.recv().await.expect("shutdown");
            // Assert
            assert_eq!(turn, shutdown);
            assert!(!task.is_finished(), "failure must wait for shutdown");
            finish.cancel();
            let error = task.await.expect("task").expect_err("provider panic");
            assert!(error.to_string().contains("provider turn panicked"));
            assert_eq!(*pid.lock().expect("pid"), None);
        }
    }
}

#[tokio::test]
async fn cleanup_task_panic_is_returned_to_the_caller() {
    // Arrange
    let mut mock = MockAppServerClient::new();
    mock.expect_run_turn()
        .once()
        .returning(|_, _| Box::pin(async { Ok(response(r#"{"answer":"done"}"#)) }));
    mock.expect_shutdown_session().once().returning(|_| {
        Box::pin(async { std::panic::resume_unwind(Box::new("shutdown task failed")) })
    });
    let client = RealOneShotClient::new(Some(Arc::new(mock)));
    // Act
    let error = client.submit(request()).await.expect_err("failed task");
    // Assert
    assert!(error.to_string().contains("One-shot cleanup task failed"));
}

#[tokio::test]
async fn forced_shutdown_rejects_new_cli_and_app_server_submissions() {
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        // Arrange
        let mut mock = MockAppServerClient::new();
        mock.expect_run_turn().never();
        let client = RealOneShotClient::new(Some(Arc::new(mock)));
        let request = OneShotRequest {
            harness: kind.to_string(),
            ..request()
        };
        // Act
        client.force_shutdown();
        let error = client.submit(request).await.expect_err("closed runtime");
        // Assert
        assert!(error.to_string().contains("forced to shut down"));
    }
}

#[tokio::test]
async fn cli_cancellable_submissions_preserve_provider_budgets() {
    // Arrange
    let client = RealOneShotClient::new(None);
    let request = OneShotRequest {
        harness: AgentKind::Claude.to_string(),
        provider_call_budget: Some(ag_contracts::ProviderCallBudget::new(0)),
        ..request()
    };
    // Act
    let error = client
        .submit_cancellable(request, CancellationToken::new())
        .await
        .expect_err("budget exhausted");
    // Assert
    assert!(error.to_string().contains("budget"));
}
