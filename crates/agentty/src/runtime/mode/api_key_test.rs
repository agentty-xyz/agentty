//! API key popup key and paste handling tests.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{handle, handle_paste};
use crate::app::App;
use crate::presentation::api_key::ApiKeyPhase;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;
use crate::test_support;

/// Builds an app showing the API key popup in `phase` above a draft.
async fn popup_app(phase: ApiKeyPhase) -> (App, tempfile::TempDir) {
    let (mut app, base_dir) = test_support::new_test_app().await;
    app.mode = test_support::api_key_input_mode("session-1");
    if let AppMode::ApiKeyInput { dialog } = &mut app.mode {
        dialog.phase = phase;
    }

    (app, base_dir)
}

/// Returns the popup's phase and pasted key, if it is open.
fn visible_popup(app: &App) -> Option<(ApiKeyPhase, String)> {
    match &app.mode {
        AppMode::ApiKeyInput { dialog } => {
            Some((dialog.phase.clone(), dialog.input.text().to_string()))
        }
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

fn editing() -> ApiKeyPhase {
    ApiKeyPhase::Editing { error: None }
}

fn saved() -> ApiKeyPhase {
    ApiKeyPhase::Saved {
        message: "Saved.".to_string(),
    }
}

#[tokio::test]
async fn typing_and_pasting_edit_one_line_of_the_key() {
    // Arrange
    let (mut app, _base_dir) = popup_app(editing()).await;

    // Act
    handle(&mut app, key(KeyCode::Char('s'))).await;
    handle_paste(&mut app, "k-one\ntwo");
    handle(&mut app, key(KeyCode::Backspace)).await;
    let result = handle(&mut app, key(KeyCode::Tab)).await;

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(visible_popup(&app), Some((editing(), "sk-on".to_string())));
}

#[tokio::test]
async fn enter_saves_the_key_and_shows_a_failure() {
    // Arrange
    let (mut app, _base_dir) = popup_app(editing()).await;
    handle_paste(&mut app, "sk-key");

    // Act
    handle(&mut app, key(KeyCode::Enter)).await;

    // Assert
    assert_eq!(
        visible_popup(&app),
        Some((
            ApiKeyPhase::Editing {
                error: Some("API keys cannot be saved in this launch.".to_string()),
            },
            "sk-key".to_string()
        ))
    );
}

#[tokio::test]
async fn esc_cancels_and_enter_dismisses_a_saved_key() {
    // Arrange
    let (mut editing_app, _editing_dir) = popup_app(editing()).await;
    let (mut saved_app, _saved_dir) = popup_app(saved()).await;

    // Act
    handle(&mut editing_app, key(KeyCode::Esc)).await;
    handle(&mut saved_app, key(KeyCode::Enter)).await;

    // Assert
    assert!(shows_restored_draft(&editing_app));
    assert!(shows_restored_draft(&saved_app));
}

#[tokio::test]
async fn a_saved_key_ignores_typing_and_pasting() {
    // Arrange
    let (mut app, _base_dir) = popup_app(saved()).await;

    // Act
    handle(&mut app, key(KeyCode::Char('x'))).await;
    handle_paste(&mut app, "pasted");

    // Assert
    assert_eq!(visible_popup(&app), Some((saved(), String::new())));
}

#[tokio::test]
async fn other_modes_ignore_keys_and_pastes() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.mode = AppMode::List;

    // Act
    let result = handle(&mut app, key(KeyCode::Enter)).await;
    handle_paste(&mut app, "pasted");

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}
