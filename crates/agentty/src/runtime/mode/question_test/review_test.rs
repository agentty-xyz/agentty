use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::super::{mark_answered_session, question_scroll_metrics};
use super::support::{TEST_TERMINAL_SIZE, handle};
use crate::domain::agent::AgentModel;
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    Session, SessionHandles, SessionRole, SessionSize, SessionStats, Status,
};
use crate::domain::transient_message::TransientMessageStore;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::ui::RenderCacheStore;
use crate::ui::component::session_output::SessionOutputLineContext;
use crate::ui::page::session_chat::SessionChatPage;

#[tokio::test]
async fn test_handle_ctrl_c_sets_in_memory_session_status_to_review() {
    // Arrange — session exists in memory with Question status. Ctrl+C
    // should revert it to Review.

    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-review-check";
    app.sessions.push_session(Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::from("/tmp/test"),
        follow_up_tasks: Vec::new(),
        id: session_id.into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            crate::domain::agent::AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: String::new(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: crate::domain::agent::SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::Question,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    });
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: session_id.into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Q?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert — session status updated to Review in memory.
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    assert_eq!(session.status, Status::Review);
}

#[tokio::test]
async fn test_handle_ctrl_c_updates_session_handle_status_to_review() {
    // Arrange — session has a runtime handle with Question status.
    // Ctrl+C must update the handle so sync_from_handles does not revert
    // the snapshot status back to Question.

    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-handle-review";
    app.sessions.push_session(Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::from("/tmp/test"),
        follow_up_tasks: Vec::new(),
        id: session_id.into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            crate::domain::agent::AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: String::new(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: crate::domain::agent::SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::Question,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    });
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Question),
    );
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: session_id.into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Q?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert — handle status updated so sync_from_handles preserves Review.
    let handles = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("handle should exist");
    let handle_status = handles.status.lock().expect("lock should succeed");
    assert_eq!(*handle_status, Status::Review);
}

#[tokio::test]
async fn test_handle_ctrl_c_closes_open_in_progress_timer_before_review() {
    // Arrange — persisted state still has an open active-work interval
    // when question mode exits.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-timer-close";
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(session_id, "InProgress", 0)
        .await
        .expect("failed to open timing window");
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: session_id.into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Q?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;
    let sessions = app
        .services
        .db()
        .sessions()
        .load_sessions_for_project(project_id)
        .await
        .expect("failed to load sessions");

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session row");
    assert_eq!(session.status, "Review");
    assert_eq!(session.in_progress_started_at, None);
    assert!(session.in_progress_total_seconds > 0);
}

#[tokio::test]
async fn mark_orchestration_controller_review_clears_proxied_questions() {
    // Arrange

    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "campaign-controller";
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(session_id)
            .role(SessionRole::Orchestrator)
            .status(Status::Question)
            .questions(vec![QuestionItem::new("Choose one")])
            .build(),
    );
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Question),
    );

    // Act
    mark_answered_session(&mut app, session_id, true);

    // Assert
    let session = &app.sessions.sessions()[0];
    assert_eq!(session.status, Status::Review);
    assert_eq!(session.questions, [] as [ag_protocol::QuestionItem; 0]);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("handle should exist")
            .status
            .lock()
            .expect("status should remain available"),
        Status::Review
    );
}

#[tokio::test]
async fn test_question_scroll_metrics_uses_default_review_model_for_loading_fallback() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-review-model";
    app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Claude,
        AgentModel::ClaudeHaiku4520251001,
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(session_id)
            .model(AgentModel::Gpt56Sol)
            .status(Status::AgentReview)
            .build(),
    );
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Chat,
        input: InputState::default(),
        questions: vec![QuestionItem::new("Need a target branch?")],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: session_id.into(),
    };
    let terminal_size = Rect::new(0, 0, 16, 24);
    let output_width = terminal_size.width.saturating_sub(2);
    let render_cache_store = RenderCacheStore::default();
    // Act
    let metrics = question_scroll_metrics(&app, &render_cache_store, terminal_size)
        .expect("chat-focused question mode should have scroll metrics");

    let session = &app.sessions.sessions()[0];
    let expected = SessionChatPage::rendered_output_line_count(
        session,
        output_width,
        metrics.view_height,
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: app.session_update_version(session_id),
        },
        render_cache_store.markdown_render_cache(),
        render_cache_store.session_output_layout_cache(),
    );

    // Assert
    assert_eq!(
        metrics.total_lines, expected,
        "chat-focused question mode must report transcript line count"
    );
}
