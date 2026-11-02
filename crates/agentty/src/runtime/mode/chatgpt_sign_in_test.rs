use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::presentation::chatgpt_sign_in::ChatGptSignInPhase;
use crate::runtime::EventResult;
use crate::test_support;

/// Builds an app showing the sign-in popup in `phase` above a draft.
async fn popup_app(phase: ChatGptSignInPhase) -> (App, tempfile::TempDir) {
    let (mut app, base_dir) = test_support::new_test_app().await;
    app.mode = test_support::chatgpt_sign_in_mode("session-1");
    if let AppMode::ChatGptSignIn { dialog } = &mut app.mode {
        dialog.phase = phase;
    }

    (app, base_dir)
}

/// Returns the visible sign-in phase, if the popup is open.
fn visible_phase(app: &App) -> Option<ChatGptSignInPhase> {
    match &app.mode {
        AppMode::ChatGptSignIn { dialog } => Some(dialog.phase.clone()),
        _ => None,
    }
}

/// Returns whether the composer with the draft is restored.
fn shows_restored_draft(app: &App) -> bool {
    matches!(&app.mode, AppMode::Prompt { input, .. } if input.text() == "draft")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[tokio::test]
async fn enter_on_confirmation_starts_the_sign_in() {
    // Arrange
    let (mut app, _base_dir) = popup_app(ChatGptSignInPhase::Confirm).await;

    // Act
    let result = handle(&mut app, key(KeyCode::Enter));

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(
        visible_phase(&app),
        Some(ChatGptSignInPhase::Waiting { url: None, .. })
    ));
}

#[tokio::test]
async fn esc_cancels_the_confirmation_and_a_waiting_sign_in() {
    // Arrange
    let (mut confirming, _confirm_dir) = popup_app(ChatGptSignInPhase::Confirm).await;
    let (mut waiting, _waiting_dir) = popup_app(ChatGptSignInPhase::Waiting {
        request_id: 1,
        url: None,
    })
    .await;

    // Act
    handle(&mut confirming, key(KeyCode::Esc));
    handle(&mut waiting, key(KeyCode::Esc));

    // Assert
    assert!(shows_restored_draft(&confirming));
    assert!(shows_restored_draft(&waiting));
}

#[tokio::test]
async fn enter_dismisses_a_finished_sign_in() {
    // Arrange
    let (mut app, _base_dir) = popup_app(ChatGptSignInPhase::Finished {
        message: "Signed in.".to_string(),
    })
    .await;

    // Act
    handle(&mut app, key(KeyCode::Enter));

    // Assert
    assert!(shows_restored_draft(&app));
}

#[tokio::test]
async fn other_keys_keep_the_popup_unchanged() {
    // Arrange
    let waiting_phase = ChatGptSignInPhase::Waiting {
        request_id: 1,
        url: None,
    };
    let (mut confirming, _confirm_dir) = popup_app(ChatGptSignInPhase::Confirm).await;
    let (mut waiting, _waiting_dir) = popup_app(waiting_phase.clone()).await;
    let (mut list_app, _list_dir) = test_support::new_test_app().await;

    // Act
    handle(&mut confirming, key(KeyCode::Char('q')));
    handle(&mut waiting, key(KeyCode::Enter));
    let list_result = handle(&mut list_app, key(KeyCode::Enter));

    // Assert
    assert_eq!(
        visible_phase(&confirming),
        Some(ChatGptSignInPhase::Confirm)
    );
    assert_eq!(visible_phase(&waiting), Some(waiting_phase));
    assert!(matches!(list_result, EventResult::Continue));
    assert!(matches!(list_app.mode, AppMode::List));
}
