//! `ag-release prepare` command-line contract, with a real `cargo update`.

use std::error::Error;
use std::fs;

use crate::support::{ag_release, demo_workspace, result};

/// Root manifest of a demo workspace whose app depends on a library member.
const DEMO_MANIFEST: &str = r#"[workspace]
members = ["demo-app", "demo-lib"]
resolver = "2"

[workspace.dependencies]
# Kept in step with the release.
demo-lib = { path = "demo-lib", version = "0.1.0" }

[workspace.metadata.release]
installers = ["shell"]

[workspace.metadata.release.targets]
x86_64-unknown-linux-gnu = "ubuntu-22.04"

[workspace.metadata.release.apps.demo-app]
"#;

#[test]
fn prepare_bumps_the_workspace_lock_file_and_changelog() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workspace = demo_workspace(DEMO_MANIFEST, &["demo-app", "demo-lib"])?;
    let root = workspace.path();
    fs::write(
        root.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n- Demo.\n\n## [v0.1.0] - 2026-01-01\n",
    )?;

    // Act
    let prepared = ag_release(root, &["prepare", "v0.2.0", "--date", "2026-02-03"]).output()?;
    let repeated = ag_release(root, &["prepare", "0.2.0", "--date", "2026-02-03"]).output()?;

    // Assert
    let written = result(&prepared)?;
    // `cargo metadata` reports the workspace root with symbolic links resolved.
    let canonical_root = root.canonicalize()?;
    let root_display = canonical_root.display();
    assert_eq!(
        written,
        format!(
            "{root_display}/Cargo.toml\n{root_display}/demo-app/Cargo.toml\n{root_display}/\
             demo-lib/Cargo.toml\n{root_display}/Cargo.lock\n{root_display}/CHANGELOG.md\n"
        )
    );
    assert_eq!(
        fs::read_to_string(root.join("Cargo.toml"))?,
        DEMO_MANIFEST.replace("version = \"0.1.0\"", "version = \"0.2.0\"")
    );
    assert!(
        fs::read_to_string(root.join("demo-app/Cargo.toml"))?.contains("\nversion = \"0.2.0\"\n")
    );
    let lock = fs::read_to_string(root.join("Cargo.lock"))?;
    assert!(
        lock.contains("name = \"demo-app\"\nversion = \"0.2.0\"\n"),
        "{lock}"
    );
    assert!(
        lock.contains("name = \"demo-lib\"\nversion = \"0.2.0\"\n"),
        "{lock}"
    );
    assert_eq!(
        fs::read_to_string(root.join("CHANGELOG.md"))?,
        "# Changelog\n\n## [Unreleased]\n\n## [v0.2.0] - 2026-02-03\n\n- Demo.\n\n## [v0.1.0] - \
         2026-01-01\n"
    );
    assert_eq!(
        result(&repeated),
        Err("error: version 0.2.0 must be newer than the apps' version 0.2.0\n".to_string())
    );

    Ok(())
}
