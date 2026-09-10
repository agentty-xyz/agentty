use std::collections::VecDeque;
use std::sync::Arc;

use ag_agent::{AgentRequestKind, MockAgentChannel};
use tokio::sync::mpsc;

use super::super::super::post_turn::TurnPersonalityPersistence;
use super::super::super::turn::resolve_turn_personality;
use super::super::{SessionCommand, SessionWorkerHandle, SessionWorkerService};
use super::support::{
    assert_first_prompt_remains_retryable, assert_first_prompt_was_accepted,
    assert_preparation_publication_failure_is_retryable, inject_handoff_failure,
    managed_first_prompt, persist_test_personality_state, queue_test_context, resume_command,
};
use crate::domain::personality::Personality;
use crate::domain::session::Status;
use crate::infra::personality::MockPersonalityCatalogClient;

#[tokio::test]
async fn test_preparation_marker_failure_preserves_prompt_and_never_runs_the_agent() {
    // Arrange, Act, Assert
    for trigger in [
        "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
         NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
        "CREATE TRIGGER reject_transcript BEFORE INSERT ON session_message BEGIN SELECT \
         RAISE(ABORT, 'transcript rejected'); END",
        "CREATE TRIGGER reject_draft_clear BEFORE UPDATE OF is_draft ON session WHEN NEW.is_draft \
         = 0 BEGIN SELECT RAISE(ABORT, 'draft clear rejected'); END",
    ] {
        assert_preparation_publication_failure_is_retryable(trigger).await;
    }
}

#[tokio::test]
async fn test_unsaved_first_prompt_cleans_attachments_when_handoff_fails() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_id = app.create_session().await.expect("session");
    let (_images, prompt) = managed_first_prompt();
    let image_path = prompt.attachments[0].local_image_path.clone();
    sqlx::query(
        "CREATE TRIGGER reject_start BEFORE INSERT ON session_operation BEGIN SELECT RAISE(ABORT, \
         'handoff rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject operation insertion");

    // Act
    let result = app.start_session(&session_id, prompt).await;

    // Assert
    assert!(
        result
            .expect_err("handoff should fail")
            .to_string()
            .contains("handoff rejected")
    );
    assert!(
        !image_path.exists(),
        "unsaved attachments have no retry owner"
    );
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .expect("session")
            .status,
        Status::Draft
    );
}

#[tokio::test]
async fn test_saved_first_prompt_retries_failed_handoffs_without_losing_attachments() {
    for (initial_status, reject_delivery) in [
        (Status::Draft, false),
        (Status::Draft, true),
        (Status::Review, false),
        (Status::Review, true),
    ] {
        // Arrange
        let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let session_id = app.create_session().await.expect("session");
        crate::test_support::set_session_status_for_test(&mut app, &session_id, initial_status);
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &initial_status.to_string(), 0)
            .await
            .expect("persist status");
        let (_images, prompt) = managed_first_prompt();
        let prompt_json = serde_json::to_string(&prompt).expect("prompt JSON");
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(&session_id, &prompt_json)
            .await
            .expect("save prompt");
        let rejected_receiver =
            inject_handoff_failure(&mut app, &pool, &session_id, reject_delivery).await;

        // Act
        app.retry_workspace_preparation(&session_id)
            .await
            .expect("first attempt");
        crate::test_support::finish_session_creation_tasks(&mut app).await;

        // Assert
        assert_first_prompt_remains_retryable(&app, &pool, &session_id, &prompt, initial_status)
            .await;
        if let Some(mut receiver) = rejected_receiver {
            if initial_status == Status::Draft {
                let rejected = receiver.try_recv().expect("gated command");
                assert!(rejected.ready_rx.expect("gate").await.is_err());
            } else {
                assert!(receiver.try_recv().is_err());
            }
            sqlx::query("DROP TRIGGER reject_start")
                .execute(&pool)
                .await
                .expect("restore persistence");
        }

        // Act: retry through the same setup action used by the UI.
        let (sender, mut receiver) = mpsc::unbounded_channel();
        app.sessions.worker_service_mut().workers.insert(
            session_id.clone().into(),
            SessionWorkerHandle {
                queued_work_sequence: Arc::default(),
                sender,
                wakeup: Arc::default(),
            },
        );
        app.retry_workspace_preparation(&session_id)
            .await
            .expect("retry");
        crate::test_support::finish_session_creation_tasks(&mut app).await;
        let accepted = receiver.try_recv().expect("accepted first turn");

        // Assert
        if let Some(ready_rx) = accepted.ready_rx {
            ready_rx.await.expect("foreground released worker");
        }
        let expected_kind = if initial_status == Status::Draft {
            AgentRequestKind::SessionStart
        } else {
            AgentRequestKind::SessionResume
        };
        assert!(matches!(accepted.command, SessionCommand::Run {
                request_kind, prompt: accepted_prompt, ..
            } if accepted_prompt == prompt && request_kind == expected_kind));
        assert!(
            receiver.try_recv().is_err(),
            "retry queues exactly one turn"
        );
        assert_first_prompt_was_accepted(&app, &session_id, &prompt, initial_status).await;
    }
}

