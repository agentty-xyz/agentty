//! Repositories a session leaves inside its worktree, for example by moving
//! one it built in its temporary directory: host staging, status, and diffs
//! never record them as gitlinks nor run anything their configuration names.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::{TempDir, tempdir};

use super::support::{
    git_command_stdout, run_git_command, setup_test_git_repo, write_executable_hook,
};
use crate::client::{GitClient, RealGitClient};

/// A committed repository holding one embedded repository at `path`.
struct EmbeddedRepository {
    directory: TempDir,
    path: &'static str,
}

impl EmbeddedRepository {
    /// Creates the repository and embeds a committed repository at `path`.
    fn new(path: &'static str) -> Self {
        let directory = tempdir().expect("failed to create temp dir");
        let fixture = Self { directory, path };
        let root = fixture.root();
        fs::create_dir(&root).expect("failed to create repository");
        setup_test_git_repo(&root);
        let embedded = fixture.embedded();
        fs::create_dir_all(&embedded).expect("failed to create embedded repository");
        setup_test_git_repo(&embedded);

        fixture
    }

    /// Returns the outer repository.
    fn root(&self) -> PathBuf {
        self.directory.path().join("repository")
    }

    /// Returns the embedded repository.
    fn embedded(&self) -> PathBuf {
        self.root().join(self.path)
    }

    /// Returns the file the embedded repository's `core.fsmonitor` creates.
    fn marker(&self) -> PathBuf {
        self.directory.path().join("fsmonitor-ran")
    }

    /// Points the embedded repository's `core.fsmonitor` at a command that
    /// records that it ran, and leaves a change there for Git to look for.
    fn arm(&self) {
        let command = self.directory.path().join("fsmonitor.sh");
        write_executable_hook(
            &command,
            &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", self.marker().display()),
        );
        run_git_command(
            &self.embedded(),
            &["config", "core.fsmonitor", &command.display().to_string()],
        );
        fs::write(self.embedded().join("README.md"), "dirty\n")
            .expect("failed to change embedded file");
    }

    /// Runs every host read and write that touches the worktree.
    async fn run_host_git(&self) {
        let client = RealGitClient;
        let root = self.root();
        client.stage_all(root.clone()).await.expect("stage all");
        client
            .worktree_status(root.clone())
            .await
            .expect("worktree status");
        client
            .tracked_worktree_status(root.clone())
            .await
            .expect("tracked status");
        client
            .diff(root.clone(), "main".to_string())
            .await
            .expect("diff");
        client
            .diff_stats(root.clone(), "main".to_string())
            .await
            .expect("diff stats");
        client
            .diff_changed_files(root, "main".to_string())
            .await
            .expect("changed files");
    }
}

/// Returns the index entries of `repository` as `mode path` lines.
fn index_entries(repository: &Path) -> String {
    git_command_stdout(repository, &["ls-files", "--format=%(objectmode) %(path)"])
}

#[tokio::test]
async fn untracked_embedded_repositories_are_never_staged_or_inspected() {
    // Arrange
    let fixture = EmbeddedRepository::new("docs/planted");
    let root = fixture.root();
    fs::write(root.join("notes.txt"), "notes\n").expect("failed to write notes");
    fixture.arm();

    // Act
    fixture.run_host_git().await;
    RealGitClient
        .commit_all(root.clone(), "Add notes".to_string())
        .await
        .expect("commit");

    // Assert
    assert!(!fixture.marker().exists(), "the embedded fsmonitor ran");
    assert_eq!(index_entries(&root), "100644 README.md\n100644 notes.txt");
    assert_eq!(
        git_command_stdout(
            &root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignore-submodules=all"
            ]
        ),
        "?? docs/planted/"
    );
}

#[tokio::test]
async fn tracked_gitlinks_are_neither_restaged_nor_inspected() {
    // Arrange
    let fixture = EmbeddedRepository::new("vendor/library");
    let root = fixture.root();
    run_git_command(&root, &["add", "vendor/library"]);
    run_git_command(&root, &["commit", "-m", "Track library"]);
    let recorded = git_command_stdout(&root, &["rev-parse", "HEAD:vendor/library"]);
    run_git_command(
        &fixture.embedded(),
        &["commit", "--allow-empty", "-m", "Move library"],
    );
    fixture.arm();

    // Act
    fixture.run_host_git().await;

    // Assert
    assert!(!fixture.marker().exists(), "the embedded fsmonitor ran");
    assert_eq!(
        git_command_stdout(&root, &["rev-parse", ":vendor/library"]),
        recorded
    );
}
