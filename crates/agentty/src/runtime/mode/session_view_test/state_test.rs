use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{
    ViewActionState, ViewKeyContext, ViewPendingUpdate, ViewSessionSnapshot, handle_view_key,
    view_context,
};
use super::support::{new_test_app_with_session, session_fixture};
use crate::domain::session::Status;
use crate::presentation::app_mode::AppMode;
use crate::presentation::help_action;
use crate::presentation::help_action::ViewSessionState;

/// Verifies session-view action keys are ignored when the current session
/// status does not allow those actions.
#[tokio::test]
async fn test_handle_view_key_ignores_status_gated_actions() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");

    // Act
    for key in [
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
    ] {
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Disabled,
            inspect_diff: ViewActionState::Disabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Disabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
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
        let should_apply =
            handle_view_key(&mut app, key, view_key_context, &mut pending_update).await;

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
}

#[test]
fn test_view_session_state_maps_rebasing_status() {
    // Arrange
    let status = Status::Rebasing;
    let session = session_fixture(status, false);

    // Act
    let state = help_action::session_view_state(&session);

    // Assert
    assert_eq!(state, ViewSessionState::Rebasing);
}

#[test]
fn test_view_session_state_maps_canceled_status() {
    // Arrange
    let status = Status::Canceled;
    let session = session_fixture(status, false);

    // Act
    let state = help_action::session_view_state(&session);

    // Assert
    assert_eq!(state, ViewSessionState::Canceled);
}
