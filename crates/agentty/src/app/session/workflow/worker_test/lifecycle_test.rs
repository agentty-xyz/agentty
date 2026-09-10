use std::collections::VecDeque;

use ag_agent::{AgentRequestKind, MockAgentChannel};

use super::super::super::post_turn::TurnPersonalityPersistence;
use super::super::super::turn::resolve_turn_personality;
use super::super::{SessionCommand, SessionWorkerService, TurnMetadata};
use super::support::{
    assert_fork_history_replayed, auto_commit_one_shot_client, capture_prepared_fork_reply,
    inject_handoff_failure, persist_test_personality_state, preparation_test_worker_context,
    prepare_fork_with_saved_reply, queue_test_context,
};
use crate::app::SessionManager;
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::turn_prompt::TurnPrompt;

#[tokio::test]
async fn test_preparation_transcript_recovers_initial_prompt_but_keeps_repeated_fork_replies() {
    // Arrange
    let (context, _db, _queue, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Draft).await;
    let prompt = TurnPrompt::from_text("repeat this prompt".to_string());

    // Act: recover an older initial transcript, then repeat it in a fork.
    SessionWorkerService::append_preparation_prompt(
        &context,
        &AgentRequestKind::SessionStart,
        &prompt,
    );
    SessionWorkerService::append_preparation_prompt(
        &context,
        &AgentRequestKind::SessionStart,
        &prompt,
    );
    SessionWorkerService::append_preparation_prompt(
        &context,
        &AgentRequestKind::SessionResume,
        &prompt,
    );
    let transcript = context.transcript.lock().expect("transcript");
    let messages = transcript.messages();

    // Assert
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message.kind == SessionMessageKind::UserPrompt
                && message.content == prompt.text)
    );
}

#[tokio::test]
async fn test_cancel_after_preparation_marker_skips_start_and_fork_reply() {
    for (status, request_kind) in [
        (Status::Draft, AgentRequestKind::SessionStart),
        (Status::Review, AgentRequestKind::SessionResume),
    ] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let session_id = app.create_draft_session().await.expect("draft");
        app.services
            .db()
            .sessions()
            .insert_session_preparation(&session_id, "main")
            .await
            .expect("preparation");
        SessionManager::prepare_reserved_session(&app.services, &session_id)
            .await
            .expect("workspace");
        crate::test_support::set_session_status_for_test(&mut app, &session_id, status);
        let prompt = TurnPrompt::from_text("saved first turn".to_string());
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(&session_id, &serde_json::to_string(&prompt).expect("JSON"))
            .await
            .expect("save");
        let operation_id = format!("workspace:{session_id}");
        let context = preparation_test_worker_context(&app, &session_id);
        let command = SessionCommand::Run {
            operation_id: operation_id.clone(),
            request_kind,
            replay_transcript: None,
            prompt: prompt.clone(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: context.session_agent,
            },
        };
        app.services
            .db()
            .operations()
            .insert_session_operation(&operation_id, &session_id, command.kind())
            .await
            .expect("queue");
        // Act: cancel between the committed marker and the worker's
        // second skip check, before it publishes InProgress.
        assert!(
            SessionWorkerService::begin_preparation_prompt(
                &context,
                &operation_id,
                &prompt.transcript_text(),
            )
            .await
            .expect("execution marker")
        );
        assert_eq!(*context.status.lock().expect("status"), status);
        app.cancel_session(&session_id).await.expect("cancel");
        let cancel_requested = context
            .db
            .operations()
            .is_cancel_requested_for_operation(&operation_id)
            .await
            .expect("cancel flag");
        let skip = SessionWorkerService::should_skip_worker_command(&context, &operation_id).await;
        let result = SessionWorkerService::process_session_command(
            &context,
            &auto_commit_one_shot_client(),
            command,
        )
        .await;
        app.wait_for_background_cleanup_tasks().await;

        // Assert
        assert!(skip, "the post-marker check must stop provider execution");
        assert!(result.is_none());
        assert!(cancel_requested);
        assert!(
            !context
                .db
                .operations()
                .is_session_operation_unfinished(&operation_id)
                .await
                .expect("operation finished")
        );
        assert!(context.cancel_token.lock().expect("token").is_cancelled());
        assert_eq!(*context.status.lock().expect("status"), Status::Canceled);
        assert!(!context.folder.exists());
    }
}

