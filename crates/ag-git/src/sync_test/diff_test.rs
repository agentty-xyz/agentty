//! Diff and branch-deletion tests covering staged and untracked changes,
//! linked worktrees, diff targets, and repository-unavailable errors.

use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;

use super::support::{git_command_output, run_git_command, setup_test_git_repo};
use crate::error::GitError;
use crate::sync::{
    DiffOutputFormat, DiffStats, classify_diff_repository_error, delete_branch, diff,
    diff_changed_files, diff_output_after_index_resolution, diff_repository_probe_is_unavailable,
    diff_stats, last_leading_applied_commit, resolve_diff_index_path, resolve_diff_target,
};

/// Counts patch body lines with the `+++`/`---` header heuristic of
/// `ag_contracts::SessionStats::line_change_counts`, which numstat replaces.
fn patch_line_totals(patch: &str) -> (u64, u64) {
    let added_lines = patch
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count();
    let deleted_lines = patch
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count();

    (
        u64::try_from(added_lines).expect("added line count should fit"),
        u64::try_from(deleted_lines).expect("deleted line count should fit"),
    )
}

#[tokio::test]
async fn delete_branch_removes_branch_from_isolated_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["branch", "review/topic"]);

    // Act
    delete_branch(temp_dir.path().to_path_buf(), "review/topic".to_string())
        .await
        .expect("branch deletion should succeed");

    // Assert
    let branch_lookup = git_command_output(
        temp_dir.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/review/topic"],
    );
    assert!(!branch_lookup.status.success());
}

#[tokio::test]
async fn diff_preserves_staged_changes_and_includes_untracked_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(temp_dir.path().join("README.md"), "staged change\n")
        .expect("failed to write staged change");
    run_git_command(temp_dir.path(), &["add", "README.md"]);
    fs::write(
        temp_dir.path().join("README.md"),
        "staged change\nunstaged change\n",
    )
    .expect("failed to write unstaged change");
    fs::write(temp_dir.path().join("new.txt"), "untracked change\n")
        .expect("failed to write untracked file");
    let cached_diff_before = git_command_output(temp_dir.path(), &["diff", "--cached"]).stdout;
    let status_before = git_command_output(
        temp_dir.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .stdout;

    // Act
    let result = diff(temp_dir.path().to_path_buf(), "main".to_string()).await;
    let changed_files = diff_changed_files(temp_dir.path().to_path_buf(), "main".to_string()).await;

    // Assert
    let diff_output = result.expect("diff should succeed");
    let cached_diff_after = git_command_output(temp_dir.path(), &["diff", "--cached"]).stdout;
    let status_after = git_command_output(
        temp_dir.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .stdout;
    assert!(diff_output.contains("staged change"));
    assert!(diff_output.contains("unstaged change"));
    assert!(diff_output.contains("untracked change"));
    assert_eq!(
        changed_files.expect("changed files should load"),
        vec!["README.md".to_string(), "new.txt".to_string()]
    );
    assert_eq!(cached_diff_after, cached_diff_before);
    assert_eq!(status_after, status_before);
}

#[tokio::test]
async fn diff_stats_match_patch_totals_for_every_change_kind() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path();
    setup_test_git_repo(repo_path);
    fs::write(repo_path.join("modified.txt"), "one\ntwo\nthree\n")
        .expect("failed to write modified file");
    fs::write(repo_path.join("deleted.txt"), "gone\nalso gone\n")
        .expect("failed to write deleted file");
    fs::write(repo_path.join("renamed.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n")
        .expect("failed to write renamed file");
    fs::write(repo_path.join("image.bin"), [0_u8, 1, 2, 3]).expect("failed to write binary file");
    fs::write(repo_path.join("script.sh"), "echo\n").expect("failed to write script file");
    run_git_command(repo_path, &["add", "."]);
    run_git_command(repo_path, &["commit", "-m", "Add fixtures"]);
    run_git_command(repo_path, &["checkout", "-b", "session"]);
    fs::write(repo_path.join("modified.txt"), "one\nTWO\nthree\n")
        .expect("failed to write committed change");
    run_git_command(repo_path, &["commit", "-am", "Session change"]);
    run_git_command(repo_path, &["checkout", "main"]);
    fs::write(repo_path.join("upstream.txt"), "upstream only\n")
        .expect("failed to write upstream file");
    run_git_command(repo_path, &["add", "upstream.txt"]);
    run_git_command(repo_path, &["commit", "-m", "Advance main"]);
    run_git_command(repo_path, &["checkout", "session"]);
    fs::write(repo_path.join("modified.txt"), "one\nTWO\nthree\nfour\n")
        .expect("failed to write unstaged change");
    fs::remove_file(repo_path.join("deleted.txt")).expect("failed to delete file");
    run_git_command(repo_path, &["mv", "renamed.txt", "moved.txt"]);
    fs::write(repo_path.join("moved.txt"), "1\n2\n3\n4\n5\n6\n7\n9\n")
        .expect("failed to edit renamed file");
    fs::write(repo_path.join("image.bin"), [0_u8, 9, 9, 9]).expect("failed to edit binary file");
    // Without filesystem modes, Git takes the worktree mode from the index.
    run_git_command(repo_path, &["config", "core.fileMode", "false"]);
    run_git_command(repo_path, &["update-index", "--chmod=+x", "script.sh"]);
    fs::write(repo_path.join("untracked.txt"), "u\nv\n").expect("failed to write untracked file");
    fs::write(repo_path.join("empty.txt"), "").expect("failed to write empty file");

    // Act
    let patch = diff(repo_path.to_path_buf(), "main".to_string())
        .await
        .expect("diff should succeed");
    let stats = diff_stats(repo_path.to_path_buf(), "main".to_string())
        .await
        .expect("diff stats should succeed");

    // Assert
    assert!(patch.contains("rename to moved.txt"));
    assert!(patch.contains("Binary files a/image.bin and b/image.bin differ"));
    assert!(patch.contains("new mode 100755"));
    assert!(patch.contains("diff --git a/empty.txt b/empty.txt"));
    assert!(!patch.contains("upstream.txt"));
    assert_eq!(
        (stats.added_lines, stats.deleted_lines),
        patch_line_totals(&patch)
    );
    assert_eq!(
        stats,
        DiffStats {
            added_lines: 5,
            deleted_lines: 4,
            has_changes: true,
        }
    );
}

#[tokio::test]
async fn diff_stats_report_no_changes_for_clean_worktree() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());

    // Act
    let stats = diff_stats(temp_dir.path().to_path_buf(), "main".to_string()).await;

    // Assert
    assert_eq!(
        stats.expect("diff stats should succeed"),
        DiffStats::default()
    );
}

