//! Repository helper tests covering non-interactive git commands, timeouts,
//! URL normalization, and repository root and layout resolution.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use opentelemetry::Value;
use opentelemetry_sdk::trace::SpanData;
use tempfile::tempdir;

use crate::error::GitError;
use crate::repo::{
    AsyncGitCommand, GIT_COMMAND_TIMEOUT, NETWORK_GIT_COMMAND_TIMEOUT,
    apply_non_interactive_environment, batch_mode_ssh_command, command_output_detail,
    format_git_invocation, main_checkout_working_tree, main_checkout_working_tree_sync,
    main_repo_root, no_submodule_recursion_environment, normalize_repo_url,
    parse_git_directory_paths, parse_repo_root, repo_root_from_git_dir_async, resolve_git_dir,
    run_git_command_sync, run_git_command_with_timeout,
};
use crate::telemetry_support::{SpanCapture, attribute, attribute_keys};

#[test]
fn test_apply_non_interactive_environment_sets_git_prompt_controls() {
    // Arrange
    let mut command = Command::new("git");

    // Act
    apply_non_interactive_environment(&mut command);

    // Assert
    let env_pairs: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|resolved_value| {
                (
                    key.to_string_lossy().to_string(),
                    resolved_value.to_string_lossy().to_string(),
                )
            })
        })
        .collect();
    assert!(
        env_pairs
            .iter()
            .any(|(key, value)| key == "GIT_TERMINAL_PROMPT" && value == "0")
    );
    assert!(
        env_pairs
            .iter()
            .any(|(key, value)| key == "GCM_INTERACTIVE" && value == "never")
    );
    assert!(
        env_pairs
            .iter()
            .any(|(key, value)| key == "GIT_SSH_COMMAND" && value.contains("BatchMode=yes"))
    );
}

#[test]
fn test_batch_mode_ssh_command_extends_configured_command_or_defaults_to_ssh() {
    // Arrange
    let configured_command = Some("ssh -i /keys/agentty".to_string());

    // Act
    let extended_command = batch_mode_ssh_command(configured_command);
    let default_command = batch_mode_ssh_command(None);

    // Assert
    assert_eq!(extended_command, "ssh -i /keys/agentty -o BatchMode=yes");
    assert_eq!(default_command, "ssh -o BatchMode=yes");
}

#[test]
fn no_submodule_recursion_follows_the_configuration_the_environment_already_passes() {
    // Arrange / Act
    let alone = no_submodule_recursion_environment(None);
    let after_two = no_submodule_recursion_environment(Some("2".to_string()));
    let unparsable = no_submodule_recursion_environment(Some("many".to_string()));

    // Assert
    let pairs = |index: usize, count: &str| {
        [
            (
                format!("GIT_CONFIG_KEY_{index}"),
                "submodule.recurse".to_string(),
            ),
            (format!("GIT_CONFIG_VALUE_{index}"), "false".to_string()),
            ("GIT_CONFIG_COUNT".to_string(), count.to_string()),
        ]
    };
    assert_eq!(alone, pairs(0, "1"));
    assert_eq!(after_two, pairs(2, "3"));
    assert_eq!(unparsable, pairs(0, "1"));
}

#[test]
fn git_commands_never_recurse_into_submodules_whatever_the_repository_configures() {
    // Arrange
    let directory = tempdir().expect("repository");
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(directory.path())
        .status()
        .expect("git init");
    assert!(status.success());
    let status = Command::new("git")
        .args(["config", "submodule.recurse", "true"])
        .current_dir(directory.path())
        .status()
        .expect("git config");
    assert!(status.success());

    // Act
    let recurse = run_git_command_sync(
        directory.path(),
        &["config", "--get", "submodule.recurse"],
        "read submodule.recurse",
    );

    // Assert
    assert_eq!(recurse.expect("configured value").trim(), "false");
}

