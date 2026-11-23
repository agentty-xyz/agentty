//! API key dialog content tests for editing, failures, and the saved step.

use super::{ApiKeyDialog, ApiKeyPhase};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::input::InputCommand;
use crate::domain::session::SessionId;
use crate::test_support;

/// Builds a Kimi dialog in `phase`.
fn dialog(phase: ApiKeyPhase) -> ApiKeyDialog {
    let mut dialog = ApiKeyDialog::new(
        AgentSelection::new(AgentKind::Harness, AgentModel::KimiK3),
        "kimi",
        "Kimi".to_string(),
        test_support::prompt_mode_snapshot("session-1", "draft"),
    );
    dialog.phase = phase;

    dialog
}

#[test]
fn editing_explains_where_the_key_is_saved() {
    // Arrange
    let dialog = dialog(ApiKeyPhase::Editing { error: None });

    // Act
    let message = dialog.message();

    // Assert
    assert_eq!(
        message,
        "`kimi-k3` needs a Kimi API key. Agentty saves it to `auth/api_keys.json` in its data \
         directory (`~/.agentty` by default), readable only by you."
    );
    assert!(dialog.is_editing());
    assert_eq!(dialog.provider, "kimi");
    assert_eq!(dialog.session_id(), &SessionId::from("session-1"));
}

#[test]
fn a_failed_save_leads_the_message() {
    // Arrange
    let dialog = dialog(ApiKeyPhase::Editing {
        error: Some("Paste a nonblank API key.".to_string()),
    });

    // Act
    let message = dialog.message();

    // Assert
    assert!(
        message.starts_with("Paste a nonblank API key.\n\n`kimi-k3` needs a Kimi API key."),
        "{message}"
    );
}

#[test]
fn the_saved_step_shows_its_outcome() {
    // Arrange
    let dialog = dialog(ApiKeyPhase::Saved {
        message: "Saved your Kimi API key.".to_string(),
    });

    // Act
    let message = dialog.message();

    // Assert
    assert_eq!(message, "Saved your Kimi API key.");
    assert!(!dialog.is_editing());
}

#[test]
fn the_pasted_key_is_masked_character_for_character() {
    // Arrange
    let mut dialog = dialog(ApiKeyPhase::Editing { error: None });
    dialog
        .input
        .apply(InputCommand::InsertText("sk-ключ".to_string()));

    // Act
    let masked = dialog.masked_input();

    // Assert
    assert_eq!(masked, "•••••••");
    assert_eq!(dialog.input.cursor, masked.chars().count());
}
