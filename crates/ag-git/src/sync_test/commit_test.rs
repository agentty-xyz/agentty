//! Commit workflow tests covering hook retries and rejections, amend
//! messages, staging, and output parsing.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};

use opentelemetry::Value;
use tempfile::tempdir;

#[cfg(unix)]
use super::support::write_executable_hook;
use super::support::{git_command_stdout, run_git_command, setup_test_git_repo};
use crate::error::GitError;
#[cfg(unix)]
use crate::hook::{HookOutput, MockCommitHookRunner};
use crate::hook_support::HostHooks;
use crate::sync::{
    SingleCommitMessageStrategy, commit_all, commit_all_preserving_single_commit,
    head_commit_message, parse_commit_count, parse_resolved_hash, stage_all,
};
use crate::telemetry_support::{CapturedTrace, SpanCapture, attribute};

#[test]
fn parse_resolved_hash_trims_output_and_rejects_empty_output() {
    // Arrange
    let error_context = "Failed to resolve HEAD hash";

    // Act
    let hash = parse_resolved_hash("abc1234\n", error_context);
    let empty = parse_resolved_hash(" \n", error_context);

    // Assert
    assert_eq!(hash.expect("hash should parse"), "abc1234");
    assert!(matches!(
        empty,
        Err(GitError::OutputParse(message))
            if message == "Failed to resolve HEAD hash: empty output"
    ));
}

#[test]
fn parse_commit_count_rejects_non_numeric_output() {
    // Arrange
    let rev_list_output = "not-a-count\n";

    // Act
    let result = parse_commit_count(rev_list_output, "main");

    // Assert
    assert!(matches!(
        result,
        Err(GitError::OutputParse(message))
            if message.starts_with("Failed to parse commit count since base branch `main`:")
    ));
}

