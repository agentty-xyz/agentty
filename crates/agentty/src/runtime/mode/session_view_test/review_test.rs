use ag_session::test_support as model_fixture;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{
    ViewActionState, ViewContext, ViewKeyContext, ViewPendingUpdate, ViewSessionSnapshot,
    end_in_progress_turn, handle_primary_view_key, handle_view_key, handle_workflow_view_key,
    open_or_regenerate_review, open_publish_branch_input, open_review_comments_in_diff,
    open_review_output_mode, view_context, view_session_snapshot,
};
use super::support::{
    apply_pending_session_diff, attach_open_review_request, new_test_app_with_session,
    reply_enabled_review_snapshot, session_fixture, session_replay_text,
};
use crate::app::ReviewCacheEntry;
use crate::app::test_support::{REVIEW_NO_DIFF_MESSAGE, diff_content_hash, review_loading_message};
use crate::domain::agent::AgentModel;
use crate::domain::session::{FollowUpTaskAction, PublishBranchAction, Status};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DiffSidebarFocus,
};
use crate::presentation::help_action::ViewSessionState;
use crate::runtime::mode::session_output_metric;
use crate::test_support::{SessionFixtureBuilder, session_manager_with_sessions};
use crate::ui::RenderCacheStore;
use crate::ui::component::session_output::SessionOutputLineContext;
use crate::ui::page::session_chat::SessionChatPage;

#[tokio::test]
async fn test_handle_view_key_ignores_diff_for_non_review_status() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = ViewSessionSnapshot {
        continue_terminal_session: ViewActionState::Disabled,
        fork_session: ViewActionState::Disabled,
        inspect_diff: ViewActionState::Disabled,
        merge_session_branch: ViewActionState::Enabled,
        mutate_session_branch: ViewActionState::Enabled,
        rebase_session_branch: ViewActionState::Enabled,
        open_worktree: ViewActionState::Disabled,
        reply_to_session: ViewActionState::Enabled,
        review_comments: ViewActionState::Disabled,
        review_watch_toggle: None,
        start_staged_session: ViewActionState::Disabled,
        follow_up_task_action: None,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Done,
        session_status: Status::Done,
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        view_key_context,
        &mut pending_update,
    )
    .await;

    // Assert
    assert!(should_apply);
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
        scroll_offset: Some(2),
            ..
        } if session_id == &view_context.session_id
    ));
    assert_eq!(pending_update.scroll_offset, Some(2));
}

#[tokio::test]
async fn test_handle_view_key_uppercase_f_does_not_start_review_when_fork_unavailable() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::Review;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = ViewSessionSnapshot {
        continue_terminal_session: ViewActionState::Disabled,
        fork_session: ViewActionState::Disabled,
        inspect_diff: ViewActionState::Enabled,
        merge_session_branch: ViewActionState::Enabled,
        mutate_session_branch: ViewActionState::Enabled,
        rebase_session_branch: ViewActionState::Enabled,
        open_worktree: ViewActionState::Enabled,
        reply_to_session: ViewActionState::Enabled,
        review_comments: ViewActionState::Disabled,
        review_watch_toggle: None,
        start_staged_session: ViewActionState::Disabled,
        follow_up_task_action: None,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
        session_status: Status::Review,
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE),
        view_key_context,
        &mut pending_update,
    )
    .await;

    // Assert
    assert!(should_apply);
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(2),
        } if session_id == &view_context.session_id
    ));
    assert!(!app.review_cache.contains_key(session_id.as_str()));
}

#[tokio::test]
async fn test_handle_view_key_p_opens_review_request_publish_input() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = ViewSessionSnapshot {
        continue_terminal_session: ViewActionState::Disabled,
        fork_session: ViewActionState::Enabled,
        inspect_diff: ViewActionState::Enabled,
        merge_session_branch: ViewActionState::Enabled,
        mutate_session_branch: ViewActionState::Enabled,
        rebase_session_branch: ViewActionState::Enabled,
        open_worktree: ViewActionState::Enabled,
        reply_to_session: ViewActionState::Enabled,
        review_comments: ViewActionState::Disabled,
        review_watch_toggle: None,
        start_staged_session: ViewActionState::Disabled,
        follow_up_task_action: None,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
        session_status: Status::Review,
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        view_key_context,
        &mut pending_update,
    )
    .await;

    // Assert
    assert!(!should_apply);
    assert!(matches!(
        app.mode,
        AppMode::PublishBranchInput {
            publish_branch_action: PublishBranchAction::PublishPullRequest,
            ref restore_view,
            ..
        } if restore_view.session_id == session_id
    ));
}

