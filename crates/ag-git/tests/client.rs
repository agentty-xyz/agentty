//! `RealGitClient` delegates every operation to Git with observable effects.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ag_git::{
    GitClient, GitError, GitFuture, RealGitClient, RebaseStepResult, SingleCommitMessageStrategy,
};
use tempfile::tempdir;

/// Fallible fixture result that can cross the `Send` client futures.
type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// One client operation driven against a freshly prepared repository.
struct ContractCase {
    /// Expected normalized observation of the operation and its effects.
    expected: &'static str,
    /// Runs the client operation and renders its result and effects.
    observe: fn(ContractRepository) -> GitFuture<String>,
    /// Trait method under test, reported when the observation differs.
    operation: &'static str,
    /// Moves the standard repository into the state the operation needs.
    prepare: fn(&ContractRepository) -> TestResult<()>,
}

/// Paths of the standard repository layout used by every contract case.
///
/// `local` is checked out on `main`, tracks `origin/main` in the bare
/// `origin` repository, and has an unpushed `feature` branch that adds
/// `feature.txt`.
#[derive(Clone)]
struct ContractRepository {
    local: PathBuf,
    origin: PathBuf,
    root: PathBuf,
}

impl ContractRepository {
    /// Builds the standard layout under `root` with Git configuration pinned
    /// away from host settings.
    fn create(root: &Path) -> TestResult<Self> {
        let root = fs::canonicalize(root)?;
        let local = root.join("local");
        let origin = root.join("origin");
        fs::create_dir(&local)?;
        fs::create_dir(&origin)?;
        let repository = Self {
            local,
            origin,
            root,
        };
        let no_hooks_path = repository.root.join("no-hooks");
        let no_hooks_path = no_hooks_path.to_str().ok_or("hooks path is not UTF-8")?;
        let origin_path = repository
            .origin
            .to_str()
            .ok_or("origin path is not UTF-8")?;

        run_git(
            &repository.origin,
            &["init", "--bare", "--quiet", "-b", "main"],
        )?;
        repository.git(&["init", "--quiet", "-b", "main"])?;
        repository.git(&["config", "user.name", "Test User"])?;
        repository.git(&["config", "user.email", "test@example.com"])?;
        repository.git(&["config", "commit.gpgsign", "false"])?;
        repository.git(&["config", "core.hooksPath", no_hooks_path])?;
        repository.commit_file("README.md", "base\n", "Initial commit")?;
        repository.git(&["remote", "add", "origin", origin_path])?;
        repository.git(&["push", "--quiet", "-u", "origin", "main"])?;
        repository.git(&["checkout", "--quiet", "-b", "feature"])?;
        repository.commit_file("feature.txt", "feature\n", "Add feature")?;
        repository.git(&["checkout", "--quiet", "main"])?;

        Ok(repository)
    }

    /// Returns the path of a sibling entry of `local` and `origin`.
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Renders `path` relative to the fixture root so observations stay
    /// independent of the temporary directory location.
    fn relative(&self, path: &Path) -> String {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        match path.strip_prefix(&self.root) {
            Ok(relative_path) => relative_path.display().to_string(),
            Err(_) => path.display().to_string(),
        }
    }

    /// Runs `git` in `local` and returns trimmed stdout.
    fn git(&self, arguments: &[&str]) -> TestResult<String> {
        run_git(&self.local, arguments)
    }

    /// Runs `git` in `local` for an observation, rendering failures inline
    /// so they surface as a mismatched observation.
    fn observe_git(&self, arguments: &[&str]) -> String {
        render_git_observation(self.git(arguments))
    }

    /// Writes `content` to `file_name` in `local` and commits it.
    fn commit_file(&self, file_name: &str, content: &str, message: &str) -> TestResult<()> {
        self.write(file_name, content)?;
        self.git(&["add", file_name])?;
        self.git(&["commit", "--quiet", "-m", message])?;

        Ok(())
    }

    /// Writes `content` to `file_name` in `local` without staging it.
    fn write(&self, file_name: &str, content: &str) -> TestResult<()> {
        fs::write(self.local.join(file_name), content)?;

        Ok(())
    }

