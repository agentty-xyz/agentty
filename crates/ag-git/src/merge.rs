use std::path::{Path, PathBuf};

use tempfile::tempdir;
use tokio::task::spawn_blocking;

use super::error::GitError;
use super::repo::{command_output_detail, run_git_command_output_sync, run_git_command_sync};
use super::worktree::detect_git_info_sync;

/// Outcome of attempting a squash merge operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SquashMergeOutcome {
    /// Squash merge staged changes and created a commit.
    Committed,
    /// Squash merge staged nothing because changes already exist in target.
    AlreadyPresentInTarget,
}

/// Outcome classification for one attempted `merge-tree --write-tree` probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MergeTreeAttempt {
    Clean,
    Conflict,
    Unsupported,
    Failed,
}

/// Captured output from the compatibility merge command.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CompatibilityMergeOutput {
    stderr: Vec<u8>,
    stdout: Vec<u8>,
    success: bool,
}

/// Executes the Git commands used by the compatibility merge probe.
#[cfg_attr(test, mockall::automock)]
trait CompatibilityMergeRunner: Send + Sync {
    /// Runs a Git command that must succeed and returns its standard output.
    fn run_git_command(
        &self,
        repo_path: &Path,
        args: &[String],
        error_context: &str,
    ) -> Result<String, GitError>;

    /// Runs the merge command and returns its status and captured output.
    fn run_git_command_output(
        &self,
        repo_path: &Path,
        args: &[String],
    ) -> Result<CompatibilityMergeOutput, GitError>;
}

/// Compatibility merge runner backed by local Git subprocesses.
struct ProcessCompatibilityMergeRunner;

impl CompatibilityMergeRunner for ProcessCompatibilityMergeRunner {
    fn run_git_command(
        &self,
        repo_path: &Path,
        args: &[String],
        error_context: &str,
    ) -> Result<String, GitError> {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();

        run_git_command_sync(repo_path, &args, error_context)
    }

    fn run_git_command_output(
        &self,
        repo_path: &Path,
        args: &[String],
    ) -> Result<CompatibilityMergeOutput, GitError> {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = run_git_command_output_sync(repo_path, &args)?;
        let success = output.status.success();

        Ok(CompatibilityMergeOutput {
            stderr: output.stderr,
            stdout: output.stdout,
            success,
        })
    }
}

/// Returns whether merging `source_branch` into `target_branch` would produce
/// conflicts without reading or changing the repository index or worktree.
///
/// # Errors
/// Returns an error when either branch cannot be resolved or neither the
/// native `git merge-tree` probe nor its compatibility fallback can compute
/// the merge.
pub(crate) async fn has_merge_conflicts(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
) -> Result<bool, GitError> {
    spawn_blocking(move || {
        let output = run_git_command_output_sync(
            &repo_path,
            &[
                "merge-tree",
                "--write-tree",
                target_branch.as_str(),
                source_branch.as_str(),
            ],
        )?;

        let attempt = classify_merge_tree_attempt(
            output.status.code(),
            output.stdout.as_slice(),
            output.stderr.as_slice(),
        );

        resolve_merge_tree_attempt(
            &repo_path,
            source_branch.as_str(),
            target_branch.as_str(),
            attempt,
            output.stdout.as_slice(),
            output.stderr.as_slice(),
        )
    })
    .await?
}