#[tokio::test]
async fn test_handle_view_key_shift_p_opens_review_request_publish_input() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = ViewSessionSnapshot {
        continue_terminal_session: ViewActionState::Disabled,
        fork_session: ViewActionState::Enabled,
        inspect_diff: ViewActionState::Enabled,
        merge_session_branch: ViewActionState::Enabled,
        mutate_session_branch: ViewActionState::Enabled,
        rebase_session_branch: ViewActionState::Enabled,
        open_worktree: ViewActionState::Enabled,
        reply_to_session: ViewActionState::Enabled,
        review_comments: ViewActionState::Disabled,
        review_watch_toggle: None,
        start_staged_session: ViewActionState::Disabled,
        follow_up_task_action: None,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
        session_status: Status::Review,
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
        view_key_context,
        &mut pending_update,
    )
    .await;

    // Assert
    assert!(!should_apply);
    assert!(matches!(
        app.mode,
        AppMode::PublishBranchInput {
            publish_branch_action: PublishBranchAction::PublishPullRequest,
            ref restore_view,
            ..
        } if restore_view.session_id == session_id
    ));
}

#[tokio::test]
async fn test_view_total_lines_uses_default_review_model_for_loading_fallback() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Claude,
        AgentModel::ClaudeHaiku55,
    );
    app.sessions.sessions_mut()[0].agent = model_fixture::codex_selection();
    app.sessions.sessions_mut()[0].status = Status::AgentReview;
    let output_width = 14;
    let viewport_height = 5;
    let render_cache_store = RenderCacheStore::default();
    let session = &app.sessions.sessions()[0];
    let expected = SessionChatPage::rendered_output_line_count(
        session,
        output_width,
        viewport_height,
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: app.session_update_version(&session_id),
        },
        render_cache_store.markdown_render_cache(),
        render_cache_store.session_output_layout_cache(),
    );

    // Act
    let total_lines = session_output_metric::tests::rendered_output_line_count(
        &app,
        &session_id,
        0,
        output_width,
        viewport_height,
    );

    // Assert
    assert_eq!(total_lines, expected);
}

#[test]
fn test_open_publish_branch_input_preserves_view_context() {
    // Arrange
    let session = session_fixture(Status::Draft, false);
    let session_id = session.id.to_string();
    let sessions = session_manager_with_sessions(vec![session]);
    let mut mode = AppMode::List;
    let view_context = ViewContext {
        scroll_offset: Some(5),
        session_id: session_id.clone().into(),
        session_index: 0,
    };

    // Act
    open_publish_branch_input(
        &mut mode,
        &sessions,
        &view_context,
        PublishBranchAction::PublishPullRequest,
    );

    // Assert
    assert!(matches!(
        mode,
        AppMode::PublishBranchInput {
            ref default_branch_name,
            input: ref input_state,
            locked_upstream_ref: None,
            publish_branch_action: PublishBranchAction::PublishPullRequest,
            restore_view:
                ConfirmationViewMode {
                    session_id: ref restored_session_id,
        scroll_offset: Some(5),
                },
        } if default_branch_name == &crate::app::session::session_branch(&session_id)
            && input_state.cursor == 0
            && input_state.text().is_empty()
            && restored_session_id == &session_id
    ));
}

