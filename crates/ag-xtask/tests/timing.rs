//! Public timing-comparison CLI contract.

use std::error::Error;
use std::fs;
use std::process::Command;

use serde_json::Value;

#[path = "support/timing.rs"]
mod support;

#[test]
fn compares_real_artifacts_and_fails_on_unreadable_input() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    for (path, contents) in support::files() {
        let destination = directory.path().join(path);
        fs::create_dir_all(destination.parent().ok_or("missing parent")?)?;
        fs::write(destination, contents)?;
    }
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .current_dir(directory.path())
            .args([
                "compare-timings",
                "--before",
                "before/0/metadata.json",
                "before/1/metadata.json",
                "before/2/metadata.json",
                "--after",
                "after/0/metadata.json",
                "after/1/metadata.json",
                "after/2/metadata.json",
            ])
            .output()
    };
    // Act
    let success = run()?;
    fs::remove_file(directory.path().join("before/0/junit.xml"))?;
    let failure = run()?;
    // Assert
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stdout)
    );
    let report: Value = serde_json::from_slice(&success.stdout)?;
    assert_eq!(report["suite"], "workspace");
    assert_eq!(report["metrics"]["hook_seconds"]["after"]["median"], 7.0);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stdout).contains("before/0/junit.xml"));

    Ok(())
}