#[tokio::test]
async fn diff_stats_count_content_lines_that_resemble_patch_headers() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(temp_dir.path().join("README.md"), "++counter\n")
        .expect("failed to write header-like change");

    // Act
    let stats = diff_stats(temp_dir.path().to_path_buf(), "main".to_string()).await;

    // Assert
    assert_eq!(
        stats.expect("diff stats should succeed"),
        DiffStats {
            added_lines: 1,
            deleted_lines: 1,
            has_changes: true,
        }
    );
}

#[test]
fn diff_stats_reject_malformed_numstat_rows() {
    // Arrange
    let rows = ["garbage", "1\t2", "x\t1\tpath", "1\tx\tpath"];

    for row in rows {
        // Act
        let result = DiffStats::from_numstat(row);

        // Assert
        assert!(
            matches!(result, Err(GitError::OutputParse(detail)) if detail.contains(row)),
            "{row:?}"
        );
    }
}

#[tokio::test]
async fn diff_reads_linked_worktree_through_its_absolute_index_path() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_path = temp_dir.path().join("repo");
    let worktree_path = temp_dir.path().join("session");
    fs::create_dir(&repo_path).expect("failed to create repository directory");
    setup_test_git_repo(&repo_path);
    let worktree_path_text = worktree_path.to_string_lossy().into_owned();
    run_git_command(
        &repo_path,
        &["worktree", "add", "-b", "session", &worktree_path_text],
    );
    fs::write(worktree_path.join("session.txt"), "session change\n")
        .expect("failed to write session file");
    let index_path =
        resolve_diff_index_path(&worktree_path).expect("worktree index path should resolve");

    // Act
    let result = diff(worktree_path.clone(), "main".to_string()).await;

    // Assert
    assert!(PathBuf::from(index_path.trim()).is_absolute());
    assert!(
        result
            .expect("linked worktree diff should succeed")
            .contains("+session change")
    );
}

