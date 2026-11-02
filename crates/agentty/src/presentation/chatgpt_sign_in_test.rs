use super::{ChatGptSignInDialog, ChatGptSignInPhase};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::SessionId;
use crate::test_support;

/// Builds a dialog for the `ChatGPT` model in `phase`.
fn dialog(phase: ChatGptSignInPhase) -> ChatGptSignInDialog {
    let mut dialog = ChatGptSignInDialog::new(
        AgentSelection::new(AgentKind::Harness, AgentModel::ChatGptGpt6Sol),
        test_support::prompt_mode_snapshot("session-1", "draft"),
    );
    dialog.phase = phase;

    dialog
}

#[test]
fn confirmation_explains_the_sign_in_and_offers_enter_or_esc() {
    // Arrange
    let dialog = dialog(ChatGptSignInPhase::Confirm);

    // Act
    let message = dialog.message();

    // Assert
    assert_eq!(
        message,
        "Use your ChatGPT plan for `chatgpt/gpt-6-sol`. Agentty opens your browser to sign in."
    );
    assert_eq!(dialog.action_label(), "Enter: Sign in · Esc: Cancel");
    assert!(!dialog.is_loading());
    assert_eq!(dialog.waiting_request_id(), None);
    assert_eq!(dialog.session_id(), &SessionId::from("session-1"));
}

#[test]
fn waiting_shows_the_url_once_ready_and_how_to_cancel() {
    // Arrange
    let starting = dialog(ChatGptSignInPhase::Waiting {
        request_id: 3,
        url: None,
    });
    let ready = dialog(ChatGptSignInPhase::Waiting {
        request_id: 3,
        url: Some("https://auth.example.test/authorize".to_string()),
    });

    // Act
    let starting_message = starting.message();
    let ready_message = ready.message();

    // Assert
    assert_eq!(
        starting_message,
        "Starting the sign-in…\n\nPress `Esc` to cancel."
    );
    assert!(ready_message.contains("`https://auth.example.test/authorize`"));
    assert!(ready_message.ends_with("Press `Esc` to cancel."));
    assert!(ready.is_loading());
    assert_eq!(ready.waiting_request_id(), Some(3));
    assert_eq!(ready.action_label(), "OK");
}

#[test]
fn finished_shows_the_outcome_with_ok() {
    // Arrange
    let dialog = dialog(ChatGptSignInPhase::Finished {
        message: "Sign-in failed: denied".to_string(),
    });

    // Act
    let message = dialog.message();

    // Assert
    assert_eq!(message, "Sign-in failed: denied");
    assert_eq!(dialog.action_label(), "OK");
    assert!(!dialog.is_loading());
    assert_eq!(dialog.waiting_request_id(), None);
}
