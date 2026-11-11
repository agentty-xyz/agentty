//! Public CLI coverage for the complete line and function coverage gate.

use std::error::Error;
use std::path::Path;
use std::process::{Command, Output};
use std::{fs, io};

use tempfile::tempdir;

fn check_coverage(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
        .current_dir(root)
        .arg("check-coverage")
        .args(arguments)
        .output()
}

#[test]
fn cli_reports_gaps_relative_to_the_repository_and_rejects_unusable_reports()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let root = directory.path().canonicalize()?;
    let record = |path: &str, count: u32| {
        let hit = count.min(1);
        [
            format!("SF:{}", root.join(path).display()),
            "FN:1,ag_xtask::main".to_string(),
            format!("FNDA:{count},ag_xtask::main"),
            "FNF:1".to_string(),
            format!("FNH:{hit}"),
            format!("DA:1,{count}"),
            "LF:1".to_string(),
            format!("LH:{hit}"),
            "end_of_record\n".to_string(),
        ]
        .join("\n")
    };
    let report = format!(
        "{}{}",
        record("crates/covered/src/main.rs", 2),
        record("crates/gap/src/main.rs", 0)
    );
    fs::write(root.join("coverage.lcov"), report)?;

    // Act
    let complete = check_coverage(&root, &["coverage.lcov", "--path", "crates/covered"])?;
    let incomplete = check_coverage(&root, &["coverage.lcov"])?;
    let missing = check_coverage(&root, &["missing.lcov"])?;

    // Assert
    assert!(complete.status.success());
    assert!(String::from_utf8_lossy(&complete.stdout).contains("Coverage complete: 1 files"));
    assert!(!incomplete.status.success());
    let incomplete = String::from_utf8_lossy(&incomplete.stdout);
    let gap = [
        "crates/gap/src/main.rs",
        "  uncovered lines: 1",
        "  uncovered functions (start line, name):",
        "    1 ag_xtask::main",
    ];
    assert!(incomplete.contains(&gap.join("\n")));
    assert!(incomplete.contains("Coverage incomplete in 1 of 2 files"));
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stdout).contains("Failed to read missing.lcov"));

    Ok(())
}
