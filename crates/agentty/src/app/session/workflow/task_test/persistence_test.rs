use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_git::{GitError, MockGitClient};
use tokio::sync::oneshot;

use super::super::SessionTaskService;
use crate::db::AppRepositories;
use crate::domain::session::{SessionDiffStats, SessionHandles, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::infra::db::DbError;
use crate::infra::fs;

#[tokio::test]
async fn refresh_diff_stats_marks_git_failures_unknown_without_erasing_totals() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_diff_stats(7, 3, true, "session-id", "S")
        .await
        .expect("failed to seed diff stats");
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().times(1).return_const(true);
    let mut git_client = MockGitClient::new();
    git_client.expect_diff().times(1).returning(|_, _| {
        Box::pin(async { Err(GitError::OutputParse("diff failed".to_string())) })
    });

    // Act
    let diff_stats = SessionTaskService::refresh_persisted_session_diff_stats(
        &database,
        &fs_client,
        &git_client,
        "session-id",
        &PathBuf::from("/tmp/missing-session"),
    )
    .await;
    let sessions = database
        .sessions()
        .load_sessions_for_project(project_id)
        .await
        .expect("failed to reload session");

    // Assert
    assert_eq!(diff_stats, Some(SessionDiffStats::Unknown));
    assert_eq!(sessions[0].added_lines, 7);
    assert_eq!(sessions[0].deleted_lines, 3);
    assert_eq!(sessions[0].has_diff, None);
    assert_eq!(sessions[0].size, "S");
}

#[tokio::test]
async fn test_workflow_notice_append_survives_hydration_during_persistence() {
    // Arrange
    let handles = SessionHandles::new_unloaded(Status::Review);
    let loaded_transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
    ]);
    let (persistence_started_tx, persistence_started_rx) = oneshot::channel();
    let (release_persistence_tx, release_persistence_rx) = oneshot::channel();
    let transcript = Arc::clone(&handles.transcript);
    let append_task = tokio::spawn(async move {
        SessionTaskService::append_live_and_persist_transcript_message(
            &transcript,
            "session-id",
            SessionMessageKind::WorkflowNotice,
            "\n[Sync] Successfully synced onto main\n",
            async move {
                let _ = persistence_started_tx.send(());
                let _ = release_persistence_rx.await;

                Ok(())
            },
            "failed to persist workflow notice",
        )
        .await;
    });
    persistence_started_rx
        .await
        .expect("persistence should start");

    // Act
    let hydrated_transcript = handles.transcript_snapshot_with_loaded(Some(&loaded_transcript));
    release_persistence_tx
        .send(())
        .expect("persistence should still be waiting");
    append_task.await.expect("append task should finish");

    // Assert
    assert_eq!(
        hydrated_transcript,
        Some(SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
            SessionMessage::new(
                2,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced onto main\n"
            ),
        ]))
    );
    assert_eq!(
        handles
            .transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .messages(),
        &[
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
            SessionMessage::new(
                2,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced onto main\n"
            ),
        ]
    );
}

#[tokio::test]
async fn test_workflow_notice_append_remains_live_after_persistence_error() {
    // Arrange
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));

    // Act
    SessionTaskService::append_live_and_persist_transcript_message(
        &transcript,
        "session-id",
        SessionMessageKind::WorkflowNotice,
        "\n[Sync Error] persistence failed\n",
        async { Err(DbError::Query(sqlx::Error::RowNotFound)) },
        "failed to persist workflow notice",
    )
    .await;

    // Assert
    assert_eq!(
        transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .messages(),
        &[SessionMessage::new(
            0,
            SessionMessageKind::WorkflowNotice,
            "\n[Sync Error] persistence failed\n"
        )]
    );
}
