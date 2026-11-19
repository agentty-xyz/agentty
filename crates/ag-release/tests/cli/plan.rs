//! `ag-release plan` command-line contract.

use std::error::Error;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{ag_release, demo_workspace};

/// Root manifest of a one-app demo workspace.
const DEMO_MANIFEST: &str = r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
include = ["README.md"]
installers = ["shell"]

[workspace.metadata.release.targets]
x86_64-unknown-linux-gnu = "ubuntu-22.04"

[workspace.metadata.release.apps.demo-app]
"#;

#[test]
fn plan_prints_json_on_stdout_and_fails_with_an_error_on_stderr() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workspace = demo_workspace(DEMO_MANIFEST, &["demo-app"])?;

    // Act
    let planned = ag_release(workspace.path(), &["plan", "--tag", "v0.1.0"]).output()?;
    let mismatched = ag_release(workspace.path(), &["plan", "--tag", "v0.2.0"]).output()?;

    // Assert
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan: Value = serde_json::from_slice(&planned.stdout)?;
    assert_eq!(plan["tag"], "v0.1.0");
    assert_eq!(plan["apps"][0]["binaries"], json!(["demo-app"]));
    assert_eq!(plan["apps"][0]["shell_installer"], "demo-app-installer.sh");
    assert_eq!(
        plan["matrix"]["include"][0]["target"],
        "x86_64-unknown-linux-gnu"
    );
    assert!(!mismatched.status.success());
    assert_eq!(String::from_utf8_lossy(&mismatched.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&mismatched.stderr),
        "error: release tag `v0.2.0` names version 0.2.0, but the apps have version 0.1.0\n"
    );

    Ok(())
}

#[test]
fn plan_accepts_this_repository_configuration() -> io::Result<()> {
    // Act
    let planned = ag_release(Path::new(env!("CARGO_MANIFEST_DIR")), &["plan"]).output()?;

    // Assert
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );

    Ok(())
}
