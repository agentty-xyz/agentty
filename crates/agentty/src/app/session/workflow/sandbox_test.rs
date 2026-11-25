//! Sandbox notice tests: the warning once per change of the sandbox state,
//! a note when it works again, and the denial hint once per turn.

use ag_contracts::TurnNotice;

use super::SandboxNotices;
use crate::domain::session_message::{SessionMessage, SessionMessageKind};

/// Returns a warning notice for `message`.
fn unavailable(message: &str) -> TurnNotice {
    TurnNotice::SandboxUnavailable {
        message: message.to_string(),
        remedy: "Install bubblewrap.".to_string(),
    }
}

/// Returns `notices` as the workflow notices of a transcript.
fn transcript(notices: &[&str]) -> Vec<SessionMessage> {
    notices
        .iter()
        .zip(0..)
        .map(|(notice, position)| {
            SessionMessage::new(position, SessionMessageKind::WorkflowNotice, *notice)
        })
        .collect()
}

#[test]
fn the_warning_is_shown_once_per_change_of_the_sandbox_state() {
    // Arrange
    let mut sandbox = SandboxNotices::default();
    let warning = sandbox
        .notice(&unavailable("Bubblewrap is missing."), &[])
        .expect("first warning");
    let warned = transcript(&[&warning, "\n[Commit] committed\n"]);

    // Act
    let repeated = sandbox.notice(&unavailable("Bubblewrap is missing."), &warned);
    let changed = sandbox.notice(&unavailable("User namespaces are disabled."), &warned);
    let restored = sandbox
        .notice(&TurnNotice::SandboxAvailable, &warned)
        .expect("restored note");
    let after_restore = transcript(&[&warning, &restored]);
    let broken_again = sandbox.notice(&unavailable("Bubblewrap is missing."), &after_restore);
    let still_working = sandbox.notice(&TurnNotice::SandboxAvailable, &after_restore);

    // Assert
    assert_eq!(
        warning,
        "\n[Sandbox Warning] Bubblewrap is missing. Install bubblewrap.\n"
    );
    assert_eq!(repeated, None);
    assert_eq!(
        changed.as_deref(),
        Some("\n[Sandbox Warning] User namespaces are disabled. Install bubblewrap.\n")
    );
    assert_eq!(restored, "\n[Sandbox] Commands run in the sandbox again.\n");
    assert_eq!(broken_again, Some(warning));
    assert_eq!(still_working, None);
}

#[test]
fn caches_kept_per_session_are_named_once_per_session_and_set() {
    // Arrange
    let mut sandbox = SandboxNotices::default();
    let kept = |caches: &[&str]| TurnNotice::HostCachesNotShared {
        caches: caches.iter().map(ToString::to_string).collect(),
    };
    let first = sandbox
        .notice(&kept(&["Cargo", "npm"]), &[])
        .expect("first notice");
    let shown = transcript(&[&first, "\n[Commit] committed\n"]);

    // Act
    let repeated = sandbox.notice(&kept(&["Cargo", "npm"]), &shown);
    let changed = sandbox.notice(&kept(&["Cargo"]), &shown);

    // Assert
    assert!(
        first.starts_with(
            "\n[Sandbox] `Share Host Caches` is on, but these caches stay per session"
        ),
        "{first}"
    );
    assert!(first.ends_with(": Cargo, npm.\n"), "{first}");
    assert_eq!(repeated, None);
    assert!(changed.is_some_and(|text| text.ends_with(": Cargo.\n")));
}

#[test]
fn a_working_sandbox_adds_nothing_to_a_session_without_warnings() {
    // Arrange
    let mut sandbox = SandboxNotices::default();

    // Act
    let available = sandbox.notice(&TurnNotice::SandboxAvailable, &[]);
    let compacted = sandbox.notice(&TurnNotice::ContextCompacted { model: None }, &[]);

    // Assert
    assert_eq!(available, None);
    assert_eq!(compacted, None);
}

#[test]
fn the_denial_hint_is_shown_once_per_turn() {
    // Arrange
    let mut sandbox = SandboxNotices::default();

    // Act
    let first = sandbox.notice(&TurnNotice::LikelySandboxDenial, &[]);
    let second = sandbox.notice(&TurnNotice::LikelySandboxDenial, &[]);

    // Assert
    let first = first.expect("hint");
    assert!(first.starts_with("\n[Sandbox] A command may have been blocked by the sandbox."));
    assert!(first.contains("You can run commands that need more access outside the session."));
    assert_eq!(second, None);
}