#[tokio::test]
async fn test_preparation_acceptance_refreshes_the_live_draft_flag() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_git_test_app().await;
    let session_id = app.create_draft_session().await.expect("draft");
    app.stage_draft_message(&session_id, "saved draft")
        .await
        .expect("stage");
    let sessions = app.services.db().sessions();
    sessions
        .insert_session_preparation(&session_id, "main")
        .await
        .expect("prepare");
    sessions
        .save_preparation_prompt(&session_id, "saved draft")
        .await
        .expect("save");
    sessions
        .update_session_preparation(
            &session_id,
            crate::infra::db::SessionPreparationState::Ready,
            None,
        )
        .await
        .expect("ready");
    let operation_id = format!("workspace:{session_id}");
    app.services
        .db()
        .operations()
        .insert_session_operation(&operation_id, &session_id, "start_prompt")
        .await
        .expect("queue");
    let context = preparation_test_worker_context(&app, &session_id);
    assert!(
        app.sessions
            .session_for_id(&session_id)
            .expect("draft")
            .is_draft_session()
    );

    // Act
    assert!(
        SessionWorkerService::begin_preparation_prompt(&context, &operation_id, "saved draft")
            .await
            .expect("accept")
    );
    app.process_pending_app_events().await;

    // Assert
    assert!(
        !app.sessions
            .session_for_id(&session_id)
            .expect("started draft")
            .is_draft_session()
    );
}

#[tokio::test]
async fn test_fork_retry_replays_history_after_insertion_or_delivery_failure() {
    for reject_delivery in [false, true] {
        // Arrange
        let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let session_id = prepare_fork_with_saved_reply(&mut app).await;
        let rejected_receiver =
            inject_handoff_failure(&mut app, &pool, &session_id, reject_delivery).await;

        // Act
        app.retry_workspace_preparation(&session_id)
            .await
            .expect("first attempt");
        crate::test_support::finish_session_creation_tasks(&mut app).await;

        // Assert
        assert!(app.sessions.should_replay_history(&session_id));
        if let Some(mut receiver) = rejected_receiver {
            assert!(receiver.try_recv().is_err());
            sqlx::query("DROP TRIGGER reject_start")
                .execute(&pool)
                .await
                .expect("restore insertion");
        }

        // Act
        let (command, _receiver) = capture_prepared_fork_reply(&mut app, &session_id).await;

        // Assert
        assert_fork_history_replayed(&command);
        assert!(!app.sessions.should_replay_history(&session_id));
    }
}

#[tokio::test]
async fn test_fork_retry_replays_history_after_worker_acceptance_failure_only_once() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_id = prepare_fork_with_saved_reply(&mut app).await;
    let (command, _receiver) = capture_prepared_fork_reply(&mut app, &session_id).await;
    assert_fork_history_replayed(&command);
    assert!(!app.sessions.should_replay_history(&session_id));
    sqlx::query(
        "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
         NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject acceptance");
    let context = preparation_test_worker_context(&app, &session_id);

    // Act
    let rejected = SessionWorkerService::begin_preparation_prompt(
        &context,
        command.operation_id(),
        "Continue the copied conversation",
    )
    .await;
    sqlx::query("DROP TRIGGER reject_transfer")
        .execute(&pool)
        .await
        .expect("restore acceptance");
    let (retry, mut receiver) = capture_prepared_fork_reply(&mut app, &session_id).await;

    // Assert
    assert!(rejected.is_err());
    assert_fork_history_replayed(&retry);

    // Act: after durable acceptance, an ordinary follow-up must not replay
    // again.
    assert!(
        SessionWorkerService::begin_preparation_prompt(
            &context,
            retry.operation_id(),
            "Continue the copied conversation"
        )
        .await
        .expect("accept retry")
    );
    app.services
        .db()
        .operations()
        .mark_session_operation_done(retry.operation_id())
        .await
        .expect("finish reply");
    assert!(
        app.sessions
            .reply(&app.services, &session_id, "Next reply")
            .await
    );
    let following = receiver.try_recv().expect("following reply").command;

    // Assert
    assert!(matches!(
        following,
        SessionCommand::Run {
            replay_transcript: None,
            ..
        }
    ));
}

#[tokio::test]
async fn test_resolve_turn_personality_clears_removed_selection() {
    // Arrange
    let (context, db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    persist_test_personality_state(
        &db,
        TurnPersonalityPersistence {
            applied_personality_id: Some("reviewer".to_string()),
            applied_personality_prompt_hash: Some("prior-hash".to_string()),
        },
    )
    .await;

    // Act
    let resolution = resolve_turn_personality(&context).await;

    // Assert
    assert_eq!(
        resolution.prompt,
        ag_agent::PersonalityPrompt::cleared(true)
    );
    assert_eq!(
        resolution.persistence,
        TurnPersonalityPersistence::default()
    );
}
