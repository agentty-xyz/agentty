//! Public CLI coverage for module router validation.

use std::path::Path;
use std::process::{Command, Output};
use std::{fs, io};

use tempfile::tempdir;

/// Runs `ag-xtask check-module-routers` in `directory`.
fn check_module_routers(directory: &Path) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
        .current_dir(directory)
        .arg("check-module-routers")
        .output()
}

#[test]
fn cli_accepts_this_workspace_and_rejects_a_router_that_defines_items() {
    // Arrange
    let directory = tempdir().expect("workspace");
    let sources = directory.path().join("crates/example/src");
    fs::create_dir_all(sources.join("app")).expect("router directory");
    fs::write(sources.join("app.rs"), "mod child;\nfn helper() {}\n").expect("router");
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    // Act
    let workspace = check_module_routers(workspace_root).expect("workspace check");
    let invalid = check_module_routers(directory.path()).expect("invalid check");

    // Assert
    assert!(
        workspace.status.success(),
        "{}",
        String::from_utf8_lossy(&workspace.stdout)
    );
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stdout).contains(
        "crates/example/src/app.rs: router module `app.rs` defines `fn`; move it into a child \
         module under `app/`"
    ));
}