#[tokio::test]
async fn commit_all_reports_no_changes_for_clean_worktree() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    let capture = SpanCapture::install().await;

    // Act
    let (result, trace) = capture
        .trace(commit_all(
            temp_dir.path().to_path_buf(),
            "No changes".to_string(),
            HostHooks::shared(),
        ))
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git commit" && stderr == "Nothing to commit: no changes detected"
    ));
    assert_eq!(
        git_command_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]),
        "1"
    );
    assert_eq!(
        commit_attempt_results(&trace),
        [commit_attempt_result(
            "nothing_to_commit",
            "completed",
            None
        )]
    );
    assert!(
        trace
            .named("git.command")
            .iter()
            .any(
                |command| attribute(command, "git.subcommand") == Some(&Value::from("commit"))
                    && command.parent_span_id
                        == trace.named("git.commit.attempt")[0].span_context.span_id()
            )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn commit_all_restages_and_retries_after_hook_modifies_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("repo");
    let attempt_log = configure_counting_pre_commit_hook(
        temp_dir.path(),
        &repo_path,
        "if [ \"$attempt\" -eq 1 ]; then\n  printf 'formatted\\n' > README.md\n  echo 'files were \
         modified by this hook' >&2\n  exit 1\nfi\n",
    );
    fs::write(repo_path.join("README.md"), "changed\n").expect("failed to write worktree change");
    let capture = SpanCapture::install().await;

    // Act
    let (result, trace) = capture
        .trace(commit_all(
            repo_path.clone(),
            "Change README".to_string(),
            HostHooks::shared(),
        ))
        .await;

    // Assert
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        git_command_stdout(&repo_path, &["show", "HEAD:README.md"]),
        "formatted"
    );
    assert_eq!(hook_attempt_count(&attempt_log), 2);
    assert_eq!(
        commit_attempt_results(&trace),
        [
            commit_attempt_result("hook_modified", "completed", None),
            commit_attempt_result("committed", "completed", None),
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn commit_all_stops_after_hooks_keep_modifying_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("repo");
    let attempt_log = configure_counting_pre_commit_hook(
        temp_dir.path(),
        &repo_path,
        "echo 'files were modified by this hook' >&2\nexit 1\n",
    );
    fs::write(repo_path.join("README.md"), "changed\n").expect("failed to write worktree change");

    // Act
    let result = commit_all(
        repo_path.clone(),
        "Change README".to_string(),
        HostHooks::shared(),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git commit"
                && stderr.contains("commit hooks kept modifying files after 5 attempts")
    ));
    assert_eq!(hook_attempt_count(&attempt_log), 5);
    assert_eq!(
        git_command_stdout(&repo_path, &["rev-list", "--count", "HEAD"]),
        "1"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn commit_all_preserves_hook_rejection_output() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("repo");
    let attempt_log = configure_counting_pre_commit_hook(
        temp_dir.path(),
        &repo_path,
        "echo 'lint failed' >&2\nexit 1\n",
    );
    fs::write(repo_path.join("README.md"), "changed\n").expect("failed to write worktree change");
    let capture = SpanCapture::install().await;

    // Act
    let (result, trace) = capture
        .trace(commit_all(
            repo_path.clone(),
            "Change README".to_string(),
            HostHooks::shared(),
        ))
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git commit" && stderr.contains("lint failed")
    ));
    assert_eq!(hook_attempt_count(&attempt_log), 1);
    assert_eq!(
        commit_attempt_results(&trace),
        [commit_attempt_result("failed", "failed", Some("execution"))]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn commit_all_runs_no_worktree_written_hook_on_the_host() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("repo");
    let marker = temp_dir.path().join("ran-on-host");
    fs::create_dir(&repo_path).expect("failed to create repository directory");
    setup_test_git_repo(&repo_path);
    // Trusted configuration: hooks live in the worktree, as hook managers
    // such as husky set them up.
    run_git_command(&repo_path, &["config", "core.hooksPath", ".githooks"]);
    // Session-written worktree file.
    write_executable_hook(
        &repo_path.join(".githooks/pre-commit"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    fs::write(repo_path.join("README.md"), "changed\n").expect("failed to write worktree change");

    // Act
    let result = commit_all(
        repo_path.clone(),
        "Change README".to_string(),
        accepting_hooks(&repo_path),
    )
    .await;

    // Assert
    assert!(result.is_ok(), "{result:?}");
    assert!(!marker.exists(), "a worktree-written hook ran on the host");
    assert_eq!(
        git_command_stdout(&repo_path, &["log", "-1", "--pretty=%s"]),
        "Change README"
    );
    // Positive control: plain Git runs the hook.
    run_git_command(&repo_path, &["commit", "--allow-empty", "-m", "Plain"]);
    assert!(marker.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn commit_all_runs_no_command_from_worktree_hook_configuration_on_the_host() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("repo");
    let marker = temp_dir.path().join("ran-on-host");
    fs::create_dir(&repo_path).expect("failed to create repository directory");
    setup_test_git_repo(&repo_path);
    // Trusted hook installed by a hook manager, which runs the commands the
    // worktree's configuration file names.
    write_executable_hook(
        &repo_path.join(".git/hooks/pre-commit"),
        "#!/bin/sh\n[ -f hooks.conf ] && sh hooks.conf\nexit 0\n",
    );
    // Session-written configuration.
    fs::write(
        repo_path.join("hooks.conf"),
        format!("touch '{}'\n", marker.display()),
    )
    .expect("failed to write hook configuration");

    // Act
    let result = commit_all(
        repo_path.clone(),
        "Add hook config".to_string(),
        accepting_hooks(&repo_path),
    )
    .await;

    // Assert
    assert!(result.is_ok(), "{result:?}");
    assert!(
        !marker.exists(),
        "worktree hook configuration ran on the host"
    );
}

#[tokio::test]
async fn commit_all_reports_a_host_commit_failure_after_the_hook_accepts() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["config", "commit.gpgsign", "true"]);
    run_git_command(temp_dir.path(), &["config", "gpg.program", "false"]);
    fs::write(temp_dir.path().join("README.md"), "changed\n").expect("failed to write change");

    // Act
    let result = commit_all(
        temp_dir.path().to_path_buf(),
        "Change README".to_string(),
        HostHooks::shared(),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git commit" && stderr.contains("gpg")
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn commit_all_reports_a_hook_runner_failure_without_committing() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    write_executable_hook(
        &temp_dir.path().join(".git/hooks/pre-commit"),
        "#!/bin/sh\nexit 0\n",
    );
    fs::write(temp_dir.path().join("README.md"), "changed\n").expect("failed to write change");
    let mut hooks = MockCommitHookRunner::new();
    hooks.expect_run_pre_commit().times(1).returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "git hook run".to_string(),
                stderr: "sandbox refused".to_string(),
            })
        })
    });

    // Act
    let result = commit_all(
        temp_dir.path().to_path_buf(),
        "Change README".to_string(),
        std::sync::Arc::new(hooks),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref stderr, .. }) if stderr == "sandbox refused"
    ));
    assert_eq!(
        git_command_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]),
        "1"
    );
}

/// Returns a hook runner that accepts every commit after checking that it
/// was asked to run the hook of `repo_path`, standing in for a sandboxed
/// runner whose hook passed.
#[cfg(unix)]
fn accepting_hooks(repo_path: &Path) -> std::sync::Arc<dyn crate::hook::CommitHookRunner> {
    let repo_path = repo_path.to_path_buf();
    let mut hooks = MockCommitHookRunner::new();
    hooks
        .expect_run_pre_commit()
        .withf(move |path| *path == repo_path)
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Ok(HookOutput {
                    output: String::new(),
                    success: true,
                })
            })
        });

    std::sync::Arc::new(hooks)
}

