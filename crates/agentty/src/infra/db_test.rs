use std::io;

use super::{DB_DIR, acquire_instance_lock, timestamp_source_from_environment};

#[tokio::test]
async fn instance_lock_is_exclusive_per_root_and_reusable_after_drop() {
    // Arrange
    let first_root = tempfile::tempdir().expect("first root");
    let other_root = tempfile::tempdir().expect("other root");
    let first = acquire_instance_lock(first_root.path())
        .await
        .expect("first lock");

    // Act
    let error = acquire_instance_lock(first_root.path())
        .await
        .expect_err("root is owned");
    let other = acquire_instance_lock(other_root.path())
        .await
        .expect("independent root");
    drop(first);
    let restarted = acquire_instance_lock(first_root.path())
        .await
        .expect("released lock");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(other.metadata().expect("other lock metadata").is_file());
    assert!(
        restarted
            .metadata()
            .expect("restarted lock metadata")
            .is_file()
    );
}

#[tokio::test]
async fn instance_lock_reports_directory_and_file_errors() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join(DB_DIR), "blocked").expect("block directory");

    // Act / Assert
    assert!(acquire_instance_lock(root.path()).await.is_err());
    std::fs::remove_file(root.path().join(DB_DIR)).expect("remove blocker");
    std::fs::create_dir_all(root.path().join(DB_DIR).join("agentty.lock"))
        .expect("block lock file");
    assert!(acquire_instance_lock(root.path()).await.is_err());
}

#[tokio::test]
async fn instance_lock_is_released_after_process_exit_or_kill() {
    const CHILD_ROOT: &str = "AGENTTY_LOCK_HOLDER_TEST_ROOT";

    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        // Arrange
        let root = std::path::PathBuf::from(root);
        let _owner = acquire_instance_lock(&root).await.expect("child lock");

        // Act / Assert: retain ownership until the parent requests exit
        // or kills this process without dropping the handle.
        while !root.join("stop").exists() {
            tokio::fs::write(root.join("ready"), b"")
                .await
                .expect("signal ownership");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        return;
    }

    // Normal exit also lets the child flush coverage for the shared holder.
    for terminate_abruptly in [false, true] {
        // Arrange
        let root = tempfile::tempdir().expect("root");
        let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("--exact")
            .arg("infra::db::tests::instance_lock_is_released_after_process_exit_or_kill")
            .env(CHILD_ROOT, root.path())
            .kill_on_drop(true)
            .spawn()
            .expect("lock holder process");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !root.path().join("ready").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child should acquire lock");

        // Act
        let error = acquire_instance_lock(root.path())
            .await
            .expect_err("child owns root");
        if terminate_abruptly {
            child.kill().await.expect("kill and reap owner");
        } else {
            tokio::fs::write(root.path().join("stop"), b"")
                .await
                .expect("request graceful exit");
            let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
                .await
                .expect("child should exit")
                .expect("reap owner");
            assert!(status.success());
        }
        let restarted = acquire_instance_lock(root.path())
            .await
            .expect("restart after process exit");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(restarted.metadata().expect("lock metadata").is_file());
    }
}

#[test]
fn environment_timestamp_source_returns_a_unix_timestamp() {
    // Arrange
    let timestamp_source = timestamp_source_from_environment();

    // Act
    let timestamp = timestamp_source.now_timestamp_seconds();

    // Assert
    assert!(timestamp > 0);
}