#[test]
fn test_open_publish_branch_input_locks_existing_upstream_branch_name() {
    // Arrange
    let session = session_fixture(Status::Draft, false);
    let session_id = session.id.to_string();
    let mut sessions = session_manager_with_sessions(vec![session]);
    let mut mode = AppMode::List;
    sessions.sessions_mut()[0].published_upstream_ref = Some("origin/review/custom".to_string());
    let view_context = ViewContext {
        scroll_offset: Some(1),
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    open_publish_branch_input(
        &mut mode,
        &sessions,
        &view_context,
        PublishBranchAction::PublishPullRequest,
    );

    // Assert
    assert!(matches!(
        mode,
        AppMode::PublishBranchInput {
            input: ref input_state,
            locked_upstream_ref: Some(ref upstream_ref),
            ..
        } if upstream_ref == "origin/review/custom"
            && input_state.text() == "review/custom"
    ));
}

#[test]
fn test_view_session_snapshot_blocks_stacked_draft_start_until_parent_review() {
    // Arrange
    let parent = SessionFixtureBuilder::new()
        .id("parent")
        .status(Status::InProgress)
        .draft(false)
        .build();
    let mut child = SessionFixtureBuilder::new()
        .id("child")
        .status(Status::Draft)
        .draft(true)
        .parent_session_id(Some("parent".into()))
        .build();
    child.prompt = "staged child draft".to_string();
    let sessions = session_manager_with_sessions(vec![parent, child]);
    let mut mode = AppMode::View {
        session_id: "child".into(),
        scroll_offset: Some(1),
    };
    let context = ViewContext::from_mode(&mut mode, &sessions).expect("view context");

    // Act
    let snapshot = ViewSessionSnapshot::from_sessions(&sessions, &context, true)
        .expect("expected view snapshot");

    // Assert
    assert_eq!(snapshot.session_state, ViewSessionState::StackedDraft);
    assert!(!snapshot.can_start_staged_session());
    assert!(snapshot.can_open_prompt_composer());
}

#[test]
fn test_view_session_snapshot_keeps_parent_reply_with_review_child() {
    // Arrange
    let parent = SessionFixtureBuilder::new()
        .id("parent")
        .status(Status::Review)
        .draft(false)
        .build();
    let mut child = SessionFixtureBuilder::new()
        .id("child")
        .status(Status::Review)
        .draft(true)
        .parent_session_id(Some("parent".into()))
        .build();
    child.prompt = "staged child draft".to_string();
    let sessions = session_manager_with_sessions(vec![parent, child]);
    let mut mode = AppMode::View {
        session_id: "parent".into(),
        scroll_offset: Some(1),
    };
    let context = ViewContext::from_mode(&mut mode, &sessions).expect("view context");

    // Act
    let snapshot = ViewSessionSnapshot::from_sessions(&sessions, &context, true)
        .expect("expected view snapshot");

    // Assert
    assert_eq!(snapshot.session_state, ViewSessionState::Review);
    assert!(snapshot.can_open_prompt_composer());
    assert!(snapshot.can_merge_session());
    assert!(snapshot.can_rebase_session());
}

#[tokio::test]
async fn workflow_diff_and_review_keys_start_background_loads() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        scroll_offset: Some(2),
        session_id: session_id.clone().into(),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let view_session_snapshot = reply_enabled_review_snapshot();
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    // Act
    let diff_result = handle_workflow_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &mut pending_update,
    )
    .await;
    let diff_is_loading = matches!(app.mode, AppMode::DiffLoading { .. });
    app.cancel_diff_view_load();
    let review_result = handle_workflow_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &mut pending_update,
    )
    .await;

    // Assert
    assert_eq!(diff_result, Some(true));
    assert!(diff_is_loading);
    assert_eq!(review_result, Some(true));
    assert!(matches!(
        app.review_cache.get(&view_context.session_id),
        Some(ReviewCacheEntry::Loading { .. })
    ));
}

#[tokio::test]
async fn test_open_or_regenerate_review_opens_when_review_output_is_missing() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let view_context = ViewContext {
        scroll_offset: Some(5),
        session_id: session_id.into(),
        session_index: 0,
    };
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    // Act
    open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

    // Assert
    assert_eq!(pending_update.scroll_offset, None);
}

#[tokio::test]
async fn test_open_or_regenerate_shows_confirmation_when_review_output_exists() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Ready {
            request_id: uuid::Uuid::nil(),
            text: "Old review".to_string(),
            diff_hash: 123,
        },
    );
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.clone().into(),
        session_index: 0,
    };
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    // Act
    open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

    // Assert — confirmation popup is shown instead of direct regeneration
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            ..
        }
    ));
    // Cache is preserved until user confirms
    assert!(app.review_cache.contains_key(session_id.as_str()));
}