#[test]
fn test_async_git_command_timeout_widens_bound_for_remote_transfers() {
    // Arrange
    let command_for = |arguments: &[&str]| {
        AsyncGitCommand::new(
            Path::new("/tmp/repo").to_path_buf(),
            arguments.iter().map(ToString::to_string).collect(),
        )
    };
    let network_commands = [
        command_for(&["push", "--porcelain", "origin", "HEAD:feature"]),
        command_for(&["fetch"]),
        command_for(&["pull", "--rebase", "origin", "main"]),
        command_for(&["ls-remote", "--heads", "origin", "feature"]),
        command_for(&["-c", "http.lowSpeedLimit=1", "--no-pager", "push"]),
    ];
    let local_commands = [
        command_for(&["status"]),
        command_for(&["-c", "diff.noprefix=false", "diff", "--no-color"]),
        command_for(&["-c", "push"]),
        command_for(&["--version"]),
        command_for(&[]),
    ];

    // Act
    let network_timeouts: Vec<Duration> = network_commands
        .iter()
        .map(AsyncGitCommand::timeout)
        .collect();
    let local_timeouts: Vec<Duration> = local_commands
        .iter()
        .map(AsyncGitCommand::timeout)
        .collect();

    // Assert
    assert!(
        network_timeouts
            .iter()
            .all(|timeout| *timeout == NETWORK_GIT_COMMAND_TIMEOUT)
    );
    assert!(
        local_timeouts
            .iter()
            .all(|timeout| *timeout == GIT_COMMAND_TIMEOUT)
    );
}

#[tokio::test]
async fn test_async_git_command_reports_spawn_failure_for_missing_directory() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let command = AsyncGitCommand::new(temp_dir.path().join("missing"), vec!["status".to_string()]);

    // Act
    let error = run_git_command_with_timeout(command, Duration::from_secs(5))
        .await
        .expect_err("missing working directory should fail to spawn git");

    // Assert
    assert!(
        matches!(&error, GitError::CommandFailed { command, stderr }
                if command == "git status" && !stderr.is_empty()),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn test_async_git_command_timeout_cancels_process() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temporary repository");
    let init_output = tokio::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(temp_dir.path())
        .output()
        .await
        .expect("failed to initialize temporary repository");
    assert!(init_output.status.success());
    let timeout = Duration::from_millis(25);
    let command = AsyncGitCommand::new(
        temp_dir.path().to_path_buf(),
        vec![
            "-c".to_string(),
            "alias.agentty-hang=!exec sleep 1".to_string(),
            "agentty-hang".to_string(),
        ],
    );

    // Act
    let error = run_git_command_with_timeout(command, timeout)
        .await
        .expect_err("long-running git command should time out");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandTimedOut {
            ref command,
            timeout: actual_timeout,
        } if command == "git -c alias.agentty-hang=!exec sleep 1 agentty-hang"
            && actual_timeout == timeout
    ));
}

#[test]
fn test_command_output_detail_prefers_stderr_then_stdout_then_unknown() {
    // Arrange

    // Act
    let stderr_detail = command_output_detail(b"stdout detail", b"stderr detail");
    let stdout_detail = command_output_detail(b"stdout detail", b"");
    let unknown_detail = command_output_detail(b"", b"");

    // Assert
    assert_eq!(stderr_detail, "stderr detail");
    assert_eq!(stdout_detail, "stdout detail");
    assert_eq!(unknown_detail, "Unknown git error");
}

#[test]
fn test_normalize_repo_url_converts_supported_github_formats() {
    // Arrange

    // Act
    let ssh_short = normalize_repo_url("git@github.com:agentty-xyz/agentty.git");
    let ssh_long = normalize_repo_url("ssh://git@github.com/agentty-xyz/agentty.git");
    let passthrough = normalize_repo_url("https://example.com/agentty-xyz/agentty.git");

    // Assert
    assert_eq!(ssh_short, "https://github.com/agentty-xyz/agentty");
    assert_eq!(ssh_long, "https://github.com/agentty-xyz/agentty");
    assert_eq!(passthrough, "https://example.com/agentty-xyz/agentty");
}

