//! Public CLI coverage for workspace manifest validation.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

#[test]
fn cli_accepts_this_workspace_and_rejects_an_uninherited_dependency() {
    // Arrange
    let directory = tempdir().expect("workspace");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    )
    .expect("workspace manifest");
    fs::create_dir_all(directory.path().join("member/src")).expect("member sources");
    fs::write(
        directory.path().join("member/Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \
         \"2024\"\n\n[dependencies]\nlocal = { path = \"../local\" }\n",
    )
    .expect("member manifest");
    fs::write(directory.path().join("member/src/lib.rs"), "").expect("member library");
    fs::create_dir_all(directory.path().join("local/src")).expect("local sources");
    fs::write(
        directory.path().join("local/Cargo.toml"),
        "[package]\nname = \"local\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("local manifest");
    fs::write(directory.path().join("local/src/lib.rs"), "").expect("local library");
    let lockfile = Command::new(env!("CARGO"))
        .args(["generate-lockfile", "--offline"])
        .current_dir(directory.path())
        .status()
        .expect("cargo generate-lockfile");
    assert!(lockfile.success());
    // Run without `CARGO`, as when invoked outside Cargo, so the checker
    // falls back to `cargo` on `PATH`.
    let run = |current_dir: &Path| {
        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .current_dir(current_dir)
            .arg("check-manifests")
            .env_remove("CARGO")
            .output()
            .expect("manifest checker")
    };

    // Act
    let workspace = run(Path::new(env!("CARGO_MANIFEST_DIR")));
    let uninherited = run(directory.path());

    // Assert
    assert!(
        workspace.status.success(),
        "{}",
        String::from_utf8_lossy(&workspace.stdout)
    );
    assert!(!uninherited.status.success());
    assert!(
        String::from_utf8_lossy(&uninherited.stdout)
            .contains("member/Cargo.toml: [dependencies] `local` must use `workspace = true`")
    );
}
