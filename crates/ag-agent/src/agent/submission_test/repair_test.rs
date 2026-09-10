use std::process::Command;
use std::sync::Arc;

use tempfile::tempdir;

use super::support::mock_shell_command;
use crate::agent::MockAgentBackend;
use crate::agent::submission::{
    OneShotRequest, attempt_one_shot_app_server_repair, submit_one_shot_with_backend,
};
use crate::app_server::MockAppServerClient;
use crate::channel::AgentRequestKind;
use crate::model::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;

#[tokio::test]
async fn oversized_one_shot_responses_do_not_launch_repair() {
    // Arrange
    let folder = tempdir().expect("workspace");
    let oversized = "x".repeat(128 * 1024 + 1);
    let response_path = folder.path().join("response.txt");
    std::fs::write(&response_path, &oversized).expect("response fixture");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().times(1).returning(move |_| {
        let mut command = Command::new("cat");
        command.arg(&response_path);

        Ok(command)
    });
    let client = MockAppServerClient::new();
    let request = OneShotRequest {
        provider_call_budget: None,
        agent_kind: AgentKind::Claude,
        child_pid: None,
        folder: folder.path().to_owned(),
        model: AgentModel::ClaudeSonnet5,
        permission_mode: PermissionMode::AutoEdit,
        prompt: "Generate title".into(),
        request_kind: AgentRequestKind::UtilityPrompt,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::Normal,
    };

    // Act
    let cli_error = submit_one_shot_with_backend(&backend, request.clone())
        .await
        .expect_err("oversized CLI response");
    let native_error = attempt_one_shot_app_server_repair(
        &client,
        "bad JSON",
        &oversized,
        request,
        "repair-limit",
        None,
    )
    .await
    .expect_err("oversized native response");

    // Assert
    assert!(cli_error.contains("lossless repair limit"));
    assert!(native_error.contains("lossless repair limit"));
}

#[tokio::test]
/// Verifies one-shot execution rejects plain-text utility output after
/// both the original parse and the protocol-repair retry fail.
async fn test_submit_one_shot_with_backend_rejects_plain_text_utility_output() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .times(2)
        .returning(|request| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));

            Ok(mock_shell_command("plain text", "", 0))
        });

    // Act
    let error = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: None,
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
    .expect_err("plain-text utility output should fail");

    // Assert
    assert!(error.contains("did not match the required JSON schema"));
    assert!(error.contains("debug_details:"));
    assert!(error.contains("direct_json_error_location: line 1, column 1"));
    assert!(error.contains("response:\nplain text"));
}

#[tokio::test]
/// Verifies one-shot execution rejects wrapped non-schema utility output
/// after both the original parse and the protocol-repair retry fail.
async fn test_submit_one_shot_with_backend_rejects_wrapped_plain_text_utility_output() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .times(2)
        .returning(|request| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));

            Ok(mock_shell_command(
                r#"{"result":"plain text","usage":{"input_tokens":2,"output_tokens":1}}"#,
                "",
                0,
            ))
        });

    // Act
    let error = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect_err("wrapped plain-text utility output should fail");

    // Assert — the provider parser extracts "plain text" from the
    // `result` wrapper, so the protocol parser sees raw text, not JSON
    // keys.
    assert!(error.contains("did not match the required JSON schema"));
    assert!(error.contains("direct_json_error:"));
    assert!(error.contains("response:\nplain text"));
}

#[tokio::test]
/// Verifies one-shot execution recovers a trailing protocol payload when
/// the provider prepends extra prose before the final JSON object.
async fn test_submit_one_shot_with_backend_recovers_wrapped_protocol_output() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .times(1)
        .returning(|request| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));
            assert_eq!(request.prompt, "Generate title");

            Ok(mock_shell_command(
                concat!(
                    "Now I have full context.\n",
                    r#"{"answer":"Generated title","questions":[]}"#
                ),
                "",
                0,
            ))
        });

    // Act
    let response = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect("wrapped protocol output should succeed");

    // Assert
    assert_eq!(
        response.response.answers(),
        vec!["Generated title".to_string()]
    );
}

#[tokio::test]
/// Verifies one-shot execution recovers valid output when the initial
/// parse fails but the protocol-repair retry returns valid protocol JSON.
async fn test_submit_one_shot_with_backend_recovers_via_protocol_repair() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let call_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().times(2).returning({
        let counter = std::sync::Arc::clone(&call_counter);

        move |request| {
            assert_eq!(request.speed_mode, SpeedMode::Fast);
            let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if call_number == 0 {
                Ok(mock_shell_command("plain text", "", 0))
            } else {
                Ok(mock_shell_command(
                    r#"{"answer":"Repaired title","questions":[]}"#,
                    "",
                    0,
                ))
            }
        }
    });

    // Act
    let response = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::Gpt56Sol,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Fast,
        },
    )
    .await
    .expect("repair retry should succeed");

    // Assert
    assert_eq!(
        response.response.answers(),
        vec!["Repaired title".to_string()]
    );
}

#[tokio::test]
async fn focused_review_repairs_trailing_text_with_direct_review_schema() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let call_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().times(2).returning({
        let counter = Arc::clone(&call_counter);

        move |request| {
            assert_eq!(request.request_kind, &AgentRequestKind::FocusedReview);
            let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if call_number == 0 {
                return Ok(mock_shell_command(
                    r#"{"project_impact":[],"suggestions":[]} trailing text"#,
                    "",
                    0,
                ));
            }

            assert!(request.prompt.contains("Complete malformed response:"));
            let prompt = crate::agent::prompt::build_cli_prompt_text(
                request,
                ag_protocol::ProtocolSchemaInstructionMode::PromptSchema,
                "Gemini",
            )
            .expect("repair envelope");
            assert!(prompt.contains("\"title\": \"FocusedReview\""));
            assert_eq!(prompt.matches("Authoritative JSON Schema:").count(), 1);
            assert!(!prompt.contains("\"answer\""));

            Ok(mock_shell_command(
                r#"{"project_impact":["Review repaired."],"suggestions":[]}"#,
                "",
                0,
            ))
        }
    });

    // Act
    let response = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: temp_directory.path().to_path_buf(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::ReadOnly,
            prompt: "Review the diff".to_string(),
            request_kind: AgentRequestKind::FocusedReview,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        },
    )
    .await
    .expect("focused review repair should succeed");

    // Assert
    assert_eq!(
        response.response.answer,
        r#"{"project_impact":["Review repaired."],"suggestions":[]}"#
    );
}

#[tokio::test]
/// Verifies one-shot execution still rejects blank utility responses
/// after both the original parse and the protocol-repair retry fail.
async fn test_submit_one_shot_with_backend_rejects_blank_utility_output() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|request| {
        assert!(matches!(
            request.request_kind,
            AgentRequestKind::UtilityPrompt
        ));

        Ok(mock_shell_command("   ", "", 0))
    });

    // Act
    let error = submit_one_shot_with_backend(
        &backend,
        OneShotRequest {
            provider_call_budget: None,
            agent_kind: AgentKind::Codex,
            child_pid: None,
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
    .expect_err("blank utility output should fail");

    // Assert
    assert!(error.contains("did not match the required JSON schema"));
    assert!(error.contains("trimmed_len: 0 chars"));
    assert!(error.contains("response:\n"));
}