#[tokio::test]
async fn test_first_prompt_handoff_preserves_a_terminal_status() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_git_test_app().await;
    let session_id = app.create_session().await.expect("session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Done);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    app.sessions.worker_service_mut().workers.insert(
        session_id.clone().into(),
        SessionWorkerHandle {
            queued_work_sequence: Arc::default(),
            sender,
            wakeup: Arc::default(),
        },
    );

    // Act: a terminal state must not be overwritten by foreground setup.
    app.sessions
        .start_session(&app.services, &session_id, "late first prompt")
        .await
        .expect("handoff");
    let command = receiver.try_recv().expect("queued command");
    command.ready_rx.expect("gate").await.expect("released");
    app.sessions.sync_from_handles();

    // Assert
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .expect("session")
            .status,
        Status::Done
    );
}

#[tokio::test]
async fn test_restart_retains_unreleased_first_prompt_and_reclaims_its_operation() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_git_test_app().await;
    let session_id = app.create_session().await.expect("session");
    let (_images, prompt) = managed_first_prompt();
    app.services
        .db()
        .sessions()
        .save_preparation_prompt(&session_id, &serde_json::to_string(&prompt).expect("JSON"))
        .await
        .expect("save");
    let mut command = resume_command(&format!("workspace:{session_id}"));
    if let SessionCommand::Run { request_kind, .. } = &mut command {
        *request_kind = AgentRequestKind::SessionStart;
    }
    let gate = app
        .sessions
        .enqueue_gated_session_command(&app.services, &session_id, command)
        .await
        .expect("queue behind gate");
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, "InProgress", 0)
        .await
        .expect("legacy foreground status");

    // Act: restart after persistence, before releasing the worker gate.
    drop(gate);
    app.services
        .db()
        .sessions()
        .recover_session_preparations()
        .await
        .expect("recover saved prompt");
    SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        app.services.db(),
        app.services.base_path(),
        app.services.git_client(),
        123,
    )
    .await
    .expect("recover operations");
    let row = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("session");
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&session_id)
        .await
        .expect("load")
        .expect("preparation");

    // Assert
    assert_eq!(row.status, "Draft");
    assert!(preparation.prompt.is_some());
    assert!(prompt.attachments[0].local_image_path.exists());
    app.services
        .db()
        .sessions()
        .reclaim_preparation_prompt_operation(&session_id)
        .await
        .expect("reclaim unstarted handoff");
    assert!(
        app.services
            .db()
            .sessions()
            .preparation_prompt_operation_status(&session_id)
            .await
            .expect("operation")
            .is_none()
    );
}

#[tokio::test]
async fn test_resolve_turn_personality_marks_new_and_unchanged_prompts() {
    // Arrange
    let (mut context, db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.sessions()
        .update_session_personality_id("sess1", Some("reviewer".to_string()))
        .await
        .expect("personality selection should persist");
    let personality = Personality {
        description: "Reviews code".to_string(),
        id: "reviewer".to_string(),
        name: "Code Reviewer".to_string(),
        prompt: "Review carefully.".to_string(),
    };
    let fingerprint = personality.fingerprint();
    let mut personality_catalog_client = MockPersonalityCatalogClient::new();
    personality_catalog_client
        .expect_resolve()
        .times(2)
        .returning(move |_, _| {
            let personality = personality.clone();
            Box::pin(async move { Some(personality) })
        });
    context.personality_catalog_client = Arc::new(personality_catalog_client);

    // Act
    let changed = resolve_turn_personality(&context).await;
    persist_test_personality_state(&db, changed.persistence.clone()).await;
    let unchanged = resolve_turn_personality(&context).await;

    // Assert
    assert_eq!(
        changed.prompt,
        ag_agent::PersonalityPrompt::active("Review carefully.".to_string(), true)
    );
    assert_eq!(
        unchanged.prompt,
        ag_agent::PersonalityPrompt::active("Review carefully.".to_string(), false)
    );
    assert_eq!(
        unchanged.persistence,
        TurnPersonalityPersistence {
            applied_personality_id: Some("reviewer".to_string()),
            applied_personality_prompt_hash: Some(fingerprint),
        }
    );
}
