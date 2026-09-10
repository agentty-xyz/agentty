use std::sync::{Arc, Mutex};

use ag_agent::{AgentSelectionMetadata, MockOneShotClient};
use tokio::sync::mpsc;

use super::super::{
    RunAgentAssistTaskInput, SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER, SessionTaskService,
    append_agentty_coauthor_trailer, validate_generated_commit_message,
};
use super::support::{insert_review_session, one_shot_submission};
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;

#[test]
/// Verifies append-only handling adds the coauthor trailer once
/// when the setting is enabled.
fn test_append_agentty_coauthor_trailer_appends_trailer_once() {
    // Arrange
    let commit_message = "Refine settings page";

    // Act
    let appended_commit_message = append_agentty_coauthor_trailer(commit_message, true);

    // Assert
    assert_eq!(
        appended_commit_message,
        format!("Refine settings page\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}")
    );
}

#[test]
/// Verifies append-only handling leaves the generated message unchanged
/// when the setting is disabled.
fn test_append_agentty_coauthor_trailer_leaves_message_unchanged_when_disabled() {
    // Arrange
    let commit_message = "Refine settings page";

    // Act
    let appended_commit_message = append_agentty_coauthor_trailer(commit_message, false);

    // Assert
    assert_eq!(appended_commit_message, "Refine settings page");
}

#[test]
/// Verifies generated commit-message validation rejects model output that
/// already includes the Agentty trailer.
fn test_validate_generated_commit_message_rejects_agentty_trailer() {
    // Arrange
    let commit_message =
        format!("Refine settings page\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}");

    // Act
    let error = validate_generated_commit_message(&commit_message)
        .expect_err("generated trailer should fail validation");

    // Assert
    assert_eq!(
        error.to_string(),
        "Session commit message model must not emit the Agentty coauthor trailer"
    );
}

#[tokio::test]
/// Verifies auto-commit falls back through smart and session selections
/// when the fast-model setting is absent.
async fn test_load_auto_commit_agent_setting_falls_back_through_defaults() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let project_id = database
        .sessions()
        .load_session_project_id("session-id")
        .await
        .expect("failed to load session project id")
        .expect("session should have project id");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartModel,
            AgentModel::Gemini31Pro.as_str(),
        )
        .await
        .expect("failed to persist default smart model");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartAgent,
            AgentKind::Antigravity.name(),
        )
        .await
        .expect("failed to persist default smart agent");

    // Act
    let smart_fallback_agent = SessionTaskService::load_auto_commit_agent_setting(
        &database,
        "session-id",
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await;

    // Assert
    assert_eq!(
        smart_fallback_agent,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini31Pro)
    );

    // Arrange
    database
        .settings()
        .upsert_project_setting(project_id, SettingName::DefaultSmartModel, "invalid")
        .await
        .expect("failed to persist invalid smart model");

    // Act
    let session_fallback_agent = SessionTaskService::load_auto_commit_agent_setting(
        &database,
        "session-id",
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await;

    // Assert
    assert_eq!(
        session_fallback_agent,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
    );
}

#[tokio::test]
/// Verifies one-shot assist output unwraps structured protocol answers
/// before persistence and session usage updates.
async fn test_run_agent_assist_task_unwraps_one_shot_answer_without_raw_json() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::ClaudeOpus5.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let child_pid = Arc::new(Mutex::new(None));
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let expected_child_pid = Arc::clone(&child_pid);
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(move |request| {
            assert_eq!(request.prompt, "Resolve conflict");
            assert!(Arc::ptr_eq(
                request.child_pid.as_ref().expect("CLI cancellation slot"),
                &expected_child_pid,
            ));

            Ok(one_shot_submission("Resolved the rebase conflict.", 11, 7))
        });

    // Act
    let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
        app_event_tx,
        child_pid: Arc::clone(&child_pid),
        db: database.clone(),
        folder: temp_dir.path().to_path_buf(),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        prompt: "Resolve conflict".to_string(),
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    })
    .await;

    // Assert
    assert!(
        result.is_ok(),
        "assist task should succeed: {:?}",
        result.err()
    );
    let output_text = transcript
        .lock()
        .ok()
        .and_then(|transcript| transcript.replay_text())
        .unwrap_or_default();
    assert!(output_text.contains("Resolved the rebase conflict."));
    assert!(!output_text.contains(r#"{"answer""#));
    assert_eq!(*child_pid.lock().expect("failed to lock child pid"), None);
    let sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert_eq!(sessions[0].input_tokens, 11);
    assert_eq!(sessions[0].output_tokens, 7);
}

#[tokio::test]
/// Verifies the coauthor trailer setting defaults to disabled when the
/// project has not persisted a value yet.
async fn test_load_include_coauthored_by_agentty_setting_defaults_to_false() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;

    // Act
    let include_coauthored_by_agentty =
        SessionTaskService::load_include_coauthored_by_agentty_setting(&database, "session-id")
            .await;

    // Assert
    assert!(!include_coauthored_by_agentty);
}

#[tokio::test]
/// Verifies the coauthor trailer setting defaults to disabled when the
/// stored value cannot be parsed as a boolean.
async fn test_load_include_coauthored_by_agentty_setting_defaults_invalid_value_to_false() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let project_id = database
        .sessions()
        .load_session_project_id("session-id")
        .await
        .expect("failed to load session project id")
        .expect("session should have project id");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::IncludeCoauthoredByAgentty,
            "invalid-bool",
        )
        .await
        .expect("failed to persist invalid coauthor flag");

    // Act
    let include_coauthored_by_agentty =
        SessionTaskService::load_include_coauthored_by_agentty_setting(&database, "session-id")
            .await;

    // Assert
    assert!(!include_coauthored_by_agentty);
}