    /// Returns the subject of the commit at `revision` in `local`.
    fn subject(&self, revision: &str) -> String {
        self.observe_git(&["log", "-1", "--format=%s", revision])
    }

    /// Returns whether `branch` in `origin` points at the local `HEAD` commit.
    fn remote_matches_head(&self, branch: &str) -> String {
        let remote_reference = format!("refs/heads/{branch}");
        let remote_head =
            render_git_observation(run_git(&self.origin, &["rev-parse", &remote_reference]));

        (remote_head == self.observe_git(&["rev-parse", "HEAD"])).to_string()
    }
}

/// Leaves the standard repository layout unchanged.
const KEEP_STANDARD_LAYOUT: fn(&ContractRepository) -> TestResult<()> = |_repository| Ok(());

/// Operations that inspect or rewrite history inside one checkout.
const LOCAL_HISTORY_CASES: &[ContractCase] = &[
    ContractCase {
        operation: "detect_git_info",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let branch = RealGitClient.detect_git_info(repository.local).await;

                format!("{branch:?}")
            })
        },
        expected: r#"Some("main")"#,
    },
    ContractCase {
        operation: "find_git_repo_root",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let root = RealGitClient
                    .find_git_repo_root(repository.local.clone())
                    .await
                    .map(|root| repository.relative(&root));

                format!("{root:?}")
            })
        },
        expected: r#"Some("local")"#,
    },
    ContractCase {
        operation: "squash_merge_diff",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let changed_files = RealGitClient
                    .squash_merge_diff(repository.local, "feature".to_string(), "main".to_string())
                    .await
                    .map(|diff| {
                        diff.lines()
                            .filter(|line| line.starts_with("+++ "))
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    });

                format!("{changed_files:?}")
            })
        },
        expected: r#"Ok(["+++ b/feature.txt"])"#,
    },
    ContractCase {
        operation: "squash_merge",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let outcome = RealGitClient
                    .squash_merge(
                        repository.local.clone(),
                        "feature".to_string(),
                        "main".to_string(),
                        "Squash feature".to_string(),
                    )
                    .await;

                format!("{outcome:?} subject={}", repository.subject("HEAD"))
            })
        },
        expected: "Ok(Committed) subject=Squash feature",
    },
    ContractCase {
        operation: "commit_all_preserving_single_commit",
        prepare: |repository| {
            repository.git(&["checkout", "--quiet", "feature"])?;

            repository.write("work.txt", "work\n")
        },
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient
                    .commit_all_preserving_single_commit(
                        repository.local.clone(),
                        "main".to_string(),
                        "Session commit".to_string(),
                        SingleCommitMessageStrategy::Replace,
                    )
                    .await;
                let session_commits =
                    repository.observe_git(&["rev-list", "--count", "main..HEAD"]);

                format!(
                    "{result:?} commits={session_commits} subject={}",
                    repository.subject("HEAD")
                )
            })
        },
        expected: "Ok(()) commits=1 subject=Session commit",
    },
    ContractCase {
        operation: "stage_all",
        prepare: |repository| repository.write("new.txt", "new\n"),
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient.stage_all(repository.local.clone()).await;
                let staged = repository.observe_git(&["diff", "--cached", "--name-only"]);

                format!("{result:?} staged={staged}")
            })
        },
        expected: "Ok(()) staged=new.txt",
    },
    ContractCase {
        operation: "head_short_hash",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let expected_hash = repository.observe_git(&["rev-parse", "--short", "HEAD"]);
                let matches_head = RealGitClient
                    .head_short_hash(repository.local)
                    .await
                    .map(|hash| hash == expected_hash);

                format!("{matches_head:?}")
            })
        },
        expected: "Ok(true)",
    },
    ContractCase {
        operation: "head_hash",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let expected_hash = repository.observe_git(&["rev-parse", "HEAD"]);
                let matches_head = RealGitClient
                    .head_hash(repository.local)
                    .await
                    .map(|hash| hash == expected_hash);

                format!("{matches_head:?}")
            })
        },
        expected: "Ok(true)",
    },
    ContractCase {
        operation: "head_commit_message",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let message = RealGitClient.head_commit_message(repository.local).await;

                format!("{message:?}")
            })
        },
        expected: r#"Ok(Some("Initial commit"))"#,
    },
    ContractCase {
        operation: "delete_branch",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient
                    .delete_branch(repository.local.clone(), "feature".to_string())
                    .await;
                let branches = repository.observe_git(&["branch", "--format=%(refname:short)"]);

                format!("{result:?} branches={branches}")
            })
        },
        expected: "Ok(()) branches=main",
    },
    ContractCase {
        operation: "is_worktree_clean",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let is_clean = RealGitClient.is_worktree_clean(repository.local).await;

                format!("{is_clean:?}")
            })
        },
        expected: "Ok(true)",
    },
    ContractCase {
        operation: "worktree_status",
        prepare: |repository| repository.write("new.txt", "new\n"),
        observe: |repository| {
            Box::pin(async move {
                let status = RealGitClient
                    .worktree_status(repository.local)
                    .await
                    .map(|status| status.trim().to_string());

                format!("{status:?}")
            })
        },
        expected: r#"Ok("?? new.txt")"#,
    },
    ContractCase {
        operation: "tracked_worktree_status",
        prepare: |repository| {
            repository.write("README.md", "changed\n")?;

            repository.write("new.txt", "new\n")
        },
        observe: |repository| {
            Box::pin(async move {
                let status = RealGitClient
                    .tracked_worktree_status(repository.local)
                    .await
                    .map(|status| status.trim().to_string());

                format!("{status:?}")
            })
        },
        expected: r#"Ok("M README.md")"#,
    },
    ContractCase {
        operation: "has_commits_since",
        prepare: |repository| {
            repository.git(&["checkout", "--quiet", "feature"])?;

            Ok(())
        },
        observe: |repository| {
            Box::pin(async move {
                let has_commits = RealGitClient
                    .has_commits_since(repository.local, "main".to_string())
                    .await;

                format!("{has_commits:?}")
            })
        },
        expected: "Ok(true)",
    },
    ContractCase {
        operation: "get_ref_ahead_behind",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let counts = RealGitClient
                    .get_ref_ahead_behind(
                        repository.local,
                        "feature".to_string(),
                        "main".to_string(),
                    )
                    .await;

                format!("{counts:?}")
            })
        },
        expected: "Ok((1, 0))",
    },
];

