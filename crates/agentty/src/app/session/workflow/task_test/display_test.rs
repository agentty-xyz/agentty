use std::sync::{Arc, Mutex};

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus, OneShotError};
use ag_worker::MockRunClient;
use tokio::sync::mpsc;

use super::super::{RunAgentAssistTaskInput, SessionTaskService};
use super::support::{insert_review_session, one_shot_submission};
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};

#[tokio::test]
async fn assist_activity_follows_answer_or_failure_and_is_excluded_from_replay() {
    for fails in [false, true] {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        insert_review_session(&database, AgentModel::ClaudeOpus55.as_str()).await;
        let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
        let mut run_client = MockRunClient::new();
        run_client.expect_submit().once().returning(move |request| {
            emit_assist_activity(&request.activity_tx.expect("assistance activity sink"));

            if fails {
                Err(OneShotError::new("command failed with exit code 7"))
            } else {
                Ok(one_shot_submission("Recovered the session.", 11, 7))
            }
        });

        // Act
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            app_event_tx: mpsc::unbounded_channel().0,
            child_pid: Arc::default(),
            db: database.clone(),
            folder: "fixture".into(),
            id: "session-id".into(),
            run_client: Arc::new(run_client),
            prompt: "Recover the session".into(),
            session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
            session_update_versions: Arc::default(),
            transcript: Arc::clone(&transcript),
        })
        .await;
        let messages = database
            .sessions()
            .load_session_messages("session-id")
            .await
            .expect("saved messages");

        // Assert
        let (kind, content) = if fails {
            assert_eq!(
                result.expect_err("assistance should fail").to_string(),
                "command failed with exit code 7"
            );
            ("workflow_notice", "[Error] Agent assistance failed.")
        } else {
            result.expect("assistance should succeed");
            ("assistant_answer", "Recovered the session.")
        };
        let summary = "Tools: Read ×2 (1 failed)\nSkills: recovery ×1";
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].kind, kind);
        assert_eq!(messages[0].content, content);
        assert_eq!(messages[1].kind, "activity_summary");
        assert_eq!(messages[1].content, summary);
        let transcript = transcript.lock().expect("transcript lock");
        assert_eq!(
            transcript.messages()[1].kind,
            SessionMessageKind::ActivitySummary
        );
        assert_eq!(transcript.messages()[1].content, summary);
        let replay = transcript.replay_text().expect("answer is replayable");
        assert!(replay.contains(content));
        assert!(!replay.contains("Tools:"));
        assert!(!replay.contains("Skills:"));
    }
}

/// Emits lifecycle updates, a retry, and explicit skill usage from a utility.
fn emit_assist_activity(sender: &mpsc::UnboundedSender<ActivityEvent>) {
    let event = ActivityEvent {
        attempt_id: "initial".into(),
        exit_code: None,
        id: "read".into(),
        kind: ActivityKind::Tool,
        name: "Read".into(),
        observed_at: std::time::SystemTime::UNIX_EPOCH,
        parent_id: None,
        status: ActivityStatus::Running,
    };
    sender.send(event.clone()).expect("start event");
    sender
        .send(ActivityEvent {
            status: ActivityStatus::Completed,
            ..event.clone()
        })
        .expect("completion event");
    sender
        .send(ActivityEvent {
            attempt_id: "retry".into(),
            status: ActivityStatus::Failed,
            ..event.clone()
        })
        .expect("retry event");
    sender
        .send(ActivityEvent {
            id: "skill".into(),
            kind: ActivityKind::Skill,
            name: "recovery".into(),
            status: ActivityStatus::Completed,
            ..event
        })
        .expect("skill event");
}

#[tokio::test]
/// Verifies assist tasks reject plain-text one-shot output after both the
/// original parse and the protocol-repair retry fail.
async fn test_run_agent_assist_task_rejects_plain_text_output() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::ClaudeOpus55.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().returning(|_| {
        Err(OneShotError::new(
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
        run_client: Arc::new(run_client),
        prompt: "Resolve conflict".to_string(),
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
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
        .and_then(|transcript| transcript.replay_text())
        .unwrap_or_default();
    assert!(output_text.contains("[Error] Agent assistance failed."));
    assert!(!output_text.contains("plain text"));
    let sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert_eq!(sessions[0].input_tokens, 0);
    assert_eq!(sessions[0].output_tokens, 0);
}