#[test]
fn diff_target_falls_back_to_merge_base_when_cherry_fails() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());

    // Act
    let target = resolve_diff_target(temp_dir.path(), "missing-base-reference", "merge-base");

    // Assert
    assert_eq!(
        target.expect("cherry failure should fall back"),
        "merge-base"
    );
}

#[test]
fn last_leading_applied_commit_reads_only_the_leading_applied_block() {
    // Arrange
    let cases = [
        ("", None),
        ("\n- aaa\n- bbb\n+ ccc\n- ddd\n", Some("bbb")),
        ("- aaa\n? bbb\n- ccc\n", Some("aaa")),
        ("+ aaa\n- bbb\n", None),
    ];

    for (cherry_output, expected) in cases {
        // Act
        let commit = last_leading_applied_commit(cherry_output);

        // Assert
        assert_eq!(commit, expected, "{cherry_output:?}");
    }
}

#[tokio::test]
async fn diff_reports_repository_unavailable_outside_git_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = diff(temp_dir.path().to_path_buf(), "main".to_string()).await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::RepositoryUnavailable { detail })
            if detail.to_ascii_lowercase().contains("not a git repository")
    ));
}

#[test]
fn diff_reports_repository_unavailable_when_removed_after_index_resolution() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let preserved_index_dir = tempdir().expect("failed to create preserved index dir");
    setup_test_git_repo(temp_dir.path());
    let index_path = resolve_diff_index_path(temp_dir.path()).expect("index path should resolve");
    let index_path = PathBuf::from(index_path.trim());
    let index_path = temp_dir.path().join(index_path);
    let preserved_index_path = preserved_index_dir.path().join("index");
    fs::copy(index_path, &preserved_index_path).expect("index copy should succeed");
    fs::remove_dir_all(temp_dir.path()).expect("worktree removal should succeed");

    // Act
    let result = diff_output_after_index_resolution(
        temp_dir.path(),
        "main",
        DiffOutputFormat::Patch,
        &preserved_index_path,
    );

    // Assert
    assert!(matches!(
        result,
        Err(GitError::RepositoryUnavailable { detail })
            if detail.contains("git add -A --intent-to-add")
    ));
}

#[tokio::test]
async fn diff_preserves_invalid_base_reference_error() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());

    // Act
    let result = diff(
        temp_dir.path().to_path_buf(),
        "missing-base-reference".to_string(),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { command, stderr })
            if command.contains("missing-base-reference")
                && stderr.contains("Git diff failed")
    ));
}

#[test]
fn diff_repository_error_classification_preserves_unrelated_failures() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    let error = GitError::CommandFailed {
        command: "git rev-parse --git-path index".to_string(),
        stderr: "fatal: ambiguous argument".to_string(),
    };

    // Act
    let classified = classify_diff_repository_error(temp_dir.path(), error);

    // Assert
    assert!(matches!(
        classified,
        GitError::CommandFailed { command, stderr }
            if command == "git rev-parse --git-path index"
                && stderr == "fatal: ambiguous argument"
    ));
}

#[test]
fn diff_repository_error_classification_ignores_localized_diagnostic() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let error = GitError::CommandFailed {
        command: "git rev-parse --git-path index".to_string(),
        stderr: "fatal: kein Git-Repository".to_string(),
    };

    // Act
    let classified = classify_diff_repository_error(temp_dir.path(), error);

    // Assert
    assert!(matches!(
        classified,
        GitError::RepositoryUnavailable { detail }
            if detail == "git rev-parse --git-path index: fatal: kein Git-Repository"
    ));
}

#[test]
fn diff_repository_probe_preserves_spawn_failure() {
    // Arrange
    let probe = Err(GitError::CommandFailed {
        command: "git rev-parse --git-dir".to_string(),
        stderr: "git executable unavailable".to_string(),
    });

    // Act
    let unavailable = diff_repository_probe_is_unavailable(probe);

    // Assert
    assert!(!unavailable);
}

#[test]
fn diff_repository_error_classification_types_missing_directory() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let missing_path = temp_dir.path().join("removed-worktree");
    let error = GitError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "worktree removed",
    ));

    // Act
    let classified = classify_diff_repository_error(&missing_path, error);

    // Assert
    assert!(matches!(
        classified,
        GitError::RepositoryUnavailable { detail } if detail == "worktree removed"
    ));
}