/// Operations that manage linked worktrees or drive rebases and conflicts.
const WORKTREE_AND_REBASE_CASES: &[ContractCase] = &[
    ContractCase {
        operation: "create_worktree",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let worktree_path = repository.path("session");
                let result = RealGitClient
                    .create_worktree(
                        repository.local,
                        worktree_path.clone(),
                        "session".to_string(),
                        "main".to_string(),
                    )
                    .await;
                let branch =
                    render_git_observation(run_git(&worktree_path, &["branch", "--show-current"]));

                format!("{result:?} branch={branch}")
            })
        },
        expected: "Ok(()) branch=session",
    },
    ContractCase {
        operation: "remove_worktree",
        prepare: add_linked_worktree,
        observe: |repository| {
            Box::pin(async move {
                let worktree_path = repository.path("session");
                let result = RealGitClient.remove_worktree(worktree_path.clone()).await;

                format!("{result:?} exists={}", worktree_path.exists())
            })
        },
        expected: "Ok(()) exists=false",
    },
    ContractCase {
        operation: "main_repo_root",
        prepare: add_linked_worktree,
        observe: |repository| {
            Box::pin(async move {
                let root = RealGitClient
                    .main_repo_root(repository.path("session"))
                    .await
                    .map(|root| repository.relative(&root));

                format!("{root:?}")
            })
        },
        expected: r#"Ok("local")"#,
    },
    ContractCase {
        operation: "main_checkout_working_tree",
        prepare: add_linked_worktree,
        observe: |repository| {
            Box::pin(async move {
                let checkout = RealGitClient
                    .main_checkout_working_tree(repository.path("session"))
                    .await
                    .map(|checkout| checkout.map(|path| repository.relative(&path)));

                format!("{checkout:?}")
            })
        },
        expected: r#"Ok(Some("local"))"#,
    },
    ContractCase {
        operation: "rebase",
        prepare: diverge_feature_from_main,
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient
                    .rebase(repository.local.clone(), "main".to_string())
                    .await;

                format!("{result:?} parent={}", repository.subject("HEAD~1"))
            })
        },
        expected: "Ok(()) parent=Advance main",
    },
    ContractCase {
        operation: "rebase",
        prepare: diverge_conflicting_branch,
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient
                    .rebase(repository.local.clone(), "main".to_string())
                    .await;
                let in_progress = RealGitClient
                    .is_rebase_in_progress(repository.local.clone())
                    .await;

                format!(
                    "{} rebasing={in_progress:?} subject={}",
                    render_outcome(result),
                    repository.subject("HEAD")
                )
            })
        },
        expected: "Err(git rebase: Failed to rebase onto main) rebasing=Ok(false) \
                   subject=Conflict edit",
    },
    ContractCase {
        operation: "rebase_start",
        prepare: diverge_conflicting_branch,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_start(repository.local.clone(), "main".to_string())
                    .await;
                let in_progress = RealGitClient.is_rebase_in_progress(repository.local).await;

                format!("{} rebasing={in_progress:?}", render_rebase_step(step))
            })
        },
        expected: "Ok(Conflict) detail=true rebasing=Ok(true)",
    },
    ContractCase {
        operation: "rebase_start",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_start(repository.local, "missing".to_string())
                    .await;

                render_rebase_step(step)
            })
        },
        expected: "Err(git rebase: Failed to rebase onto missing)",
    },
    ContractCase {
        operation: "rebase_onto_start",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_onto_start(repository.local, "missing".to_string(), "main".to_string())
                    .await;

                render_rebase_step(step)
            })
        },
        expected: "Err(git rebase --onto: Failed to rebase onto missing after main)",
    },
    ContractCase {
        operation: "rebase_continue",
        prepare: start_conflicting_rebase,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_continue(repository.local.clone())
                    .await;
                let in_progress = RealGitClient.is_rebase_in_progress(repository.local).await;

                format!("{} rebasing={in_progress:?}", render_rebase_step(step))
            })
        },
        expected: "Ok(Conflict) detail=true rebasing=Ok(true)",
    },
    ContractCase {
        operation: "rebase_continue",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient.rebase_continue(repository.local).await;

                render_rebase_step(step)
            })
        },
        expected: "Err(git rebase --continue: Failed to continue rebase)",
    },
    ContractCase {
        operation: "rebase_start",
        prepare: diverge_feature_from_main,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_start(repository.local.clone(), "main".to_string())
                    .await;

                format!("{step:?} parent={}", repository.subject("HEAD~1"))
            })
        },
        expected: "Ok(Completed) parent=Advance main",
    },
    ContractCase {
        operation: "rebase_onto_start",
        prepare: diverge_feature_from_main,
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_onto_start(
                        repository.local.clone(),
                        "main".to_string(),
                        "feature~1".to_string(),
                    )
                    .await;

                format!("{step:?} parent={}", repository.subject("HEAD~1"))
            })
        },
        expected: "Ok(Completed) parent=Advance main",
    },
    ContractCase {
        operation: "rebase_continue",
        prepare: |repository| {
            start_conflicting_rebase(repository)?;
            repository.write("README.md", "resolved\n")?;
            repository.git(&["add", "README.md"])?;

            Ok(())
        },
        observe: |repository| {
            Box::pin(async move {
                let step = RealGitClient
                    .rebase_continue(repository.local.clone())
                    .await;

                format!("{step:?} subject={}", repository.subject("HEAD"))
            })
        },
        expected: "Ok(Completed) subject=Conflict edit",
    },
    ContractCase {
        operation: "abort_rebase",
        prepare: start_conflicting_rebase,
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient.abort_rebase(repository.local.clone()).await;
                let branch = repository.observe_git(&["branch", "--show-current"]);

                format!("{result:?} branch={branch}")
            })
        },
        expected: "Ok(()) branch=conflict",
    },
    ContractCase {
        operation: "is_rebase_in_progress",
        prepare: start_conflicting_rebase,
        observe: |repository| {
            Box::pin(async move {
                let in_progress = RealGitClient.is_rebase_in_progress(repository.local).await;

                format!("{in_progress:?}")
            })
        },
        expected: "Ok(true)",
    },
    ContractCase {
        operation: "in_progress_operation",
        prepare: start_conflicting_rebase,
        observe: |repository| {
            Box::pin(async move {
                let operation = RealGitClient.in_progress_operation(repository.local).await;

                format!("{operation:?}")
            })
        },
        expected: "Ok(Some(Rebase))",
    },
    ContractCase {
        operation: "has_unmerged_paths",
        prepare: start_conflicting_rebase,
        observe: |repository| {
            Box::pin(async move {
                let has_unmerged = RealGitClient.has_unmerged_paths(repository.local).await;

                format!("{has_unmerged:?}")
            })
        },
        expected: "Ok(true)",
    },
    ContractCase {
        operation: "list_conflicted_files",
        prepare: start_conflicting_rebase,
        observe: |repository| {
            Box::pin(async move {
                let files = RealGitClient.list_conflicted_files(repository.local).await;

                format!("{files:?}")
            })
        },
        expected: r#"Ok(["README.md"])"#,
    },
    ContractCase {
        operation: "list_staged_conflict_marker_files",
        prepare: |repository| {
            start_conflicting_rebase(repository)?;
            repository.git(&["add", "README.md"])?;

            Ok(())
        },
        observe: |repository| {
            Box::pin(async move {
                let files = RealGitClient
                    .list_staged_conflict_marker_files(
                        repository.local,
                        vec!["README.md".to_string()],
                    )
                    .await;

                format!("{files:?}")
            })
        },
        expected: r#"Ok(["README.md"])"#,
    },
    ContractCase {
        operation: "list_staged_conflict_marker_files",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let no_paths = RealGitClient
                    .list_staged_conflict_marker_files(repository.local.clone(), Vec::new())
                    .await;
                let outside_path = RealGitClient
                    .list_staged_conflict_marker_files(
                        repository.local,
                        vec!["../outside.txt".to_string()],
                    )
                    .await;

                format!(
                    "no_paths={} outside_path={}",
                    render_outcome(no_paths),
                    render_outcome(outside_path)
                )
            })
        },
        expected: "no_paths=Ok([]) outside_path=Err(git grep: Failed to check for staged conflict \
                   markers)",
    },
];

