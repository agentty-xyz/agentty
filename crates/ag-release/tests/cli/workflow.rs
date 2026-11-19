//! `ag-release workflow` command-line contract.

use std::error::Error;
use std::path::Path;
use std::{fs, io};

use crate::support::{ag_release, demo_workspace, result};

/// Root manifest of a one-app demo workspace that installs `ag-release`
/// from Git.
const DEMO_MANIFEST: &str = r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
installers = ["shell"]

[workspace.metadata.release.targets]
x86_64-unknown-linux-gnu = "ubuntu-22.04"

[workspace.metadata.release.apps.demo-app]

[workspace.metadata.release.workflow.tool]
git = "https://example.com/demo-org/ag-release"
rev = "0123456789abcdef0123456789abcdef01234567"
"#;

#[test]
fn workflow_writes_the_file_that_check_then_accepts_until_it_changes() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let workspace = demo_workspace(DEMO_MANIFEST)?;
    let root = workspace.path();
    let path = root.join(".github/workflows/release.yml");

    // Act
    let missing = ag_release(root, &["workflow", "--check"]).output()?;
    let written = ag_release(root, &["workflow"]).output()?;
    let current = ag_release(root, &["workflow", "--check"]).output()?;
    fs::write(&path, format!("{}# edited\n", fs::read_to_string(&path)?))?;
    let edited = ag_release(root, &["workflow", "--check"]).output()?;

    // Assert
    assert_eq!(
        result(&missing),
        Err(
            "error: `.github/workflows/release.yml` is missing; run `ag-release workflow` and \
             commit the result\n"
                .to_string()
        )
    );
    let written_path = result(&written)?;
    assert_eq!(
        fs::canonicalize(written_path.trim_end())?,
        fs::canonicalize(&path)?
    );
    assert_eq!(
        result(&current),
        Ok("`.github/workflows/release.yml` is up to date\n".to_string())
    );
    assert!(!edited.status.success());
    assert!(
        String::from_utf8_lossy(&edited.stderr)
            .contains("`.github/workflows/release.yml` differs from the workflow")
    );

    Ok(())
}

#[test]
fn workflow_check_accepts_this_repository_workflow() -> io::Result<()> {
    // Act
    let checked = ag_release(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &["workflow", "--check"],
    )
    .output()?;

    // Assert
    assert_eq!(
        result(&checked),
        Ok("`.github/workflows/release.yml` is up to date\n".to_string())
    );

    Ok(())
}