/// Classifies native merge-tree output, including the pre-2.38 unsupported
/// synopsis that does not advertise `--write-tree`.
fn classify_merge_tree_attempt(
    exit_code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> MergeTreeAttempt {
    match exit_code {
        Some(0) => MergeTreeAttempt::Clean,
        Some(1) if stderr.is_empty() => MergeTreeAttempt::Conflict,
        Some(129)
            if !String::from_utf8_lossy(stdout).contains("--write-tree")
                && !String::from_utf8_lossy(stderr).contains("--write-tree") =>
        {
            MergeTreeAttempt::Unsupported
        }
        _ => MergeTreeAttempt::Failed,
    }
}

/// Resolves a classified native probe, delegating unsupported Git versions to
/// an isolated compatibility merge.
fn resolve_merge_tree_attempt(
    repo_path: &std::path::Path,
    source_branch: &str,
    target_branch: &str,
    attempt: MergeTreeAttempt,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<bool, GitError> {
    match attempt {
        MergeTreeAttempt::Clean => Ok(false),
        MergeTreeAttempt::Conflict => Ok(true),
        MergeTreeAttempt::Unsupported => {
            has_merge_conflicts_via_temporary_clone(repo_path, source_branch, target_branch)
        }
        MergeTreeAttempt::Failed => {
            let detail = command_output_detail(stdout, stderr);

            Err(GitError::CommandFailed {
                command: format!("git merge-tree --write-tree {target_branch} {source_branch}"),
                stderr: format!("Failed to inspect merge conflicts: {detail}"),
            })
        }
    }
}

/// Computes the merge in a disposable local clone for Git versions whose
/// `merge-tree` lacks `--write-tree`.
fn has_merge_conflicts_via_temporary_clone(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
) -> Result<bool, GitError> {
    let temporary_directory = tempdir()?;
    let command_runner = ProcessCompatibilityMergeRunner;

    has_merge_conflicts_via_temporary_clone_with_runner(
        repo_path,
        source_branch,
        target_branch,
        &temporary_directory,
        &command_runner,
    )
}

/// Computes a compatibility merge through an injectable command boundary.
fn has_merge_conflicts_via_temporary_clone_with_runner(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
    temporary_directory: &tempfile::TempDir,
    command_runner: &dyn CompatibilityMergeRunner,
) -> Result<bool, GitError> {
    let source_revision = format!("{source_branch}^{{commit}}");
    let source_commit = command_runner.run_git_command(
        repo_path,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            source_revision,
        ],
        "Failed to resolve merge source",
    )?;
    let target_revision = format!("{target_branch}^{{commit}}");
    let target_commit = command_runner.run_git_command(
        repo_path,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            target_revision,
        ],
        "Failed to resolve merge target",
    )?;
    let source_commit = source_commit.trim();
    let target_commit = target_commit.trim();

    let clone_path = temporary_directory.path().join("repository");
    let clone_path_text = clone_path.to_string_lossy();
    command_runner.run_git_command(
        repo_path,
        &[
            "clone".to_string(),
            "--shared".to_string(),
            "--no-checkout".to_string(),
            "--quiet".to_string(),
            ".".to_string(),
            clone_path_text.into_owned(),
        ],
        "Failed to create compatibility merge clone",
    )?;
    command_runner.run_git_command(
        &clone_path,
        &[
            "checkout".to_string(),
            "--detach".to_string(),
            "--quiet".to_string(),
            target_commit.to_string(),
        ],
        "Failed to check out compatibility merge target",
    )?;

    let disabled_hooks_path = temporary_directory.path().join("disabled-hooks");
    let disabled_hooks_path = disabled_hooks_path.to_string_lossy();
    let hooks_config = format!("core.hooksPath={disabled_hooks_path}");
    let merge_output = command_runner.run_git_command_output(
        &clone_path,
        &[
            "-c".to_string(),
            hooks_config,
            "-c".to_string(),
            "user.name=Agentty".to_string(),
            "-c".to_string(),
            "user.email=agentty@localhost".to_string(),
            "-c".to_string(),
            "user.useConfigOnly=true".to_string(),
            "merge".to_string(),
            "--no-commit".to_string(),
            "--no-ff".to_string(),
            source_commit.to_string(),
        ],
    )?;
    if merge_output.success {
        return Ok(false);
    }

    let unmerged_files = command_runner.run_git_command(
        &clone_path,
        &["ls-files".to_string(), "--unmerged".to_string()],
        "Failed to inspect compatibility merge conflicts",
    )?;
    if !unmerged_files.trim().is_empty() {
        return Ok(true);
    }

    let detail = command_output_detail(&merge_output.stdout, &merge_output.stderr);

    Err(GitError::CommandFailed {
        command: format!("git merge --no-commit --no-ff {source_commit}"),
        stderr: format!("Failed to inspect merge conflicts in compatibility clone: {detail}"),
    })
}

