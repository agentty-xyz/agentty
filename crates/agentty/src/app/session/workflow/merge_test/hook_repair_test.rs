use std::sync::{Arc, Mutex};

use ag_agent::OneShotError;
use ag_git::{GitError, MockGitClient, RebaseStepResult};
use ag_worker::MockRunClient;
use mockall::Sequence;

use super::super::{
    ExistingSessionRebaseAssistClient, MockSyncAssistClient, RebaseAssistFuture,
    RebaseAssistLoopInput, RebaseAssistMode,
};
use super::support::{build_rebase_assist_input_for_test, build_sync_rebase_input_for_test};
use crate::app::SessionManager;
use crate::app::session::SessionError;

struct RecordingAssistClient {
    prompts: Arc<Mutex<Vec<String>>>,
}

impl ExistingSessionRebaseAssistClient for RecordingAssistClient {
    fn resolve_rebase_conflicts(
        &self,
        prompt: String,
    ) -> RebaseAssistFuture<Result<(), SessionError>> {
        self.prompts.lock().expect("prompt lock").push(prompt);

        Box::pin(async { Ok(()) })
    }
}

/// The hook must pass after restaging before continuation becomes possible.
#[tokio::test]
async fn repairs_hook_failures_in_existing_session_and_continues() {
    // Arrange
    let mut git = MockGitClient::new();
    let mut sequence = Sequence::new();
    git.expect_list_conflicted_files()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(vec![]) }));
    git.expect_list_staged_conflict_marker_files()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(vec![]) }));
    for diagnostic in ["formatter changed files", "lint error in dependent crate"] {
        git.expect_run_pre_commit_hook()
            .once()
            .in_sequence(&mut sequence)
            .returning(move |_| Box::pin(async move { Err(hook_failure(diagnostic)) }));
        git.expect_stage_all()
            .once()
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
    }
    git.expect_run_pre_commit_hook()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    git.expect_rebase_continue()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(RebaseStepResult::Completed) }));
    git.expect_abort_rebase().never();
    let (_folder, mut input) = build_rebase_assist_input_for_test(Arc::new(git)).await;
    let prompts = Arc::new(Mutex::new(Vec::new()));
    input.assist_mode = RebaseAssistMode::ExistingSession(Arc::new(RecordingAssistClient {
        prompts: Arc::clone(&prompts),
    }));
    let transcript = Arc::clone(&input.transcript);

    // Act
    let result = SessionManager::run_rebase_assist_loop_core(
        RebaseAssistLoopInput::Session(Box::new(input)),
        None,
    )
    .await;

    // Assert
    assert!(result.is_ok());
    let prompts = prompts.lock().expect("prompt lock");
    assert_eq!(prompts.len(), 2);
    assert_repair_prompt(&prompts[0], "formatter changed files");
    assert_repair_prompt(&prompts[1], "lint error in dependent crate");
    assert!(!prompts[1].contains("formatter changed files"));
    assert!(
        transcript
            .lock()
            .expect("transcript lock")
            .replay_text()
            .unwrap_or_default()
            .contains("Repairing pre-commit hook failure")
    );
}

#[tokio::test]
async fn repairs_project_hook_failure_through_sync_assistance() {
    // Arrange
    let mut git = MockGitClient::new();
    let mut sequence = Sequence::new();
    git.expect_run_pre_commit_hook()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Err(hook_failure("formatting failed")) }));
    let mut assist = MockSyncAssistClient::new();
    assist
        .expect_resolve_rebase_conflicts()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_, prompt, _| {
            assert_repair_prompt(&prompt, "formatting failed");
            Box::pin(async { Ok(()) })
        });
    git.expect_stage_all()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    git.expect_run_pre_commit_hook()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    let folder = tempfile::tempdir().expect("temporary checkout");
    let input = RebaseAssistLoopInput::Project(build_sync_rebase_input_for_test(
        folder.path().to_path_buf(),
        Arc::new(git),
        Arc::new(assist),
    ));

    // Act
    let result = input.run_pre_commit_hook().await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn project_hook_repair_preserves_assistance_errors() {
    // Arrange
    let mut git = MockGitClient::new();
    git.expect_run_pre_commit_hook()
        .once()
        .returning(|_| Box::pin(async { Err(hook_failure("lint failed")) }));
    git.expect_stage_all().never();
    let mut assist = MockSyncAssistClient::new();
    assist
        .expect_resolve_rebase_conflicts()
        .once()
        .returning(|_, _, _| {
            Box::pin(async { Err(SessionError::Workflow("agent unavailable".into())) })
        });
    let folder = tempfile::tempdir().expect("temporary checkout");
    let input = RebaseAssistLoopInput::Project(build_sync_rebase_input_for_test(
        folder.path().to_path_buf(),
        Arc::new(git),
        Arc::new(assist),
    ));

    // Act
    let error = input
        .run_pre_commit_hook()
        .await
        .expect_err("assistance should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Sync rebase hook repair failed: agent unavailable")
    );
}

#[tokio::test]
async fn session_hook_repair_aborts_on_assistance_or_staging_failure() {
    for fail_assistance in [true, false] {
        // Arrange
        let mut git = MockGitClient::new();
        git.expect_list_conflicted_files()
            .once()
            .returning(|_| Box::pin(async { Ok(vec![]) }));
        git.expect_list_staged_conflict_marker_files()
            .once()
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        git.expect_run_pre_commit_hook()
            .once()
            .returning(|_| Box::pin(async { Err(hook_failure("lint failed")) }));
        git.expect_stage_all()
            .times(usize::from(!fail_assistance))
            .returning(|_| Box::pin(async { Err(GitError::OutputParse("staging failed".into())) }));
        git.expect_rebase_continue().never();
        git.expect_abort_rebase()
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_folder, mut input) = build_rebase_assist_input_for_test(Arc::new(git)).await;
        if fail_assistance {
            let mut run_client = MockRunClient::new();
            run_client.expect_submit().once().returning(|request| {
                assert_repair_prompt(&request.prompt, "lint failed");
                Err(OneShotError::new("agent unavailable"))
            });
            input.run_client = Arc::new(run_client);
        }

        // Act
        let error = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(Box::new(input)),
            None,
        )
        .await
        .expect_err("repair should fail");

        // Assert
        let expected = if fail_assistance {
            "agent unavailable"
        } else {
            "staging failed"
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
}

fn hook_failure(diagnostic: &str) -> GitError {
    GitError::CommandFailed {
        command: "git hook run pre-commit".into(),
        stderr: diagnostic.into(),
    }
}

fn assert_repair_prompt(prompt: &str, diagnostic: &str) {
    assert!(prompt.contains(diagnostic));
    assert!(prompt.contains("rebase remains paused"));
    assert!(prompt.contains("current checkout"));
    assert!(prompt.contains("Preserve the conflict resolutions"));
    assert!(prompt.contains("Never skip or disable hooks"));
    assert!(prompt.contains("Do not stage files, create commits"));
    assert!(prompt.contains("Agentty will stage"));
}