#[test]
fn test_resolve_git_dir_supports_directories_and_gitdir_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_with_directory = temp_dir.path().join("repo-directory");
    let repo_with_absolute_file = temp_dir.path().join("repo-absolute");
    let repo_with_relative_file = temp_dir.path().join("repo-relative");
    let relative_git_dir = repo_with_relative_file.join(".actual-git");
    let malformed_repo = temp_dir.path().join("repo-malformed");
    fs::create_dir_all(repo_with_directory.join(".git"))
        .expect("failed to create .git directory repo");
    fs::create_dir_all(&repo_with_absolute_file).expect("failed to create absolute repo");
    fs::create_dir_all(&repo_with_relative_file).expect("failed to create relative repo");
    fs::create_dir_all(&relative_git_dir).expect("failed to create relative git dir");
    fs::create_dir_all(&malformed_repo).expect("failed to create malformed repo");
    fs::write(
        repo_with_absolute_file.join(".git"),
        format!("gitdir: {}", temp_dir.path().join("absolute-git").display()),
    )
    .expect("failed to write absolute gitdir file");
    fs::write(
        repo_with_relative_file.join(".git"),
        "gitdir: .actual-git\n",
    )
    .expect("failed to write relative gitdir file");
    fs::write(malformed_repo.join(".git"), "not-a-gitdir-file")
        .expect("failed to write malformed gitdir file");

    // Act
    let directory_git_dir = resolve_git_dir(&repo_with_directory);
    let absolute_git_dir = resolve_git_dir(&repo_with_absolute_file);
    let relative_git_dir_resolved = resolve_git_dir(&repo_with_relative_file);
    let malformed_git_dir = resolve_git_dir(&malformed_repo);

    // Assert
    assert_eq!(directory_git_dir, Some(repo_with_directory.join(".git")));
    assert_eq!(absolute_git_dir, Some(temp_dir.path().join("absolute-git")));
    assert_eq!(relative_git_dir_resolved, Some(relative_git_dir));
    assert_eq!(malformed_git_dir, None);
}

#[test]
fn test_run_git_command_sync_returns_command_failed_on_invalid_subcommand() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = run_git_command_sync(
        temp_dir.path(),
        &["definitely-not-a-git-subcommand"],
        "Git command failed",
    );

    // Assert
    let error = result.expect_err("invalid git command should fail");
    assert!(
        matches!(&error, GitError::CommandFailed { command, stderr }
                if command == "git definitely-not-a-git-subcommand"
                    && stderr.contains("Git command failed")),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn repo_root_from_git_dir_async_falls_back_to_git_toplevel() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    run_setup_git(temp_dir.path(), &["init", "--quiet"]);
    let nonstandard_git_dir = temp_dir.path().join("custom-admin");

    // Act
    let repo_root = repo_root_from_git_dir_async(temp_dir.path(), &nonstandard_git_dir)
        .await
        .expect("repository root fallback should succeed");

    // Assert
    assert_eq!(
        repo_root,
        fs::canonicalize(temp_dir.path()).expect("repository root should canonicalize")
    );
}

#[tokio::test]
async fn test_main_repo_root_returns_command_failed_outside_git_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = main_repo_root(temp_dir.path().to_path_buf()).await;

    // Assert
    let error = result.expect_err("non-repo should fail");
    assert!(
        matches!(&error, GitError::CommandFailed { command, stderr }
                if command.starts_with("git rev-parse")
                    && stderr.contains("Git rev-parse failed")),
        "unexpected error: {error:?}"
    );
}