#[tokio::test]
async fn commit_all_preserving_single_commit_reuses_head_message_on_amend() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(temp_dir.path().join("session.txt"), "first\n")
        .expect("failed to write session file");
    run_git_command(temp_dir.path(), &["add", "session.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Session commit"]);
    fs::write(temp_dir.path().join("session.txt"), "second\n")
        .expect("failed to update session file");

    // Act
    let result = commit_all_preserving_single_commit(
        temp_dir.path().to_path_buf(),
        "main".to_string(),
        "Ignored message".to_string(),
        SingleCommitMessageStrategy::Reuse,
        HostHooks::shared(),
    )
    .await;

    // Assert
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        git_command_stdout(temp_dir.path(), &["rev-list", "--count", "main..HEAD"]),
        "1"
    );
    assert_eq!(
        git_command_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]),
        "Session commit"
    );
    assert_eq!(
        git_command_stdout(temp_dir.path(), &["show", "HEAD:session.txt"]),
        "second"
    );
}

#[tokio::test]
async fn stage_all_reports_failure_outside_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = stage_all(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git rev-parse --path-format=absolute --git-common-dir"
                && stderr.starts_with("Failed to resolve the common Git directory:")
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn stage_all_reports_files_git_cannot_read() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    let unreadable = temp_dir.path().join("unreadable.txt");
    fs::write(&unreadable, "secret").expect("failed to write file");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
        .expect("failed to make file unreadable");

    // Act
    let result = stage_all(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git add -A" && stderr.starts_with("Failed to stage changes:")
    ));
}

#[tokio::test]
async fn head_commit_message_returns_none_for_unborn_head() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    run_git_command(temp_dir.path(), &["init", "-b", "main"]);

    // Act
    let message = head_commit_message(temp_dir.path().to_path_buf()).await;

    // Assert
    assert_eq!(message.expect("unborn HEAD should not fail"), None);
}

#[tokio::test]
async fn head_commit_message_reports_failure_outside_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = head_commit_message(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref command, ref stderr })
            if command == "git rev-parse --verify HEAD"
                && stderr.to_ascii_lowercase().contains("not a git repository")
    ));
}

/// Creates a committed repository at `repo_path` whose `pre-commit` hook
/// appends each run to an attempt log outside the worktree before running
/// `body` with the 1-based `attempt` number in scope.
///
/// Returns the attempt log path.
#[cfg(unix)]
fn configure_counting_pre_commit_hook(
    fixture_root: &Path,
    repo_path: &Path,
    body: &str,
) -> PathBuf {
    let hooks_path = fixture_root.join("hooks");
    let attempt_log = fixture_root.join("hook-attempts.log");
    fs::create_dir(repo_path).expect("failed to create repository directory");
    setup_test_git_repo(repo_path);
    let hooks_path_text = hooks_path.to_string_lossy().into_owned();
    run_git_command(repo_path, &["config", "core.hooksPath", &hooks_path_text]);
    write_executable_hook(
        &hooks_path.join("pre-commit"),
        &format!(
            "#!/bin/sh\necho run >> '{}'\nattempt=$(wc -l < '{}' | tr -d ' ')\n{body}exit 0\n",
            attempt_log.display(),
            attempt_log.display()
        ),
    );

    attempt_log
}

/// Result, outcome, and error type recorded on one `git.commit.attempt`.
type CommitAttemptRecord = (Option<Value>, Option<Value>, Option<Value>);

/// Returns the records of every commit attempt in `trace`, in order.
fn commit_attempt_results(trace: &CapturedTrace) -> Vec<CommitAttemptRecord> {
    trace
        .named("git.commit.attempt")
        .into_iter()
        .map(|attempt| {
            (
                attribute(attempt, "agentty.git.commit.result").cloned(),
                attribute(attempt, "agentty.outcome").cloned(),
                attribute(attempt, "error.type").cloned(),
            )
        })
        .collect()
}

/// Builds the expected record of one commit attempt.
fn commit_attempt_result(
    result: &'static str,
    outcome: &'static str,
    error_type: Option<&'static str>,
) -> CommitAttemptRecord {
    (
        Some(Value::from(result)),
        Some(Value::from(outcome)),
        error_type.map(Value::from),
    )
}

/// Returns how many times the counting `pre-commit` hook ran.
#[cfg(unix)]
fn hook_attempt_count(attempt_log: &Path) -> usize {
    fs::read_to_string(attempt_log)
        .expect("hook attempt log should exist")
        .lines()
        .count()
}
