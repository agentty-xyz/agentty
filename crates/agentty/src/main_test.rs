use std::env;

use agentty::app::{AppError, agentty_home};
use agentty::infra::db::{DB_DIR, DB_FILE, acquire_instance_lock};
use clap::Parser;
use clap::error::ErrorKind;

use super::{Cli, run};

const LOCK_FAILURE_CHILD_ENV: &str = "AGENTTY_LOCK_FAILURE_CHILD";
const OWNED_ROOT_CHILD_ENV: &str = "AGENTTY_OWNED_ROOT_CHILD";
const DATABASE_FAILURE_CHILD_ENV: &str = "AGENTTY_DATABASE_FAILURE_CHILD";

#[test]
fn cli_defaults_to_automatic_updates() {
    // Arrange / Act
    let cli = Cli::try_parse_from(["agentty"]).expect("default arguments should parse");

    // Assert
    assert!(!cli.no_update);
}

#[test]
fn cli_parses_no_update_flag() {
    // Arrange / Act
    let cli = Cli::try_parse_from(["agentty", "--no-update"]).expect("--no-update should parse");

    // Assert
    assert!(cli.no_update);
}

#[test]
fn cli_rejects_unknown_arguments() {
    // Arrange / Act
    let error =
        Cli::try_parse_from(["agentty", "--no-updte"]).expect_err("unknown arguments should fail");

    // Assert
    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
}

#[tokio::test]
async fn run_reports_instance_lock_parent_creation_failure() {
    if env::var_os(LOCK_FAILURE_CHILD_ENV).is_some() {
        // Arrange
        let cli = Cli { no_update: false };

        // Act
        let error = run(cli)
            .await
            .expect_err("startup should reject a file-backed root");

        // Assert
        assert!(matches!(error, AppError::Workflow(_)));
        assert!(
            error
                .to_string()
                .contains("Failed to acquire the Agentty instance lock")
        );

        return;
    }

    // Arrange
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let blocking_root = temp_dir.path().join("agentty-root");
    tokio::fs::write(&blocking_root, b"")
        .await
        .expect("blocking root file should be created");
    let test_binary = env::current_exe().expect("test binary path should resolve");

    // Act
    let status = tokio::process::Command::new(test_binary)
        .arg("--exact")
        .arg("tests::run_reports_instance_lock_parent_creation_failure")
        .env(LOCK_FAILURE_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", blocking_root)
        .status()
        .await
        .expect("isolated startup test should run");

    // Assert
    assert!(status.success());
}

#[tokio::test]
async fn run_rejects_an_owned_root_before_opening_the_database() {
    if env::var_os(OWNED_ROOT_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(Cli { no_update: true })
            .await
            .expect_err("root is owned");

        // Assert
        assert!(
            error
                .to_string()
                .contains("Another Agentty instance is already using")
        );

        return;
    }

    // Arrange
    let root = tempfile::tempdir().expect("root");
    let owner = acquire_instance_lock(root.path())
        .await
        .expect("owner lock");
    let database_path = root.path().join(DB_DIR).join(DB_FILE);

    // Act
    let status = tokio::process::Command::new(env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("tests::run_rejects_an_owned_root_before_opening_the_database")
        .env(OWNED_ROOT_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", root.path())
        .status()
        .await
        .expect("contending process");

    // Assert
    assert!(status.success());
    assert!(
        !database_path.exists(),
        "contention must precede database creation"
    );
    drop(owner);
}

#[tokio::test]
async fn run_releases_instance_lock_after_database_open_failure() {
    if env::var_os(DATABASE_FAILURE_CHILD_ENV).is_some() {
        // Arrange / Act
        let error = run(Cli { no_update: true })
            .await
            .expect_err("database path is a directory");

        // Assert
        assert!(matches!(error, AppError::Db(_)));
        let _owner = acquire_instance_lock(&agentty_home())
            .await
            .expect("failed startup must release ownership");

        return;
    }

    // Arrange
    let root = tempfile::tempdir().expect("root");
    tokio::fs::create_dir_all(root.path().join(DB_DIR).join(DB_FILE))
        .await
        .expect("block database opening");

    // Act
    let status = tokio::process::Command::new(env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("tests::run_releases_instance_lock_after_database_open_failure")
        .env(DATABASE_FAILURE_CHILD_ENV, "1")
        .env("AGENTTY_ROOT", root.path())
        .status()
        .await
        .expect("startup process");

    // Assert
    assert!(status.success());
}