/// Runs a setup git command in `cwd`, asserting success and returning
/// trimmed stdout.
fn run_setup_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run setup git command");
    assert!(
        output.status.success(),
        "git command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[tokio::test]
async fn test_bare_layout_resolves_bare_admin_root_and_no_working_checkout() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let root = temp_dir.path();
    let bare_dir = root.join(".bare");
    run_setup_git(root, &["init", "--bare", ".bare"]);
    run_setup_git(&bare_dir, &["config", "user.name", "Test User"]);
    run_setup_git(&bare_dir, &["config", "user.email", "test@example.com"]);
    let empty_tree = run_setup_git(&bare_dir, &["hash-object", "-w", "-t", "tree", "/dev/null"]);
    let commit = run_setup_git(&bare_dir, &["commit-tree", &empty_tree, "-m", "init"]);
    run_setup_git(&bare_dir, &["update-ref", "refs/heads/main", &commit]);
    run_setup_git(&bare_dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let main_worktree = root.join("main");
    let session_worktree = root.join("session");
    run_setup_git(
        &bare_dir,
        &[
            "worktree",
            "add",
            main_worktree.to_str().expect("main worktree path is utf-8"),
            "main",
        ],
    );
    run_setup_git(
        &bare_dir,
        &[
            "worktree",
            "add",
            "-b",
            "session",
            session_worktree
                .to_str()
                .expect("session worktree path is utf-8"),
            "main",
        ],
    );

    // Act
    let admin_root = main_repo_root(session_worktree.clone())
        .await
        .expect("failed to resolve admin root");
    let working_checkout = main_checkout_working_tree_sync(&session_worktree)
        .expect("failed to resolve working checkout");

    // Assert
    assert_eq!(
        admin_root,
        fs::canonicalize(&bare_dir).expect("failed to canonicalize bare dir")
    );
    assert_eq!(working_checkout, None);
}

#[test]
fn test_non_bare_layout_resolves_main_working_checkout_for_linked_worktree() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let root = temp_dir.path();
    let main_checkout = root.join("main");
    fs::create_dir_all(&main_checkout).expect("failed to create main checkout dir");
    run_setup_git(&main_checkout, &["init", "-b", "main"]);
    run_setup_git(&main_checkout, &["config", "user.name", "Test User"]);
    run_setup_git(
        &main_checkout,
        &["config", "user.email", "test@example.com"],
    );
    fs::write(main_checkout.join("README.md"), "test repo").expect("failed to write file");
    run_setup_git(&main_checkout, &["add", "README.md"]);
    run_setup_git(&main_checkout, &["commit", "-m", "Initial commit"]);
    let session_worktree = root.join("session");
    run_setup_git(
        &main_checkout,
        &[
            "worktree",
            "add",
            "-b",
            "session",
            session_worktree
                .to_str()
                .expect("session worktree path is utf-8"),
            "main",
        ],
    );

    // Act
    let working_checkout = main_checkout_working_tree_sync(&session_worktree)
        .expect("failed to resolve working checkout");

    // Assert
    assert_eq!(
        working_checkout,
        Some(fs::canonicalize(&main_checkout).expect("failed to canonicalize main checkout"))
    );
}

#[test]
fn test_main_checkout_working_tree_resolves_standalone_and_separated_git_dirs() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let root = fs::canonicalize(temp_dir.path()).expect("failed to canonicalize temp dir");
    let standalone_checkout = root.join("standalone");
    let separated_checkout = root.join("separated");
    let separated_git_dir = root.join("separated-metadata");
    run_setup_git(
        &root,
        &[
            "init",
            "--quiet",
            standalone_checkout
                .to_str()
                .expect("standalone path is utf-8"),
        ],
    );
    run_setup_git(
        &root,
        &[
            "init",
            "--quiet",
            "--separate-git-dir",
            separated_git_dir
                .to_str()
                .expect("separated git dir is utf-8"),
            separated_checkout
                .to_str()
                .expect("separated path is utf-8"),
        ],
    );

    for checkout in [standalone_checkout, separated_checkout] {
        // Act
        let working_checkout =
            main_checkout_working_tree_sync(&checkout).expect("failed to resolve working checkout");

        // Assert
        assert_eq!(working_checkout, Some(checkout));
    }
}

