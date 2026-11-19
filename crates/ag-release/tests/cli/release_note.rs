//! `ag-release notes` command-line contract.

use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::support::{ag_release, demo_workspace, result};

/// Root manifest of a one-app demo workspace with both installers.
const DEMO_MANIFEST: &str = r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
installers = ["shell", "npm"]

[workspace.metadata.release.targets]
x86_64-unknown-linux-gnu = "ubuntu-22.04"

[workspace.metadata.release.apps.demo-app]
"#;

/// Changelog of the demo workspace with an entry for its version.
const DEMO_CHANGELOG: &str = r"# Changelog

## [v0.1.0] - 2026-01-02

### Added

- First release.
";

#[test]
fn notes_print_the_release_title_or_body_from_the_changelog() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workspace = demo_workspace(DEMO_MANIFEST, &["demo-app"])?;
    fs::write(workspace.path().join("CHANGELOG.md"), DEMO_CHANGELOG)?;
    let notes = |args: &[&str]| -> Result<Result<String, String>, Box<dyn Error>> {
        Ok(result(&ag_release(workspace.path(), args).output()?))
    };

    // Act
    let title = notes(&["notes", "--tag", "v0.1.0", "--title"])?;
    let body = notes(&["notes", "--tag", "v0.1.0"])?;
    let mismatched = notes(&["notes", "--tag", "v0.2.0"])?;

    // Assert
    assert_eq!(title, Ok("v0.1.0 - 2026-01-02\n".to_string()));
    let body = body?;
    let download = "https://github.com/demo-org/demo-app/releases/download/v0.1.0";
    let expected_lines = [
        "## Release Notes\n\n### Added\n\n- First release.\n\n# demo-app 0.1.0\n".to_string(),
        format!("curl --proto '=https' --tlsv1.2 -LsSf {download}/demo-app-installer.sh | sh\n"),
        "npm install demo-app@0.1.0\n".to_string(),
        format!(
            "| [demo-app-x86_64-unknown-linux-gnu.tar.gz]({download}/\
             demo-app-x86_64-unknown-linux-gnu.tar.gz) | x64 Linux | \
             [checksum]({download}/demo-app-x86_64-unknown-linux-gnu.tar.gz.sha256) |\n"
        ),
        "--repo demo-org/demo-app\n".to_string(),
    ];
    for expected in &expected_lines {
        assert!(body.contains(expected.as_str()), "{expected}\n---\n{body}");
    }
    assert!(body.starts_with(&expected_lines[0]), "{body}");
    assert!(body.ends_with("```\n"), "{body}");
    assert_eq!(
        mismatched,
        Err(
            "error: release tag `v0.2.0` names version 0.2.0, but the apps have version 0.1.0\n"
                .to_string()
        )
    );

    Ok(())
}

#[test]
fn notes_render_this_repository_release() -> Result<(), Box<dyn Error>> {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let planned = result(&ag_release(root, &["plan"]).output()?)?;
    let plan: Value = serde_json::from_str(&planned)?;
    let version = plan["version"].as_str().ok_or("plan version")?;
    let tag = format!("v{version}");

    // Act
    let title = result(&ag_release(root, &["notes", "--tag", &tag, "--title"]).output()?)?;
    let body = result(&ag_release(root, &["notes", "--tag", &tag]).output()?)?;

    // Assert
    assert!(
        title == format!("{tag}\n") || title.starts_with(&format!("{tag} - ")),
        "{title}"
    );
    assert_eq!(title.lines().count(), 1, "{title}");
    let apps = plan["apps"].as_array().ok_or("plan apps")?;
    let mut section_start = 0;
    for app in apps {
        let name = app["name"].as_str().ok_or("app name")?;
        let heading = format!("# {name} {version}\n");
        let found = body[section_start..]
            .find(&heading)
            .ok_or_else(|| format!("missing `{heading}`"))?;
        section_start += found + heading.len();
        let archives = app["archives"].as_array().ok_or("app archives")?;
        for archive in archives {
            let name = archive["name"].as_str().ok_or("archive name")?;
            assert!(body.contains(&format!("| [{name}](")), "{name}\n{body}");
        }
    }
    let release_notes = body.starts_with("## Release Notes\n\n");
    let first_app = apps
        .first()
        .and_then(|app| app["name"].as_str())
        .ok_or("first app")?;
    assert!(
        release_notes || body.starts_with(&format!("# {first_app} {version}\n")),
        "{body}"
    );
    assert!(body.ends_with("```\n"), "{body}");

    Ok(())
}
