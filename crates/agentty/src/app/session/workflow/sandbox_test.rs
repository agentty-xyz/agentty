//! Sandbox notice reduction tests: the warning once per session or when it
//! changes, and the denial hint for each turn that reports one.

use super::TurnSandbox;
use crate::domain::session_message::{SessionMessage, SessionMessageKind};

/// Returns the turn state after observing the warnings `messages`, each
/// with the same remedy.
fn unavailable(messages: &[&str]) -> TurnSandbox {
    let mut sandbox = TurnSandbox::default();
    for message in messages {
        sandbox.observe_unavailable(message, "Install bubblewrap.");
    }

    sandbox
}

#[test]
fn quiet_turns_add_no_notices() {
    // Arrange
    let sandbox = TurnSandbox::default();

    // Act
    let notices = sandbox.notices(&[]);

    // Assert
    assert_eq!(notices, Vec::<String>::new());
}

#[test]
fn the_warning_is_shown_once_per_session_until_it_changes() {
    // Arrange
    let first = unavailable(&["Bubblewrap is missing."]);
    let shown = first.notices(&[]);
    let transcript = [
        SessionMessage::new(0, SessionMessageKind::WorkflowNotice, shown[0].clone()),
        SessionMessage::new(
            1,
            SessionMessageKind::WorkflowNotice,
            "\n[Commit] committed\n",
        ),
        SessionMessage::new(2, SessionMessageKind::UserPrompt, "Next"),
    ];
    let changed = unavailable(&["User namespaces are disabled."]);

    // Act
    let repeated = first.notices(&transcript);
    let after_change = changed.notices(&transcript);

    // Assert
    assert_eq!(
        shown,
        ["\n[Sandbox Warning] Bubblewrap is missing. Install bubblewrap.\n"]
    );
    assert_eq!(repeated, Vec::<String>::new());
    assert_eq!(
        after_change,
        ["\n[Sandbox Warning] User namespaces are disabled. Install bubblewrap.\n"]
    );
}

#[test]
fn a_likely_denial_adds_the_hint_after_the_latest_warning() {
    // Arrange
    let mut sandbox = unavailable(&["Old reason.", "New reason."]);
    sandbox.observe_likely_denial();

    // Act
    let notices = sandbox.notices(&[]);

    // Assert
    assert_eq!(notices.len(), 2);
    assert_eq!(
        notices[0],
        "\n[Sandbox Warning] New reason. Install bubblewrap.\n"
    );
    assert!(notices[1].starts_with("\n[Sandbox] A command may have been blocked by the sandbox."));
    assert!(notices[1].contains("You can run commands that need more access outside the session."));
}