/// Operations that exchange commits or metadata with the `origin` remote.
const REMOTE_CASES: &[ContractCase] = &[
    ContractCase {
        operation: "pull_rebase",
        prepare: fall_behind_origin,
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient.pull_rebase(repository.local.clone()).await;

                format!("{result:?} subject={}", repository.subject("HEAD"))
            })
        },
        expected: "Ok(Completed) subject=Upstream change",
    },
    ContractCase {
        operation: "push_current_branch",
        prepare: commit_local_change,
        observe: |repository| {
            Box::pin(async move {
                let upstream = RealGitClient
                    .push_current_branch(repository.local.clone())
                    .await;

                format!(
                    "{upstream:?} synced={}",
                    repository.remote_matches_head("main")
                )
            })
        },
        expected: r#"Ok("origin/main") synced=true"#,
    },
    ContractCase {
        operation: "push_current_branch_to_remote_branch",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let upstream = RealGitClient
                    .push_current_branch_to_remote_branch(
                        repository.local.clone(),
                        "review/topic".to_string(),
                    )
                    .await;

                format!(
                    "{upstream:?} synced={}",
                    repository.remote_matches_head("review/topic")
                )
            })
        },
        expected: r#"Ok("origin/review/topic") synced=true"#,
    },
    ContractCase {
        operation: "remote_branch_exists",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let main_exists = RealGitClient
                    .remote_branch_exists(repository.local.clone(), "main".to_string())
                    .await;
                let feature_exists = RealGitClient
                    .remote_branch_exists(repository.local, "feature".to_string())
                    .await;

                format!("main={main_exists:?} feature={feature_exists:?}")
            })
        },
        expected: "main=Ok(true) feature=Ok(false)",
    },
    ContractCase {
        operation: "current_upstream_reference",
        prepare: KEEP_STANDARD_LAYOUT,
        observe: |repository| {
            Box::pin(async move {
                let upstream = RealGitClient
                    .current_upstream_reference(repository.local)
                    .await;

                format!("{upstream:?}")
            })
        },
        expected: r#"Ok("origin/main")"#,
    },
    ContractCase {
        operation: "fetch_remote",
        prepare: |repository| {
            fall_behind_origin(repository)?;
            repository.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;

            Ok(())
        },
        observe: |repository| {
            Box::pin(async move {
                let result = RealGitClient.fetch_remote(repository.local.clone()).await;
                let behind = repository.observe_git(&["rev-list", "--count", "HEAD..origin/main"]);

                format!("{result:?} behind={behind}")
            })
        },
        expected: "Ok(()) behind=1",
    },
    ContractCase {
        operation: "get_ahead_behind",
        prepare: fall_behind_origin,
        observe: |repository| {
            Box::pin(async move {
                let counts = RealGitClient.get_ahead_behind(repository.local).await;

                format!("{counts:?}")
            })
        },
        expected: "Ok((0, 1))",
    },
    ContractCase {
        operation: "branch_tracking_statuses",
        prepare: fall_behind_origin,
        observe: |repository| {
            Box::pin(async move {
                let statuses = RealGitClient
                    .branch_tracking_statuses(repository.local)
                    .await
                    .map(|statuses| statuses.into_iter().collect::<BTreeMap<_, _>>());

                format!("{statuses:?}")
            })
        },
        expected: r#"Ok({"feature": None, "main": Some((0, 1))})"#,
    },
    ContractCase {
        operation: "list_upstream_commit_titles",
        prepare: fall_behind_origin,
        observe: |repository| {
            Box::pin(async move {
                let titles = RealGitClient
                    .list_upstream_commit_titles(repository.local)
                    .await;

                format!("{titles:?}")
            })
        },
        expected: r#"Ok(["Upstream change"])"#,
    },
    ContractCase {
        operation: "list_local_commit_titles",
        prepare: commit_local_change,
        observe: |repository| {
            Box::pin(async move {
                let titles = RealGitClient
                    .list_local_commit_titles(repository.local)
                    .await;

                format!("{titles:?}")
            })
        },
        expected: r#"Ok(["Local change"])"#,
    },
    ContractCase {
        operation: "repo_url",
        prepare: |repository| {
            repository.git(&[
                "remote",
                "set-url",
                "origin",
                "git@github.com:agentty-xyz/agentty.git",
            ])?;

            Ok(())
        },
        observe: |repository| {
            Box::pin(async move {
                let url = RealGitClient.repo_url(repository.local).await;

                format!("{url:?}")
            })
        },
        expected: r#"Ok("https://github.com/agentty-xyz/agentty")"#,
    },
];

