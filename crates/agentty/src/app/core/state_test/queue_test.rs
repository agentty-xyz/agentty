use std::path::PathBuf;
use std::sync::Arc;

use super::support::{session_creation_resources, test_turn_applied_state, test_view_app_mode};
use crate::app::AppError;
use crate::app::core::event::AppEvent;
use crate::domain::session::{QueuedMessage, SessionFollowUpTask, SessionStats, Status};
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn queued_session_work_has_tick_driven_ui() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut session = crate::test_support::SessionFixtureBuilder::new()
        .id("session-id")
        .status(Status::Review)
        .build();
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(
            0,
            "sync after this turn".to_string(),
        )),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::SyncQueue,
        turn_position: None,
    });
    app.sessions.push_session(session);
    app.mode = test_view_app_mode("session-id");

    // Act
    let queued_action_has_tick_driven_ui = app.has_visible_tick_driven_ui();
    let session = app
        .sessions
        .sessions_mut()
        .first_mut()
        .expect("queued session should remain available");
    session
        .transient_messages
        .retract(TransientMessageSlot::SyncQueue);
    session.queued_messages.push(QueuedMessage::new(
        1,
        TurnPrompt::from_text("follow up".to_string()),
    ));
    let queued_message_has_tick_driven_ui = app.has_visible_tick_driven_ui();

    // Assert
    assert!(queued_action_has_tick_driven_ui);
    assert!(queued_message_has_tick_driven_ui);
}

#[tokio::test]
/// Verifies launching an already-linked follow-up task opens its sibling
/// session instead of creating another session.
async fn launch_or_open_selected_follow_up_task_opens_existing_sibling_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: Some("session-2".into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    let mut sibling_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/sibling-session"));
    sibling_session.id = "session-2".into();
    sibling_session.title = Some("Sibling session".to_string());
    app.sessions.push_session(source_session);
    app.sessions.push_session(sibling_session);

    // Act
    app.launch_or_open_selected_follow_up_task("session-1")
        .await
        .expect("follow-up task should open the linked sibling session");

    // Assert
    assert_eq!(app.sessions.selected_session_index(), Some(1));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id == "session-2"
    ));
}

#[tokio::test]
/// Verifies a stale launched-session link is cleared before replacement
/// session creation starts, so a failed launch does not keep retrying the
/// same orphaned sibling id.
async fn launch_or_open_selected_follow_up_task_clears_stale_sibling_link_before_launch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: Some("missing-session".into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    app.sessions.push_session(source_session);

    // Act
    let result = app
        .launch_or_open_selected_follow_up_task("session-1")
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Session(crate::app::SessionError::Workflow(message)))
            if message == "Git branch is required to create a session"
    ));
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(
        app.sessions.sessions()[0].follow_up_tasks[0].launched_session_id,
        None
    );
}

#[tokio::test]
async fn failed_follow_up_preparation_rolls_back_reserved_sibling() {
    // Arrange
    let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let source_id = app.create_session().await.expect("source");
    crate::test_support::set_session_status_for_test(&mut app, &source_id, Status::Review);
    app.sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == source_id)
        .expect("source")
        .follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: None,
        position: 0,
        text: "Follow up on the source".to_string(),
    }];
    let original_resources = session_creation_resources(directory.path()).await;
    sqlx::query(
        "CREATE TRIGGER reject_ready BEFORE UPDATE OF state ON session_preparation WHEN NEW.state \
         = 'ready' BEGIN SELECT RAISE(ABORT, 'preparation rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject preparation after checkout");

    for _attempt in 0..2 {
        // Act
        let result = app.launch_or_open_selected_follow_up_task(&source_id).await;

        // Assert
        assert!(
            result
                .expect_err("preparation failure")
                .to_string()
                .contains("preparation rejected")
        );
        let sessions = app
            .services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, source_id);
        let unlinked_preparations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_preparation WHERE session_id != ?")
                .bind(&source_id)
                .fetch_one(&pool)
                .await
                .expect("preparation count");
        assert_eq!(unlinked_preparations, 0);
        assert!(
            app.sessions
                .session_for_id(&source_id)
                .expect("source")
                .follow_up_tasks[0]
                .launched_session_id
                .is_none()
        );
        assert_eq!(
            session_creation_resources(directory.path()).await,
            original_resources
        );
    }

    // Act: readiness remains the success contract after the fault is removed.
    sqlx::query("DROP TRIGGER reject_ready")
        .execute(&pool)
        .await
        .expect("restore preparation");
    let retry_id = app.create_session().await.expect("retry creation");
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&retry_id)
        .await
        .expect("load")
        .expect("preparation");

    // Assert
    assert_eq!(preparation.state, db::SessionPreparationState::Ready);
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions")
            .len(),
        2
    );
}

#[tokio::test]
/// Verifies agent responses update cached follow-up tasks immediately for
/// the active session.
async fn apply_app_events_agent_response_updates_session_follow_up_tasks() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-follow-up-view"),
        ));

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            vec![
                "Document the new shortcut.",
                "Add a focused regression test.",
            ],
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0]
            .follow_up_tasks
            .iter()
            .map(|task| task.text.clone())
            .collect::<Vec<_>>(),
        vec![
            "Document the new shortcut.".to_string(),
            "Add a focused regression test.".to_string()
        ]
    );
}
