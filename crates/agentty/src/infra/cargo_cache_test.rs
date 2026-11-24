//! Real trusted Cargo cache client tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{CargoCacheClient, RealCargoCacheClient};

/// A lockfile pinning one crates.io package.
const LOCKFILE: &str = "version = 4\n\n[[package]]\nname = \"leaf\"\nversion = \
                        \"1.0.0\"\nsource = \
                        \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \
                        \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n";

#[tokio::test]
/// Fetches nothing for a worktree without a `Cargo.lock`, and reports a
/// Cargo that cannot run without starting a fetch.
async fn refresh_reports_only_real_failures() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let client = RealCargoCacheClient::new(root.path().join("cache"), "/nonexistent/cargo");
    let unlocked = root.path().join("unlocked");
    let locked = root.path().join("locked");
    for worktree in [&unlocked, &locked] {
        std::fs::create_dir_all(worktree).expect("worktree");
    }
    std::fs::write(locked.join("Cargo.lock"), LOCKFILE).expect("lockfile");
    let fetched = Arc::new(AtomicBool::new(false));
    let on_fetch = || {
        let fetched = Arc::clone(&fetched);
        Box::new(move || fetched.store(true, Ordering::SeqCst)) as Box<dyn FnOnce() + Send>
    };

    // Act
    let skipped = client
        .refresh(root.path().to_path_buf(), unlocked, on_fetch())
        .await;
    let failed = client
        .refresh(root.path().to_path_buf(), locked, on_fetch())
        .await;

    // Assert
    assert_eq!(skipped, Ok(()));
    assert!(failed.is_err());
    assert!(!fetched.load(Ordering::SeqCst));
}