#[tokio::test]
async fn stale_view_context_cannot_open_review_comments() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: usize::MAX,
    };

    // Act
    open_review_comments_in_diff(&mut app, &view_context);

    // Assert
    assert!(!matches!(app.mode, AppMode::DiffLoading { .. }));
}

#[tokio::test]
async fn test_open_review_output_mode_leaves_existing_cache_unchanged() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Ready {
            request_id: uuid::Uuid::nil(),
            diff_hash: 123,
            text: "Cached review".to_string(),
        },
    );
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    open_review_output_mode(&mut app, &view_context);

    // Assert
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    assert_eq!(review_status_message, None);
    assert_eq!(review_text, Some("Cached review"));
}

#[tokio::test]
async fn test_open_review_output_mode_starts_loading_when_diff_exists() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Claude,
        AgentModel::ClaudeOpus55,
    );
    app.sessions.sessions_mut()[0].status = Status::Review;
    let session_folder = app.sessions.sessions()[0].folder.clone();
    std::fs::write(session_folder.join("README.md"), "review test content\n")
        .expect("failed to update readme");
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    open_review_output_mode(&mut app, &view_context);

    // Assert
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    assert_eq!(
        review_status_message,
        Some(review_loading_message(app.review_agent()))
    );
    assert_eq!(review_text, None);
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert!(matches!(
        app.review_cache.get(&view_context.session_id),
        Some(ReviewCacheEntry::Loading { .. })
    ));
}

#[tokio::test]
async fn test_open_review_output_mode_shows_no_diff_message_when_diff_empty() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::Review;
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    open_review_output_mode(&mut app, &view_context);
    apply_pending_session_diff(&mut app, &view_context.session_id, Ok("")).await;

    // Assert
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    assert_eq!(review_status_message, None);
    assert_eq!(review_text, Some(REVIEW_NO_DIFF_MESSAGE));
    assert!(matches!(
        app.review_cache.get(&view_context.session_id),
        Some(ReviewCacheEntry::Ready {
            diff_hash,
            text,
         .. }) if *diff_hash == diff_content_hash("") && text == REVIEW_NO_DIFF_MESSAGE
    ));
}

#[tokio::test]
async fn test_open_review_output_mode_ignores_stale_session_selection() {
    // Arrange
    let (app, _base_dir, session_id) = new_test_app_with_session().await;
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: 99,
    };
    let mut app = app;

    // Act
    open_review_output_mode(&mut app, &view_context);

    // Assert
    assert!(!app.review_cache.contains_key(&view_context.session_id));
}

#[tokio::test]
async fn test_open_review_output_mode_uses_ready_cache_entry() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let cached_text = "## Review\nCached review from auto-generation.";
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Ready {
            request_id: uuid::Uuid::nil(),
            diff_hash: 123,
            text: cached_text.to_string(),
        },
    );
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    open_review_output_mode(&mut app, &view_context);

    // Assert
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    assert_eq!(review_status_message, None);
    assert_eq!(review_text, Some(cached_text));
}

#[tokio::test]
async fn test_open_review_output_mode_shows_loading_for_cache_loading_entry() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Claude,
        AgentModel::ClaudeOpus55,
    );
    let review_agent = app.review_agent();
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Loading {
            request_id: uuid::Uuid::nil(),
            progress: None,
            diff_hash: 456,
            review_agent,
        },
    );
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    open_review_output_mode(&mut app, &view_context);

    // Assert
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    assert_eq!(
        review_status_message,
        Some(review_loading_message(review_agent))
    );
    assert_eq!(review_text, None);
}

#[tokio::test]
async fn focused_review_key_reveals_running_review_after_sync() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::Review;
    let request_id = uuid::Uuid::new_v4();
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Loading {
            request_id,
            progress: None,
            diff_hash: 456,
            review_agent: app.review_agent(),
        },
    );
    let view_context = ViewContext {
        scroll_offset: Some(2),
        session_id: session_id.into(),
        session_index: 0,
    };
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    // Act
    open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

    // Assert
    assert_eq!(pending_update.scroll_offset, None);
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert_eq!(
        app.review_cache
            .get(&view_context.session_id)
            .and_then(ReviewCacheEntry::request_id),
        Some(request_id)
    );
    assert_eq!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::Review)
            .expect("running review progress")
            .body
            .text(),
        review_loading_message(app.review_agent())
    );
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!matches!(app.mode, AppMode::Confirmation { .. }));
}