#[tokio::test]
async fn real_git_client_inspects_and_changes_local_history() -> TestResult<()> {
    run_contract_cases(LOCAL_HISTORY_CASES).await
}

#[tokio::test]
async fn real_git_client_manages_worktrees_and_rebases() -> TestResult<()> {
    run_contract_cases(WORKTREE_AND_REBASE_CASES).await
}

#[tokio::test]
async fn real_git_client_synchronizes_with_remote() -> TestResult<()> {
    run_contract_cases(REMOTE_CASES).await
}

/// Runs each case against its own fresh standard repository.
async fn run_contract_cases(cases: &[ContractCase]) -> TestResult<()> {
    for case in cases {
        // Arrange
        let temporary_directory = tempdir()?;
        let repository = ContractRepository::create(temporary_directory.path())?;
        (case.prepare)(&repository)?;

        // Act
        let observation = (case.observe)(repository).await;

        // Assert
        assert_eq!(observation, case.expected, "{}", case.operation);
    }

    Ok(())
}

/// Adds a linked `session` worktree beside `local`.
fn add_linked_worktree(repository: &ContractRepository) -> TestResult<()> {
    let worktree_path = repository.path("session");
    let worktree_path = worktree_path.to_str().ok_or("worktree path is not UTF-8")?;

    repository.git(&[
        "worktree",
        "add",
        "--quiet",
        "-b",
        "session",
        worktree_path,
        "main",
    ])?;

    Ok(())
}

