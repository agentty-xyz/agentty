//! Sandboxed commit hook tests: the hook request, its outcome, failures to
//! run it, and the sandbox notices a session hook run reports.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentError, CommandScope, MockSessionCommandClient, SessionCommandOutput,
    SessionCommandRequest, TurnNotice,
};
use ag_git::{CommitHookRunner, GitError, HookOutput, PRE_COMMIT_HOOK_COMMAND};
use tokio::sync::mpsc;

use super::{SandboxedCommitHookRunner, SessionHookTarget};
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

/// Returns a client answering every run with `exit_code`, `output`, and
/// `sandbox`, recording each request in `requests`.
fn client(
    requests: &Arc<Mutex<Vec<SessionCommandRequest>>>,
    exit_code: i32,
    output: &'static str,
    sandbox: &TurnNotice,
) -> Arc<MockSessionCommandClient> {
    let requests = Arc::clone(requests);
    let sandbox = sandbox.clone();
    let mut client = MockSessionCommandClient::new();
    client.expect_run().returning(move |request| {
        requests.lock().expect("requests").push(request);
        let sandbox = sandbox.clone();

        Box::pin(async move {
            Ok(SessionCommandOutput {
                exit_code: Some(exit_code),
                output: output.to_string(),
                sandbox,
            })
        })
    });

    Arc::new(client)
}

/// Returns the target of session `session-1` with an empty ephemeral
/// transcript.
async fn session_target() -> SessionHookTarget {
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();

    SessionHookTarget {
        app_event_tx,
        db: AppRepositories::in_memory().await.expect("db"),
        session_id: SessionId::from("session-1"),
        session_update_versions: Arc::default(),
        transcript: Arc::new(Mutex::new(SessionTranscript::ephemeral(Vec::new()))),
    }
}

/// Returns the transcript contents of `target`.
fn transcript(target: &SessionHookTarget) -> Vec<String> {
    target
        .transcript
        .lock()
        .expect("transcript")
        .messages()
        .iter()
        .map(|message| message.content.clone())
        .collect()
}

#[tokio::test]
async fn a_session_hook_runs_in_the_session_sandbox_and_reports_its_output() {
    // Arrange
    let requests = Arc::default();
    let target = session_target().await;
    let runner = SandboxedCommitHookRunner::session(
        client(&requests, 0, "checks passed", &TurnNotice::SandboxAvailable),
        target.clone(),
    );

    // Act
    let output = runner
        .run_pre_commit(PathBuf::from("/worktree"))
        .await
        .expect("hook runs");

    // Assert
    assert_eq!(
        output,
        HookOutput {
            output: "checks passed".to_string(),
            success: true,
        }
    );
    assert_eq!(
        *requests.lock().expect("requests"),
        [SessionCommandRequest {
            command: PRE_COMMIT_HOOK_COMMAND.to_string(),
            folder: PathBuf::from("/worktree"),
            scope: CommandScope::Session {
                session_id: "session-1".to_string(),
            },
        }]
    );
    assert_eq!(transcript(&target), Vec::<String>::new());
}

#[tokio::test]
async fn a_rejecting_hook_reports_its_output_without_success() {
    // Arrange
    let requests = Arc::default();
    let runner = SandboxedCommitHookRunner::session(
        client(
            &requests,
            1,
            "trailing whitespace",
            &TurnNotice::SandboxAvailable,
        ),
        session_target().await,
    );

    // Act
    let output = runner
        .run_pre_commit(PathBuf::from("/worktree"))
        .await
        .expect("hook runs");

    // Assert
    assert_eq!(
        output,
        HookOutput {
            output: "trailing whitespace".to_string(),
            success: false,
        }
    );
}

#[tokio::test]
async fn a_hook_that_cannot_run_fails_with_the_client_error() {
    // Arrange
    let mut client = MockSessionCommandClient::new();
    client
        .expect_run()
        .returning(|_| Box::pin(async { Err(AgentError::Runtime("sandbox refused".to_string())) }));
    let runner = SandboxedCommitHookRunner::workspace(Arc::new(client));

    // Act
    let error = runner
        .run_pre_commit(PathBuf::from("/checkout"))
        .await
        .expect_err("hook cannot run");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git hook run pre-commit" && stderr == "sandbox refused"
    ));
}

#[tokio::test]
async fn session_hooks_warn_once_without_the_sandbox_and_note_when_it_returns() {
    // Arrange
    let requests = Arc::default();
    let target = session_target().await;
    let unavailable = TurnNotice::SandboxUnavailable {
        message: "Bubblewrap is missing.".to_string(),
        remedy: "Install bubblewrap.".to_string(),
    };
    let warned =
        SandboxedCommitHookRunner::session(client(&requests, 0, "", &unavailable), target.clone());
    let restored = SandboxedCommitHookRunner::session(
        client(&requests, 0, "", &TurnNotice::SandboxAvailable),
        target.clone(),
    );

    // Act
    for runner in [&warned, &warned, &restored, &restored] {
        runner
            .run_pre_commit(PathBuf::from("/worktree"))
            .await
            .expect("hook runs");
    }

    // Assert
    assert_eq!(
        transcript(&target),
        [
            "\n[Sandbox Warning] Bubblewrap is missing. Install bubblewrap.\n",
            "\n[Sandbox] Commands run in the sandbox again.\n",
        ]
    );
}

#[tokio::test]
async fn a_workspace_hook_runs_in_a_sandbox_scoped_to_the_checkout() {
    // Arrange
    let requests = Arc::default();
    let runner = SandboxedCommitHookRunner::workspace(client(
        &requests,
        0,
        "ok",
        &TurnNotice::SandboxAvailable,
    ));

    // Act
    let output = runner
        .run_pre_commit(PathBuf::from("/checkout"))
        .await
        .expect("hook runs");

    // Assert
    assert!(output.success);
    assert_eq!(
        *requests.lock().expect("requests"),
        [SessionCommandRequest {
            command: PRE_COMMIT_HOOK_COMMAND.to_string(),
            folder: PathBuf::from("/checkout"),
            scope: CommandScope::Workspace,
        }]
    );
}