/// Returns the full patch diff that will be squashed when merging a source
/// branch into a target branch.
///
/// Uses `git diff <target>..<source>`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `source_branch` - Name of the branch being merged
/// * `target_branch` - Name of the branch receiving the squash merge
///
/// # Returns
/// The full patch diff for the squash merge range.
///
/// # Errors
/// Returns an error if invoking `git` fails or `git diff` exits with a
/// non-zero status.
pub(crate) async fn squash_merge_diff(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
) -> Result<String, GitError> {
    spawn_blocking(move || {
        let revision_range = format!("{target_branch}..{source_branch}");

        run_git_command_sync(
            &repo_path,
            &["diff", revision_range.as_str()],
            "Failed to read squash merge diff",
        )
    })
    .await?
}

/// Performs a squash merge from a source branch to a target branch.
///
/// This function:
/// 1. Verifies the repository is already on the target branch
/// 2. Performs `git merge --squash` from the source branch
/// 3. Commits the squashed changes, running configured commit hooks
///
/// The caller is responsible for ensuring `repo_path` is already checked out
/// on `target_branch`. Switching branches here would disrupt the user's
/// working directory.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root, already on `target_branch`
/// * `source_branch` - Name of the branch to merge from (e.g., `wt/abc123`)
/// * `target_branch` - Name of the branch to merge into (e.g., `main`)
/// * `commit_message` - Message for the squash commit
///
/// # Returns
/// A [`SquashMergeOutcome`] describing whether a squash commit was created.
///
/// # Errors
/// Returns an error if the repository is on the wrong branch, the merge
/// fails, or the commit or a configured commit hook fails.
pub(crate) async fn squash_merge(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
    commit_message: String,
) -> Result<SquashMergeOutcome, GitError> {
    spawn_blocking(move || {
        // Verify that `repo_path` is already on the target branch.
        let current_branch = detect_git_info_sync(&repo_path).ok_or_else(|| {
            GitError::OutputParse(format!(
                "Failed to detect current branch in {}",
                repo_path.display()
            ))
        })?;

        if current_branch != target_branch {
            return Err(GitError::CommandFailed {
                command: "git merge --squash".to_string(),
                stderr: format!(
                    "Cannot merge: repository is on '{current_branch}' but expected \
                     '{target_branch}'. Switch to '{target_branch}' first."
                ),
            });
        }

        run_git_command_sync(
            &repo_path,
            &["merge", "--squash", source_branch.as_str()],
            &format!("Failed to squash merge {source_branch}"),
        )?;

        // `git diff --cached --quiet` exits 0 when index matches `HEAD`.
        let cached_diff =
            run_git_command_output_sync(&repo_path, &["diff", "--cached", "--quiet"])?;

        if cached_diff.status.success() {
            return Ok(SquashMergeOutcome::AlreadyPresentInTarget);
        }

        if cached_diff.status.code() != Some(1) {
            let detail = command_output_detail(&cached_diff.stdout, &cached_diff.stderr);

            return Err(GitError::CommandFailed {
                command: "git diff --cached".to_string(),
                stderr: detail,
            });
        }

        run_git_command_sync(
            &repo_path,
            &["commit", "-m", commit_message.as_str()],
            "Failed to commit squash merge",
        )?;

        Ok(SquashMergeOutcome::Committed)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use mockall::Sequence;

    use super::*;

    /// Runs `git` in `repo_path` and asserts the command succeeds.
    fn run_git_command(repo_path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("failed to run git command");

        assert!(
            output.status.success(),
            "git command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Runs `git` in `repo_path` and returns trimmed stdout.
    fn run_git_stdout(repo_path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("failed to run git command");

        assert!(
            output.status.success(),
            "git command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Creates a committed repository rooted at `repo_path`.
    fn setup_test_git_repo(repo_path: &Path) {
        run_git_command(repo_path, &["init", "-b", "main"]);
        run_git_command(repo_path, &["config", "user.name", "Test User"]);
        run_git_command(repo_path, &["config", "user.email", "test@example.com"]);
        fs::write(repo_path.join("README.md"), "base\n").expect("failed to write base file");
        run_git_command(repo_path, &["add", "README.md"]);
        run_git_command(repo_path, &["commit", "-m", "Initial commit"]);
    }

    /// Creates diverged branches that edit the same line differently.
    fn setup_conflicting_branches(repo_path: &Path) {
        run_git_command(repo_path, &["checkout", "-b", "session-branch"]);
        fs::write(repo_path.join("README.md"), "session\n")
            .expect("failed to write session content");
        run_git_command(repo_path, &["add", "README.md"]);
        run_git_command(repo_path, &["commit", "-m", "Session change"]);
        run_git_command(repo_path, &["checkout", "main"]);
        fs::write(repo_path.join("README.md"), "main\n").expect("failed to write main content");
        run_git_command(repo_path, &["add", "README.md"]);
        run_git_command(repo_path, &["commit", "-m", "Main change"]);
    }

    /// Creates unrelated target and source branches that cannot be merged
    /// without an explicit unrelated-histories override.
    fn setup_unrelated_branches(repo_path: &Path) {
        run_git_command(repo_path, &["checkout", "--orphan", "unrelated-branch"]);
        run_git_command(repo_path, &["rm", "-rf", "."]);
        fs::write(repo_path.join("unrelated.txt"), "unrelated\n")
            .expect("failed to write unrelated content");
        run_git_command(repo_path, &["add", "unrelated.txt"]);
        run_git_command(repo_path, &["commit", "-m", "Unrelated change"]);
        run_git_command(repo_path, &["checkout", "main"]);
    }

    /// Adds one ordered checked-command result to a compatibility runner.
    fn expect_checked_command(
        command_runner: &mut MockCompatibilityMergeRunner,
        sequence: &mut Sequence,
        result: Result<String, GitError>,
    ) {
        command_runner
            .expect_run_git_command()
            .times(1)
            .in_sequence(sequence)
            .return_once(move |_, _, _| result);
    }

    /// Adds successful source and target resolution to a compatibility runner.
    fn expect_resolved_revisions(
        command_runner: &mut MockCompatibilityMergeRunner,
        sequence: &mut Sequence,
    ) {
        expect_checked_command(command_runner, sequence, Ok("source-commit\n".to_string()));
        expect_checked_command(command_runner, sequence, Ok("target-commit\n".to_string()));
    }

    /// Adds successful clone creation and target checkout to a compatibility
    /// runner.
    fn expect_prepared_clone(
        command_runner: &mut MockCompatibilityMergeRunner,
        sequence: &mut Sequence,
    ) {
        expect_checked_command(command_runner, sequence, Ok(String::new()));
        expect_checked_command(command_runner, sequence, Ok(String::new()));
    }

    /// Adds one ordered merge-command result to a compatibility runner.
    fn expect_merge_command(
        command_runner: &mut MockCompatibilityMergeRunner,
        sequence: &mut Sequence,
        result: Result<CompatibilityMergeOutput, GitError>,
    ) {
        command_runner
            .expect_run_git_command_output()
            .times(1)
            .in_sequence(sequence)
            .return_once(move |_, _| result);
    }

    /// Executes the compatibility probe through a configured mock runner.
    fn run_compatibility_probe_with_mock(
        command_runner: &MockCompatibilityMergeRunner,
    ) -> Result<bool, GitError> {
        let temporary_directory = tempdir().expect("failed to create temp dir");

        has_merge_conflicts_via_temporary_clone_with_runner(
            Path::new("repository"),
            "session-branch",
            "main",
            &temporary_directory,
            command_runner,
        )
    }

    /// Creates a deterministic command failure for compatibility-probe tests.
    fn compatibility_command_error(detail: &str) -> GitError {
        GitError::CommandFailed {
            command: "git compatibility-probe".to_string(),
            stderr: detail.to_string(),
        }
    }

    /// Returns merge output that requires an unmerged-file inspection.
    fn conflicting_merge_output() -> CompatibilityMergeOutput {
        CompatibilityMergeOutput {
            stderr: b"merge conflict".to_vec(),
            stdout: Vec::new(),
            success: false,
        }
    }

    #[test]
    fn classify_merge_tree_attempt_recognizes_legacy_git_synopsis() {
        // Arrange
        let stderr = b"error: unknown option `write-tree'\nusage: git merge-tree <base-tree> <branch1> <branch2>\n";

        // Act
        let attempt = classify_merge_tree_attempt(Some(129), b"", stderr);

        // Assert
        assert_eq!(attempt, MergeTreeAttempt::Unsupported);
    }

    #[test]
    fn classify_merge_tree_attempt_keeps_supported_usage_errors_failed() {
        // Arrange
        let stderr = b"error: unknown option `invalid'\nusage: git merge-tree [--write-tree] [<options>] <branch1> <branch2>\n";

        // Act
        let attempt = classify_merge_tree_attempt(Some(129), b"", stderr);

        // Assert
        assert_eq!(attempt, MergeTreeAttempt::Failed);
    }

    #[test]
    fn compatibility_probe_preserves_source_resolution_error() {
        // Arrange
        let mut command_runner = MockCompatibilityMergeRunner::new();
        let mut sequence = Sequence::new();
        expect_checked_command(
            &mut command_runner,
            &mut sequence,
            Err(compatibility_command_error("source resolution failed")),
        );

        // Act
        let error = run_compatibility_probe_with_mock(&command_runner)
            .expect_err("source resolution should fail the compatibility probe");

        // Assert
        assert_eq!(
            error.to_string(),
            "git compatibility-probe: source resolution failed"
        );
    }

    #[test]
    fn compatibility_probe_preserves_target_resolution_error() {
        // Arrange
        let mut command_runner = MockCompatibilityMergeRunner::new();
        let mut sequence = Sequence::new();
        expect_checked_command(
            &mut command_runner,
            &mut sequence,
            Ok("source-commit\n".to_string()),
        );
        expect_checked_command(
            &mut command_runner,
            &mut sequence,
            Err(compatibility_command_error("target resolution failed")),
        );

        // Act
        let error = run_compatibility_probe_with_mock(&command_runner)
            .expect_err("target resolution should fail the compatibility probe");

        // Assert
        assert_eq!(
            error.to_string(),
            "git compatibility-probe: target resolution failed"
        );
    }

    #[test]
    fn compatibility_probe_preserves_clone_creation_error() {
        // Arrange
        let mut command_runner = MockCompatibilityMergeRunner::new();
        let mut sequence = Sequence::new();
        expect_resolved_revisions(&mut command_runner, &mut sequence);
        expect_checked_command(
            &mut command_runner,
            &mut sequence,
            Err(compatibility_command_error("clone creation failed")),
        );

        // Act
        let error = run_compatibility_probe_with_mock(&command_runner)
            .expect_err("clone creation should fail the compatibility probe");

        // Assert
        assert_eq!(
            error.to_string(),
            "git compatibility-probe: clone creation failed"
        );
    }

    #[test]
    fn compatibility_probe_preserves_target_checkout_error() {
        // Arrange
        let mut command_runner = MockCompatibilityMergeRunner::new();
        let mut sequence = Sequence::new();
        expect_resolved_revisions(&mut command_runner, &mut sequence);
        expect_checked_command(&mut command_runner, &mut sequence, Ok(String::new()));
        expect_checked_command(
            &mut command_runner,
            &mut sequence,
            Err(compatibility_command_error("target checkout failed")),
        );

        // Act
        let error = run_compatibility_probe_with_mock(&command_runner)
            .expect_err("target checkout should fail the compatibility probe");

        // Assert
        assert_eq!(
            error.to_string(),
            "git compatibility-probe: target checkout failed"
        );
    }

    #[test]
    fn compatibility_probe_preserves_merge_execution_error() {
        // Arrange
        let mut command_runner = MockCompatibilityMergeRunner::new();
        let mut sequence = Sequence::new();
        expect_resolved_revisions(&mut command_runner, &mut sequence);
        expect_prepared_clone(&mut command_runner, &mut sequence);
        expect_merge_command(
            &mut command_runner,
            &mut sequence,
            Err(compatibility_command_error("merge execution failed")),
        );

        // Act
        let error = run_compatibility_probe_with_mock(&command_runner)
            .expect_err("merge execution should fail the compatibility probe");

        // Assert
        assert_eq!(
            error.to_string(),
            "git compatibility-probe: merge execution failed"
        );
    }

    #[test]
    fn compatibility_probe_preserves_unmerged_inspection_error() {
        // Arrange
        let mut command_runner = MockCompatibilityMergeRunner::new();
        let mut sequence = Sequence::new();
        expect_resolved_revisions(&mut command_runner, &mut sequence);
        expect_prepared_clone(&mut command_runner, &mut sequence);
        expect_merge_command(
            &mut command_runner,
            &mut sequence,
            Ok(conflicting_merge_output()),
        );
        expect_checked_command(
            &mut command_runner,
            &mut sequence,
            Err(compatibility_command_error("unmerged inspection failed")),
        );

        // Act
        let error = run_compatibility_probe_with_mock(&command_runner)
            .expect_err("unmerged inspection should fail the compatibility probe");

        // Assert
        assert_eq!(
            error.to_string(),
            "git compatibility-probe: unmerged inspection failed"
        );
    }

    #[test]
    fn compatibility_probe_returns_false_for_clean_merge() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "session-branch"]);
        fs::write(temp_dir.path().join("session.txt"), "session\n")
            .expect("failed to write session file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);

        // Act
        let has_conflicts = resolve_merge_tree_attempt(
            temp_dir.path(),
            "session-branch",
            "main",
            MergeTreeAttempt::Unsupported,
            b"",
            b"legacy git usage",
        )
        .expect("compatibility probe should succeed");

        // Assert
        assert!(!has_conflicts);
    }

    #[test]
    fn compatibility_probe_returns_true_for_conflicting_merge() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        setup_conflicting_branches(temp_dir.path());
        let original_head = run_git_stdout(temp_dir.path(), &["rev-parse", "HEAD"]);

        // Act
        let has_conflicts = resolve_merge_tree_attempt(
            temp_dir.path(),
            "session-branch",
            "main",
            MergeTreeAttempt::Unsupported,
            b"",
            b"legacy git usage",
        )
        .expect("compatibility probe should succeed");

        // Assert
        assert!(has_conflicts);
        assert_eq!(
            run_git_stdout(temp_dir.path(), &["rev-parse", "HEAD"]),
            original_head
        );
        assert!(run_git_stdout(temp_dir.path(), &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn compatibility_probe_returns_error_when_merge_cannot_start() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        setup_unrelated_branches(temp_dir.path());

        // Act
        let error = resolve_merge_tree_attempt(
            temp_dir.path(),
            "unrelated-branch",
            "main",
            MergeTreeAttempt::Unsupported,
            b"",
            b"legacy git usage",
        )
        .expect_err("unrelated histories should fail the compatibility probe");

        // Assert
        assert!(
            error
                .to_string()
                .contains("refusing to merge unrelated histories")
        );
    }

    #[tokio::test]
    async fn has_merge_conflicts_returns_false_for_clean_merge() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "session-branch"]);
        fs::write(temp_dir.path().join("session.txt"), "session\n")
            .expect("failed to write session file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);

        // Act
        let has_conflicts = has_merge_conflicts(
            temp_dir.path().to_path_buf(),
            "session-branch".to_string(),
            "main".to_string(),
        )
        .await
        .expect("merge conflict probe should succeed");

        // Assert
        assert!(!has_conflicts);
    }

    #[tokio::test]
    async fn has_merge_conflicts_returns_true_for_conflicting_merge() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        setup_conflicting_branches(temp_dir.path());

        // Act
        let has_conflicts = has_merge_conflicts(
            temp_dir.path().to_path_buf(),
            "session-branch".to_string(),
            "main".to_string(),
        )
        .await
        .expect("merge conflict probe should succeed");

        // Assert
        assert!(has_conflicts);
    }

    #[tokio::test]
    async fn has_merge_conflicts_returns_error_for_missing_branch() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());

        // Act
        let error = has_merge_conflicts(
            temp_dir.path().to_path_buf(),
            "missing-branch".to_string(),
            "main".to_string(),
        )
        .await
        .expect_err("missing branch should fail conflict detection");

        // Assert
        assert!(error.to_string().contains("git merge-tree"));
        assert!(error.to_string().contains("missing-branch"));
    }

    #[tokio::test]
    async fn has_merge_conflicts_returns_error_for_missing_repository() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let missing_repo = temp_dir.path().join("missing");

        // Act
        let error = has_merge_conflicts(
            missing_repo,
            "session-branch".to_string(),
            "main".to_string(),
        )
        .await
        .expect_err("missing repository should fail conflict detection");

        // Assert
        assert!(error.to_string().contains("git merge-tree"));
    }

    #[tokio::test]
    async fn squash_merge_returns_branch_mismatch_error_when_target_is_not_checked_out() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "feature-branch"]);

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "feature-branch".to_string(),
            "main".to_string(),
            "Merge feature".to_string(),
        )
        .await;

        // Assert
        let error = result.expect_err("branch mismatch should fail").to_string();
        assert!(error.contains("repository is on 'feature-branch'"));
        assert!(error.contains("Switch to 'main' first."));
    }

    #[tokio::test]
    async fn squash_merge_commits_the_provided_multiline_message() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "feature-branch"]);
        fs::write(temp_dir.path().join("feature.txt"), "feature content")
            .expect("failed to write feature file");
        run_git_command(temp_dir.path(), &["add", "feature.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Add feature"]);
        run_git_command(temp_dir.path(), &["checkout", "main"]);
        let commit_message = "Refine merge flow\n\n- Reuse the session commit body".to_string();

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "feature-branch".to_string(),
            "main".to_string(),
            commit_message.clone(),
        )
        .await;
        let head_message = run_git_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);

        // Assert
        assert_eq!(
            result.expect("squash merge should succeed"),
            SquashMergeOutcome::Committed,
        );
        assert_eq!(head_message, commit_message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn squash_merge_runs_pre_commit_hook() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "feature-branch"]);
        fs::write(temp_dir.path().join("feature.txt"), "feature content")
            .expect("failed to write feature file");
        run_git_command(temp_dir.path(), &["add", "feature.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Add feature"]);
        run_git_command(temp_dir.path(), &["checkout", "main"]);
        let hooks_dir = temp_dir.path().join("test-hooks");
        fs::create_dir(&hooks_dir).expect("failed to create hooks directory");
        let hook_path = hooks_dir.join("pre-commit");
        fs::write(&hook_path, "#!/bin/sh\necho hook-blocked >&2\nexit 1\n")
            .expect("failed to write pre-commit hook");
        let mut permissions = fs::metadata(&hook_path)
            .expect("failed to read hook metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook_path, permissions).expect("failed to make hook executable");
        run_git_command(temp_dir.path(), &["config", "core.hooksPath", "test-hooks"]);

        // Act
        let error = squash_merge(
            temp_dir.path().to_path_buf(),
            "feature-branch".to_string(),
            "main".to_string(),
            "Squash merge feature".to_string(),
        )
        .await
        .expect_err("pre-commit hook should block the squash commit");

        // Assert
        assert!(error.to_string().contains("hook-blocked"));
    }

    #[tokio::test]
    async fn squash_merge_skips_commit_creation_when_changes_are_already_present() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "session-branch"]);
        fs::write(temp_dir.path().join("session.txt"), "session change")
            .expect("failed to write session file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);
        run_git_command(temp_dir.path(), &["checkout", "main"]);
        fs::write(temp_dir.path().join("session.txt"), "session change")
            .expect("failed to write main file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(
            temp_dir.path(),
            &["commit", "-m", "Apply same change on main"],
        );
        let commit_count_before = run_git_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]);
        let head_message_before = run_git_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "session-branch".to_string(),
            "main".to_string(),
            "Merge session".to_string(),
        )
        .await;
        let commit_count_after = run_git_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]);
        let head_message_after = run_git_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);

        // Assert
        assert_eq!(
            result.expect("squash merge should succeed"),
            SquashMergeOutcome::AlreadyPresentInTarget,
        );
        assert_eq!(commit_count_after, commit_count_before);
        assert_eq!(head_message_after, head_message_before);
    }

    #[tokio::test]
    async fn squash_merge_returns_command_detail_for_missing_source_branch() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "missing-branch".to_string(),
            "main".to_string(),
            "Merge feature".to_string(),
        )
        .await;

        // Assert
        let error = result.expect_err("missing branch should fail").to_string();
        assert!(error.contains("Failed to squash merge missing-branch"));
        assert!(error.contains("missing-branch"));
    }
}
