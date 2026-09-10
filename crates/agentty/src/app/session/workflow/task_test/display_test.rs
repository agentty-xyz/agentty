use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::MockOneShotClient;
use tokio::sync::mpsc;

use super::super::{RunAgentAssistTaskInput, SessionTaskService};
use super::support::insert_review_session;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::SessionTranscript;

#[tokio::test]
/// Verifies assist tasks reject plain-text one-shot output after both the
/// original parse and the protocol-repair retry fail.
async fn test_run_agent_assist_task_rejects_plain_text_output() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::ClaudeOpus5.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().returning(|_| {
        Err(agent::OneShotError::new(
            "One-shot agent output did not match the required JSON schema\nresponse:\nplain text",
        ))
    });

    // Act
    let error = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: temp_dir.path().to_path_buf(),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        prompt: "Resolve conflict".to_string(),
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    })
    .await
    .expect_err("plain-text utility output should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("did not match the required JSON schema")
    );
    assert!(error.to_string().contains("response:\nplain text"));
    let output_text = transcript
        .lock()
        .ok()
        .and_then(|transcript| transcript.replay_text());
    assert_eq!(output_text, None);
    let sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert_eq!(sessions[0].input_tokens, 0);
    assert_eq!(sessions[0].output_tokens, 0);
}