#[tokio::test]
async fn test_end_in_progress_turn_transitions_session_to_review() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    app.sessions.session_handles_mut().insert(
        session_id.clone().into(),
        crate::domain::session::SessionHandles::new(Status::InProgress),
    );

    // Act
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    let handle_status = *app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles missing")
        .status
        .lock()
        .expect("lock failed");
    assert_eq!(handle_status, Status::Review);
}

#[tokio::test]
async fn test_end_in_progress_turn_keeps_review_session_review_ready() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::Review;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::Review.to_string(), 0)
        .await;
    app.sessions.session_handles_mut().insert(
        session_id.clone().into(),
        crate::domain::session::SessionHandles::new(Status::Review),
    );

    // Act
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    let handle_status = *app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles missing")
        .status
        .lock()
        .expect("lock failed");
    assert_eq!(handle_status, Status::Review);
}

#[tokio::test]
async fn review_comment_key_opens_diff_from_view() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    attach_open_review_request(session);
    session.status = Status::Review;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot =
        view_session_snapshot(&app, &view_context).expect("expected session snapshot");

    // Act
    let result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(result, Some(false));
    assert!(matches!(
        app.mode,
        AppMode::DiffLoading {
            ref session_id,
            sidebar_focus: DiffSidebarFocus::Comments,
            ..
        } if session_id == &view_context.session_id
    ));
}

/// Sends one plain workflow key against a fresh review-ready view.
async fn press_review_workflow_key(
    app: &mut crate::app::App,
    session_id: &str,
    character: char,
) -> Option<bool> {
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    handle_workflow_view_key(
        app,
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
        &view_context,
        &reply_enabled_review_snapshot(),
        &mut pending_update,
    )
    .await
}

#[tokio::test]
async fn workflow_keys_confirm_fork_and_merge_and_start_rebase() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);

    // Act
    let fork_result = press_review_workflow_key(&mut app, &session_id, 'F').await;
    let fork_mode_is_confirmation = matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ForkSession,
            ref confirmation_title,
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(2),
                ..
            }),
            ..
        } if confirmation_title == "Confirm Fork"
    );
    let merge_result = press_review_workflow_key(&mut app, &session_id, 'm').await;
    let merge_mode_is_confirmation = matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            ..
        }
    );
    let rebase_result = press_review_workflow_key(&mut app, &session_id, 'r').await;

    // Assert
    assert_eq!(fork_result, Some(false));
    assert!(fork_mode_is_confirmation);
    assert_eq!(merge_result, Some(true));
    assert!(merge_mode_is_confirmation);
    assert_eq!(rebase_result, Some(true));
    let handle_status = *app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("session handle")
        .status
        .lock()
        .expect("status lock");
    assert_eq!(handle_status, Status::Rebasing);
}

#[tokio::test]
async fn rejected_follow_up_and_staged_start_keys_report_transcript_errors() {
    for (character, snapshot_update, expected_notice) in [
        (
            'l',
            (|snapshot: &mut ViewSessionSnapshot| {
                snapshot.follow_up_task_action = Some(FollowUpTaskAction::Launch);
            }) as fn(&mut ViewSessionSnapshot),
            "[Follow-Up Task Error]",
        ),
        (
            's',
            |snapshot: &mut ViewSessionSnapshot| {
                snapshot.start_staged_session = ViewActionState::Enabled;
            },
            "[Start Error]",
        ),
    ] {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Merged);
        app.sessions.sessions_mut()[0].follow_up_tasks =
            vec![crate::domain::session::SessionFollowUpTask {
                id: 1,
                launched_session_id: None,
                position: 0,
                text: "Write the follow-up.".to_string(),
            }];
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: None,
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let mut view_session_snapshot = reply_enabled_review_snapshot();
        snapshot_update(&mut view_session_snapshot);

        // Act
        let result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &view_context,
            &view_session_snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(result, Some(false));
        app.sessions.sync_from_handles();
        assert!(
            session_replay_text(&app.sessions.sessions()[0]).contains(expected_notice),
            "missing {expected_notice}"
        );
    }
}