/// Advances `main` past the `feature` fork point and checks out `feature`.
fn diverge_feature_from_main(repository: &ContractRepository) -> TestResult<()> {
    repository.commit_file("main.txt", "main\n", "Advance main")?;
    repository.git(&["checkout", "--quiet", "feature"])?;

    Ok(())
}

/// Checks out a `conflict` branch whose `README.md` edit conflicts with `main`.
fn diverge_conflicting_branch(repository: &ContractRepository) -> TestResult<()> {
    repository.commit_file("README.md", "main\n", "Main edit")?;
    repository.git(&["checkout", "--quiet", "-b", "conflict", "main~1"])?;

    repository.commit_file("README.md", "conflict\n", "Conflict edit")
}

/// Leaves a `conflict` branch stopped mid-rebase on a `README.md` conflict.
fn start_conflicting_rebase(repository: &ContractRepository) -> TestResult<()> {
    diverge_conflicting_branch(repository)?;

    let rebase = git_output(&repository.local, &["rebase", "main"])?;
    if rebase.status.success() {
        return Err("rebase should stop on the README.md conflict".into());
    }

    Ok(())
}

/// Commits one unpushed change on `main`.
fn commit_local_change(repository: &ContractRepository) -> TestResult<()> {
    repository.commit_file("local.txt", "local\n", "Local change")
}

