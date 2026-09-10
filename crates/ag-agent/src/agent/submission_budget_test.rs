use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_protocol::ProtocolSchemaInstructionMode;

use super::submit_one_shot_with_backend;
use crate::app_server::{AppServerSessionRegistry, RuntimeInspector, run_turn_with_restart_retry};
use crate::{
    AgentKind, AgentModel, AgentRequestKind, AppServerError, MockAgentBackend, MockAppServerClient,
    OneShotClient, OneShotRequest, PermissionMode, ProviderCallBudget, RealOneShotClient,
    ReasoningLevel, SpeedMode, is_input_size_error,
};

fn request(limit: usize) -> OneShotRequest {
    OneShotRequest {
        agent_kind: AgentKind::Codex,
        child_pid: None,
        folder: PathBuf::from("."),
        model: AgentModel::Gpt56Sol,
        permission_mode: PermissionMode::ReadOnly,
        prompt: "Summarize changes".into(),
        provider_call_budget: Some(ProviderCallBudget::new(limit)),
        request_kind: AgentRequestKind::UtilityPrompt,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::Normal,
    }
}

#[tokio::test]
async fn cli_initial_turns_and_protocol_repairs_share_the_budget() {
    // Arrange
    for limit in 0..=2 {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&count);
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(limit)
            .returning(move |_| {
                let response = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    "malformed response"
                } else {
                    r#"{"answer":"summary","questions":[]}"#
                };
                let mut command = Command::new("printf");
                command.arg("%s").arg(response);
                Ok(command)
            });
        let request = request(limit);

        // Act
        let result = submit_one_shot_with_backend(&backend, request.clone()).await;
        let next = submit_one_shot_with_backend(&backend, request).await;

        // Assert
        assert_eq!(count.load(Ordering::SeqCst), limit);
        if limit == 2 {
            assert_eq!(result.expect("repair fits").response.answer, "summary");
        } else {
            assert!(is_input_size_error(&result.expect_err("budget exhausted")));
        }
        assert!(is_input_size_error(
            &next.expect_err("shared budget exhausted")
        ));
    }
}

#[tokio::test]
async fn app_server_initial_turns_restart_retries_and_protocol_repairs_share_the_budget() {
    // Arrange
    for (limit, restart) in [(0, false), (1, false), (2, false), (2, true), (3, true)] {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&count);
        let sessions = Arc::new(AppServerSessionRegistry::new("budget test"));
        let mut transport = MockAppServerClient::new();
        transport.expect_run_turn().returning(move |request, _| {
            let count = Arc::clone(&calls);
            let sessions = Arc::clone(&sessions);
            Box::pin(async move {
                run_turn_with_restart_retry(
                    &sessions,
                    request,
                    RuntimeInspector {
                        matches_request: |(): &(), _| true,
                        pid: |()| None,
                        provider_conversation_id: |()| None,
                        retain_runtime_after_turn: false,
                        restored_context: |()| false,
                    },
                    ProtocolSchemaInstructionMode::TransportSchema,
                    |_| Box::pin(async { Ok(()) }),
                    move |(), _| {
                        let attempt = count.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async move {
                            if restart && attempt == 0 {
                                return Err(AppServerError::Provider("transport failure".into()));
                            }
                            let response = if attempt == usize::from(restart) {
                                "malformed response"
                            } else {
                                r#"{"answer":"summary","questions":[]}"#
                            };
                            Ok((response.to_string(), 0, 0))
                        })
                    },
                    |()| Box::pin(async {}),
                )
                .await
            })
        });
        transport
            .expect_shutdown_session()
            .times(2)
            .returning(|_| Box::pin(async {}));
        let client = RealOneShotClient::new(Some(Arc::new(transport)));
        let request = request(limit);

        // Act
        let result = client.submit(request.clone()).await;
        let next = client.submit(request).await;

        // Assert
        assert_eq!(count.load(Ordering::SeqCst), limit);
        if limit == 2 + usize::from(restart) {
            assert_eq!(result.expect("repair fits").response.answer, "summary");
        } else {
            assert!(is_input_size_error(
                &result.expect_err("budget exhausted").to_string()
            ));
        }
        assert!(is_input_size_error(
            &next.expect_err("shared budget exhausted").to_string()
        ));
    }
}