#[test]
fn test_parse_git_directory_paths_rejects_missing_paths() {
    // Arrange
    let repo_path = Path::new("repository");

    // Act
    let missing_git_dir = parse_git_directory_paths(repo_path, "\n  \n");
    let missing_common_dir = parse_git_directory_paths(repo_path, ".git\n");

    // Assert
    assert!(matches!(
        missing_git_dir,
        Err(GitError::OutputParse(ref detail))
            if detail == "Git rev-parse output missing git-dir"
    ));
    assert!(matches!(
        missing_common_dir,
        Err(GitError::OutputParse(ref detail))
            if detail == "Git rev-parse output missing git-common-dir"
    ));
}

#[test]
fn test_parse_repo_root_rejects_blank_output() {
    // Arrange / Act
    let result = parse_repo_root(" \n");

    // Assert
    assert!(matches!(
        result,
        Err(GitError::OutputParse(ref detail))
            if detail == "Git rev-parse --show-toplevel returned empty output"
    ));
}

#[test]
fn test_format_git_invocation_returns_bare_git_for_empty_args() {
    // Arrange / Act / Assert
    assert_eq!(format_git_invocation(&[]), "git");
}

#[test]
fn test_format_git_invocation_joins_args_after_git() {
    // Arrange / Act / Assert
    assert_eq!(
        format_git_invocation(&["diff", "--cached", "--quiet"]),
        "git diff --cached --quiet"
    );
}

#[tokio::test]
async fn test_git_command_spans_join_blocking_callers_with_content_free_metadata() {
    // Arrange
    let capture = SpanCapture::install().await;
    let temp_dir = tempdir().expect("failed to create temp dir");
    run_setup_git(temp_dir.path(), &["init", "--quiet"]);
    let repo_path = temp_dir.path().to_path_buf();
    let untraced = main_checkout_working_tree(repo_path.clone()).await;

    // Act
    let (traced, trace) = capture
        .trace(main_checkout_working_tree(repo_path.clone()))
        .await;

    // Assert
    assert_eq!(
        untraced.expect("untraced lookup"),
        traced.expect("traced lookup")
    );
    let commands = trace.named("git.command");
    assert!(commands.len() >= 2, "{}", commands.len());
    assert_eq!(
        capture
            .finished_spans()
            .iter()
            .filter(|span| span.name == "git.command")
            .count(),
        commands.len()
    );
    for command in commands {
        assert_eq!(command.parent_span_id, trace.root_id);
        assert_eq!(
            attribute_keys(command),
            [
                "agentty.outcome",
                "agentty.output.bytes",
                "agentty.timed_out",
                "git.subcommand",
                "process.exit.code",
            ]
        );
        assert_eq!(
            attribute(command, "git.subcommand"),
            Some(&Value::from("rev-parse"))
        );
        assert_eq!(
            attribute(command, "process.exit.code"),
            Some(&Value::I64(0))
        );
        assert_eq!(
            attribute(command, "agentty.timed_out"),
            Some(&Value::Bool(false))
        );
        assert!(matches!(
            attribute(command, "agentty.output.bytes"),
            Some(Value::I64(bytes)) if *bytes > 0
        ));
        assert_eq!(
            attribute(command, "agentty.outcome"),
            Some(&Value::from("completed"))
        );
    }
    let repo_path_text = repo_path.to_string_lossy().into_owned();
    assert!(!format!("{:?}", trace.spans).contains(&repo_path_text));
}