/// Publishes one commit to `origin/main` and rewinds local `main` behind it.
fn fall_behind_origin(repository: &ContractRepository) -> TestResult<()> {
    repository.commit_file("upstream.txt", "upstream\n", "Upstream change")?;
    repository.git(&["push", "--quiet", "origin", "main"])?;
    repository.git(&["reset", "--quiet", "--hard", "HEAD~1"])?;

    Ok(())
}

/// Renders a client result with command failures reduced to the command and
/// the error context before Git's own version-dependent diagnostic.
fn render_outcome<T: Debug>(result: Result<T, GitError>) -> String {
    match result {
        Ok(value) => format!("Ok({value:?})"),
        Err(GitError::CommandFailed { command, stderr }) => {
            let context = stderr.split(':').next().unwrap_or_default();

            format!("Err({command}: {context})")
        }
        Err(error) => format!("Err({error})"),
    }
}

/// Renders a rebase step with the conflict detail reduced to its presence.
fn render_rebase_step(step: Result<RebaseStepResult, GitError>) -> String {
    match step {
        Ok(RebaseStepResult::Conflict { detail }) => {
            format!("Ok(Conflict) detail={}", !detail.is_empty())
        }
        step => render_outcome(step),
    }
}

/// Renders a fixture Git result for an observation string.
fn render_git_observation(result: TestResult<String>) -> String {
    result.unwrap_or_else(|error| format!("<git failed: {error}>"))
}

/// Runs `git` in `directory` and returns trimmed stdout, failing on a
/// non-zero exit.
fn run_git(directory: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = git_output(directory, arguments)?;
    if !output.status.success() {
        return Err(format!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Runs `git` in `directory` without host or system configuration.
fn git_output(directory: &Path, arguments: &[&str]) -> TestResult<Output> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;

    Ok(output)
}
