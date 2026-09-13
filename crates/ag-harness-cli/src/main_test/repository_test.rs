use std::env;

use ag_harness::{Repository, Tool};
use clap::Parser;

use super::support::test_git_executable;
use crate::{Cli, CliError, comparison_options, repository_from_path, repository_or_default};

#[test]
fn cli_accepts_the_default_git_executable() {
    // Arrange
    let arguments = [
        "ag-harness",
        "--database",
        "unused.db",
        "run",
        "muse-test",
        "Hello",
    ];

    // Act
    let cli = Cli::try_parse_from(arguments)
        .expect("missing Git executable override should use the default");

    // Assert
    assert_eq!(cli.git_executable, None);
}

#[test]
#[cfg(unix)]
fn git_executable_default_skips_a_non_executable_file() {
    // Arrange
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let executable_name = format!("git{}", env::consts::EXE_SUFFIX);
    let inert = storage.path().join(executable_name);
    std::fs::write(&inert, "not executable").expect("inert Git fixture should be written");
    let trusted_git = test_git_executable();
    let trusted_directory = trusted_git
        .parent()
        .expect("trusted Git executable should have a parent");
    let path =
        env::join_paths([storage.path(), trusted_directory]).expect("test PATH should be valid");
    let root = env::current_dir().expect("current directory should resolve");
    let expected = Repository::new(&root, trusted_git)
        .expect("trusted Git executable should configure the repository");

    // Act
    let actual = repository_from_path(&root, Some(path.as_os_str()));

    // Assert
    assert_eq!(
        actual.expect("non-executable Git should be skipped"),
        expected
    );
}

#[test]
fn git_executable_default_uses_the_process_path() {
    // Arrange
    let root = env::current_dir().expect("current directory should resolve");
    let expected = Repository::new(&root, test_git_executable())
        .expect("trusted Git executable should configure the repository");

    // Act
    let actual = repository_or_default(root, None)
        .expect("test PATH should contain a trusted Git executable");

    // Assert
    assert_eq!(actual, expected);
}

#[test]
fn git_executable_default_requires_git_on_path() {
    // Arrange
    let root = env::current_dir().expect("current directory should resolve");

    // Act
    let error = repository_from_path(&root, None)
        .expect_err("missing PATH should not produce a Git executable");

    // Assert
    assert!(matches!(error, CliError::GitExecutableNotFound));
}

#[test]
fn git_executable_default_preserves_repository_root_errors() {
    // Arrange
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let missing_root = storage.path().join("missing-root");
    let path = env::var_os("PATH").expect("test PATH should be configured");

    // Act
    let error = repository_from_path(&missing_root, Some(path.as_os_str()))
        .expect_err("missing repository root should fail");

    // Assert
    assert!(matches!(
        error,
        CliError::Repository(ag_harness::RepositoryError::Root { .. })
    ));
}

#[tokio::test]
async fn comparison_selection_is_explicit_validated_and_preserves_permissions() {
    // Arrange
    let repository =
        Repository::new(env!("CARGO_MANIFEST_DIR"), test_git_executable()).expect("repository");

    // Act
    let absent = comparison_options(&repository, None, false)
        .await
        .expect("no base required");
    let selected = comparison_options(&repository, Some("HEAD"), true)
        .await
        .expect("explicit commit");
    let invalid = comparison_options(&repository, Some("HEAD^{tree}"), false).await;

    // Assert
    assert!(absent.comparison_base().is_none());
    assert!(absent.tool_policy().allows(Tool::Read));
    assert!(!absent.tool_policy().allows(Tool::Write));
    assert!(selected.comparison_base().is_some());
    assert!(selected.tool_policy().allows(Tool::Write));
    assert!(matches!(invalid, Err(CliError::ComparisonBase(_))));
}

#[test]
fn run_and_resume_accept_explicit_comparison_selection() {
    // Arrange
    let requests = [
        vec!["ag-harness", "run", "model", "--comparison-base", "release"],
        vec!["ag-harness", "resume", "session", "--comparison-base", "v1"],
    ];

    // Act
    let selections: Vec<_> = requests
        .into_iter()
        .map(|arguments| {
            Cli::try_parse_from(arguments)
                .expect("CLI selection")
                .comparison_base
        })
        .collect();

    // Assert
    assert_eq!(selections, [Some("release".into()), Some("v1".into())]);
}
