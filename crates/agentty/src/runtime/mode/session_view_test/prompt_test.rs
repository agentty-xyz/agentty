use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{
    ViewActionState, ViewKeyContext, ViewPendingUpdate, handle_view_key,
    session_prompt_history_entries, switch_view_to_prompt, view_context,
};
use super::support::{new_test_app_with_session, reply_enabled_review_snapshot};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::presentation::app_mode::{AppMode, ChatFocus, PromptModeSnapshot};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};

#[test]
fn test_session_prompt_history_entries_excludes_generated_agent_prompts() {
    // Arrange
    let mut session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Review)
        .build();
    session.transcript = Some(SessionTranscript::new(vec![
        SessionMessage::conversation(
            0,
            SessionMessageKind::UserPrompt,
            "first line\n\nsecond line",
        ),
        SessionMessage::conversation(
            1,
            SessionMessageKind::AgentPrompt,
            "Process the selected review comments",
        ),
        SessionMessage::conversation(
            2,
            SessionMessageKind::AssistantAnswer,
            "Resolved the review comments",
        ),
        SessionMessage::conversation(3, SessionMessageKind::UserPrompt, "latest prompt"),
    ]));

    // Act
    let entries = session_prompt_history_entries(&session);

    // Assert
    assert_eq!(
        entries,
        vec![
            "first line\n\nsecond line".to_string(),
            "latest prompt".to_string()
        ]
    );
}

#[tokio::test]
async fn test_handle_view_key_enter_opens_empty_prompt_composer() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = reply_enabled_review_snapshot();
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        view_key_context,
        &mut pending_update,
    )
    .await;

    // Assert
    assert!(should_apply);
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            scroll_offset: Some(2),
            ..
        } if input.is_empty() && session_id == &view_context.session_id
    ));
}

#[tokio::test]
async fn test_handle_view_key_slash_opens_for_reply_enabled_stacked_parent() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let mut view_session_snapshot = reply_enabled_review_snapshot();
    view_session_snapshot.mutate_session_branch = ViewActionState::Disabled;
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        view_key_context,
        &mut pending_update,
    )
    .await;

    // Assert
    assert!(should_apply);
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            scroll_offset: Some(2),
            ..
        } if input.text() == "/"
            && input.cursor == 1
            && session_id == &view_context.session_id
    ));
}

#[tokio::test]
async fn test_handle_view_key_slash_stays_closed_when_mutation_and_reply_are_blocked() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let mut view_session_snapshot = reply_enabled_review_snapshot();
    view_session_snapshot.mutate_session_branch = ViewActionState::Disabled;
    view_session_snapshot.reply_to_session = ViewActionState::Disabled;
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
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
    assert_eq!(pending_update.scroll_offset, Some(2));
}

#[tokio::test]
async fn test_slash_prompt_replacement_prevents_saved_prompt_restoration() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    app.save_prompt_progress(PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::with_text("saved reply".to_string()),
        scroll_offset: Some(4),
        session_id: session_id.into(),
        slash_state: PromptSlashState::default(),
    });
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot = reply_enabled_review_snapshot();
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    // Act
    let should_apply = handle_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        view_key_context,
        &mut pending_update,
    )
    .await;
    let opened_prefilled_slash = matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            scroll_offset: Some(2),
            ..
        } if input.text() == "/"
            && input.cursor == 1
            && session_id == &view_context.session_id
    );
    app.mode = AppMode::View {
        session_id: view_context.session_id.clone(),
        scroll_offset: Some(2),
    };
    switch_view_to_prompt(
        &mut app,
        &view_context,
        PromptHistoryState::new(Vec::new()),
        InputState::default(),
        Some(2),
    )
    .await;

    // Assert
    assert!(should_apply);
    assert!(opened_prefilled_slash);
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            scroll_offset: Some(2),
            ..
        } if input.is_empty()
            && session_id == &view_context.session_id
    ));
    assert!(app.prompt_progress.is_empty());
}

#[tokio::test]
async fn test_switch_view_to_prompt_restores_saved_progress() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    app.save_prompt_progress(PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(vec!["previous".to_string()]),
        input: InputState::with_text("saved reply".to_string()),
        scroll_offset: Some(4),
        session_id: session_id.into(),
        slash_state: PromptSlashState::default(),
    });

    // Act
    switch_view_to_prompt(
        &mut app,
        &view_context,
        PromptHistoryState::new(Vec::new()),
        InputState::default(),
        Some(2),
    )
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            focus: ChatFocus::Input,
            history_state,
            input,
            scroll_offset: Some(4),
            ..
        } if history_state.entries == vec!["previous".to_string()]
            && input.text() == "saved reply"
    ));
    assert!(app.prompt_progress.is_empty());
}