#[tokio::test]
async fn test_async_git_command_spans_classify_exits_timeouts_and_spawn_failures() {
    // Arrange
    let capture = SpanCapture::install().await;
    let temp_dir = tempdir().expect("failed to create temp dir");
    run_setup_git(temp_dir.path(), &["init", "--quiet"]);
    let repo_path = temp_dir.path().to_path_buf();
    let completed = async_command(&repo_path, &["-c", "color.ui=never", "status", "--short"]);
    let rejected = async_command(
        &repo_path,
        &["rev-parse", "--verify", "agentty-missing-ref"],
    );
    let hung = async_command(
        &repo_path,
        &["-c", "alias.agentty-hang=!exec sleep 1", "agentty-hang"],
    );
    let signaled = async_command(
        &repo_path,
        &[
            "-c",
            "alias.agentty-signal=!kill -9 $PPID",
            "agentty-signal",
        ],
    );
    let unspawnable = async_command(&repo_path.join("missing"), &["status"]);

    // Act
    let (results, trace) = capture
        .trace(async {
            [
                run_git_command_with_timeout(completed, Duration::from_secs(5)).await,
                run_git_command_with_timeout(rejected, Duration::from_secs(5)).await,
                run_git_command_with_timeout(signaled, Duration::from_secs(5)).await,
                run_git_command_with_timeout(hung, Duration::from_millis(25)).await,
                run_git_command_with_timeout(unspawnable, Duration::from_secs(5)).await,
            ]
        })
        .await;

    // Assert
    assert!(matches!(
        &results,
        [
            Ok(completed),
            Ok(rejected),
            Ok(signaled),
            Err(GitError::CommandTimedOut { .. }),
            Err(GitError::CommandFailed { .. }),
        ] if completed.success()
            && rejected.exit_code == Some(128)
            && signaled.exit_code.is_none()
    ));
    let commands = trace.named("git.command");
    let observed: Vec<_> = commands
        .iter()
        .map(|command| command_span_record(command))
        .collect();
    assert_eq!(
        observed,
        [
            expected_command_span("status", Some(0), false, "completed", None),
            expected_command_span("rev-parse", Some(128), false, "completed", None),
            expected_command_span("other", None, false, "completed", None),
            expected_command_span("other", None, true, "failed", Some("timeout")),
            expected_command_span("status", None, false, "failed", Some("spawn")),
        ]
    );
    assert!(
        commands
            .iter()
            .all(|command| command.parent_span_id == trace.root_id)
    );
    let exported = format!("{:?}", trace.spans);
    for private_text in [
        "agentty-hang",
        "agentty-missing-ref",
        "agentty-signal",
        "color.ui",
        "--short",
        &repo_path.to_string_lossy(),
    ] {
        assert!(!exported.contains(private_text), "{private_text}");
    }
}

/// Subcommand, exit code, timeout flag, outcome, and error type recorded on
/// one `git.command` span.
type CommandSpanRecord = (
    Option<Value>,
    Option<Value>,
    Option<Value>,
    Option<Value>,
    Option<Value>,
);

/// Builds one asynchronous git command from borrowed arguments.
fn async_command(repo_path: &Path, arguments: &[&str]) -> AsyncGitCommand {
    AsyncGitCommand::new(
        repo_path.to_path_buf(),
        arguments.iter().map(ToString::to_string).collect(),
    )
}

/// Returns the content-free metadata recorded on one `git.command` span.
fn command_span_record(command: &SpanData) -> CommandSpanRecord {
    (
        attribute(command, "git.subcommand").cloned(),
        attribute(command, "process.exit.code").cloned(),
        attribute(command, "agentty.timed_out").cloned(),
        attribute(command, "agentty.outcome").cloned(),
        attribute(command, "error.type").cloned(),
    )
}

/// Builds the expected metadata of one `git.command` span.
fn expected_command_span(
    subcommand: &'static str,
    exit_code: Option<i64>,
    timed_out: bool,
    outcome: &'static str,
    error_type: Option<&'static str>,
) -> CommandSpanRecord {
    (
        Some(Value::from(subcommand)),
        exit_code.map(Value::I64),
        Some(Value::Bool(timed_out)),
        Some(Value::from(outcome)),
        error_type.map(Value::from),
    )
}