#[tokio::test]
async fn bracket_keys_cycle_between_follow_up_tasks() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].follow_up_tasks = vec![
        crate::domain::session::SessionFollowUpTask {
            id: 1,
            launched_session_id: None,
            position: 0,
            text: "First follow-up".to_string(),
        },
        crate::domain::session::SessionFollowUpTask {
            id: 2,
            launched_session_id: None,
            position: 1,
            text: "Second follow-up".to_string(),
        },
    ];
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: None,
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = reply_enabled_review_snapshot();
    let initial_task = app.sessions.selected_follow_up_task_position(&session_id);

    // Act
    let next_result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &pending_update,
    )
    .await;
    let next_task = app.sessions.selected_follow_up_task_position(&session_id);
    let previous_result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(next_result, Some(true));
    assert_eq!(previous_result, Some(true));
    assert_ne!(next_task, initial_task);
    assert_eq!(
        app.sessions.selected_follow_up_task_position(&session_id),
        initial_task
    );
}

#[test]
fn test_open_publish_branch_input_ignores_an_unloaded_session_index() {
    // Arrange
    let sessions = session_manager_with_sessions(Vec::new());
    let mut mode = AppMode::List;
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: "missing-session".into(),
        session_index: 0,
    };

    // Act
    open_publish_branch_input(
        &mut mode,
        &sessions,
        &view_context,
        PublishBranchAction::PublishPullRequest,
    );

    // Assert
    assert!(matches!(mode, AppMode::List));
}

#[tokio::test]
async fn interrupting_an_unloaded_session_skips_missing_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    end_in_progress_turn(&mut app, "missing-session").await;

    // Assert
    assert!(app.sessions.sessions().is_empty());
    assert!(app.sessions.session_handles().is_empty());
}

#[tokio::test]
async fn interrupting_a_turn_survives_persistence_and_lock_failures() {
    // Arrange
    crate::test_support::enable_tracing_log_fallback();
    let (mut app, _base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    sqlx::query("DROP TABLE session_operation")
        .execute(&pool)
        .await
        .expect("operation table should drop");
    sqlx::query(
        "CREATE TRIGGER fail_status_update BEFORE UPDATE OF status ON session BEGIN SELECT \
         RAISE(FAIL, 'forced status failure'); END",
    )
    .execute(&pool)
    .await
    .expect("failure trigger should be installed");
    let cancel_token = std::sync::Arc::clone(
        &app.sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("session handle")
            .cancel_token,
    );
    crate::test_support::poison_lock(cancel_token);

    // Act
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
}

/// Presses `W` in the open view of `session_id` using its live snapshot.
async fn press_watch_key(app: &mut crate::app::App, session_id: &str) {
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot =
        view_session_snapshot(app, &view_context).expect("expected session snapshot");
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    handle_view_key(
        app,
        KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT),
        view_key_context,
        &mut pending_update,
    )
    .await;
}

#[tokio::test]
async fn watch_key_offers_watching_then_stops_it() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);

    // Act
    press_watch_key(&mut app, &session_id).await;
    let ignored_without_review_request = matches!(app.mode, AppMode::View { .. });
    attach_open_review_request(&mut app.sessions.sessions_mut()[0]);
    press_watch_key(&mut app, &session_id).await;
    let offered_mode = std::mem::replace(&mut app.mode, AppMode::List);
    app.enable_review_watch(&session_id.clone().into()).await;
    let watching_status = app.sessions.sessions()[0].status;
    press_watch_key(&mut app, &session_id).await;

    // Assert
    assert!(ignored_without_review_request);
    assert!(matches!(
        offered_mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::WatchReviewRequest,
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(2),
                ..
            }),
            ..
        }
    ));
    assert_eq!(watching_status, Status::Watching);
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert!(matches!(app.mode, AppMode::View { .. }));
}
