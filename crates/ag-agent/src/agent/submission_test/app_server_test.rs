use std::sync::{Arc, Mutex};

use tempfile::tempdir;

use crate::agent::submission::{
    OneShotRequest, attempt_one_shot_app_server_repair, submit_one_shot_with_app_server_client,
};
use crate::app_server::{AppServerError, AppServerTurnResponse, MockAppServerClient};
use crate::channel::AgentRequestKind;
use crate::model::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;

#[tokio::test]
/// Verifies app-server-backed one-shot execution returns the parsed
/// structured answer and usage totals.
async fn test_submit_one_shot_with_app_server_client_returns_protocol_response() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut app_server_client = MockAppServerClient::new();
    app_server_client
        .expect_run_turn()
        .times(1)
        .returning(|request, _| {
            assert_eq!(request.model, AgentModel::Gpt56Sol.as_str());
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));
            assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
            assert_eq!(request.prompt.text, "Generate title");
            assert_eq!(request.speed_mode, SpeedMode::Fast);

            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"Generated title","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 11,
                    output_tokens: 7,
                    pid: Some(42),
                    provider_conversation_id: Some("thread-1".to_string()),
                })
            })
        });
    app_server_client
        .expect_shutdown_session()
        .times(1)
        .returning(|_| Box::pin(async {}));

    // Act
    let response = submit_one_shot_with_app_server_client(
        &app_server_client,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::Gpt56Sol,
            permission_mode: PermissionMode::ReadOnly,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Fast,
        },
    )
    .await
    .expect("one-shot prompt should succeed");

    // Assert
    assert_eq!(
        response.response.answers(),
        vec!["Generated title".to_string()]
    );
    assert_eq!(response.stats.input_tokens, 11);
    assert_eq!(response.stats.output_tokens, 7);
}

#[tokio::test]
/// Verifies app-server turn failures shut down the temporary session and
/// clear the caller's shared child-process slot.
async fn test_submit_one_shot_with_app_server_client_clears_pid_after_turn_failure() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let child_pid = Arc::new(Mutex::new(Some(42)));
    let mut app_server_client = MockAppServerClient::new();
    app_server_client
        .expect_run_turn()
        .times(1)
        .returning(|_, _| {
            Box::pin(async {
                Err(AppServerError::Provider(
                    "app-server turn failed".to_string(),
                ))
            })
        });
    app_server_client
        .expect_shutdown_session()
        .times(1)
        .returning(|_| Box::pin(async {}));

    // Act
    let error = submit_one_shot_with_app_server_client(
        &app_server_client,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: Some(Arc::clone(&child_pid)),
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::Gpt56Sol,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect_err("app-server turn failure should surface");

    // Assert
    assert!(error.contains("app-server turn failed"));
    assert_eq!(
        *child_pid.lock().expect("child pid lock should succeed"),
        None
    );
}

#[tokio::test]
async fn one_shot_app_server_repair_preserves_permissions_and_conversation() {
    for permission_mode in PermissionMode::ALL {
        // Arrange
        let folder = tempdir().expect("workspace");
        let mut client = MockAppServerClient::new();
        client
            .expect_run_turn()
            .times(1)
            .returning(move |request, _| {
                assert_eq!(request.permission_mode, permission_mode);
                assert_eq!(request.session_id, "one-shot-session");
                assert_eq!(
                    request.provider_conversation_id.as_deref(),
                    Some("native-session")
                );

                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"Repaired","questions":[]}"#.into(),
                        context_reset: false,
                        input_tokens: 2,
                        output_tokens: 1,
                        pid: None,
                        provider_conversation_id: Some("native-session".into()),
                    })
                })
            });
        let request = OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Gemini,
            child_pid: None,
            folder: folder.path().to_owned(),
            model: AgentModel::Gemini31Pro,
            permission_mode,
            prompt: "Generate title".into(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        };

        // Act
        let (response, input_tokens, output_tokens) = attempt_one_shot_app_server_repair(
            &client,
            "invalid JSON",
            "malformed",
            request,
            "one-shot-session",
            Some("native-session"),
        )
        .await
        .expect("repair succeeds");

        // Assert
        assert_eq!(response.to_display_text(), "Repaired");
        assert_eq!((input_tokens, output_tokens), (2, 1));
    }
}

#[tokio::test]
/// Verifies app-server-backed one-shot execution rejects plain-text
/// utility output after both the original parse and the protocol-repair
/// retry fail.
async fn test_submit_one_shot_with_app_server_client_rejects_plain_text_utility_output() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut app_server_client = MockAppServerClient::new();
    app_server_client
        .expect_run_turn()
        .times(2)
        .returning(|request, _| {
            assert_eq!(request.model, AgentModel::Gpt56Sol.as_str());
            assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
            assert_eq!(request.speed_mode, SpeedMode::Fast);

            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: "plain text".to_string(),
                    context_reset: false,
                    input_tokens: 2,
                    output_tokens: 1,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    app_server_client
        .expect_shutdown_session()
        .times(1)
        .returning(|_| Box::pin(async {}));

    // Act
    let error = submit_one_shot_with_app_server_client(
        &app_server_client,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::Gpt56Sol,
            permission_mode: PermissionMode::ReadOnly,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Fast,
        },
    )
    .await
    .expect_err("plain-text utility output should fail");

    // Assert
    assert!(error.contains("did not match the required JSON schema"));
    assert!(error.contains("debug_details:"));
    assert!(error.contains("response:\nplain text"));
}

#[tokio::test]
/// Verifies app-server-backed non-utility one-shot execution still
/// rejects plain-text output after both the original parse and the
/// protocol-repair retry fail.
async fn test_submit_one_shot_with_app_server_client_rejects_plain_text_non_utility_output() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut app_server_client = MockAppServerClient::new();
    app_server_client
        .expect_run_turn()
        .times(2)
        .returning(|request, _| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::SessionStart
            ));

            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: "plain text".to_string(),
                    context_reset: false,
                    input_tokens: 2,
                    output_tokens: 1,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    app_server_client
        .expect_shutdown_session()
        .times(1)
        .returning(|_| Box::pin(async {}));

    // Act
    let error = submit_one_shot_with_app_server_client(
        &app_server_client,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::Gpt56Sol,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect_err("invalid non-utility output should fail");

    // Assert
    assert!(error.contains("did not match the required JSON schema"));
    assert!(error.contains("debug_details:"));
    assert!(error.contains("response:\nplain text"));
}
