//! Public slow-test report and ratchet CLI contract.

use std::error::Error;
use std::fs;
use std::process::{Command, Output};

const JUNIT: &str = r#"<testsuites tests="2" failures="0" errors="0" time="7.5"><testsuite>
<testcase name="app::tests::quick" classname="agentty" time="0.004"/>
<testcase name="app::tests::slow" classname="agentty" time="7"/>
</testsuite></testsuites>"#;

const ALLOWLIST: &str = "- binary: agentty
  test: app::tests::slow
  reason: Fixture offender.
";

#[test]
fn reports_checks_and_fails_on_new_offenders_or_unreadable_input() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("junit.xml"), JUNIT)?;
    fs::write(directory.path().join("allowlist.yaml"), ALLOWLIST)?;
    let run = |arguments: &[&str]| -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .current_dir(directory.path())
            .arg("slow-tests")
            .args(arguments)
            .output()?)
    };

    // Act
    let report = run(&["junit.xml", "--top", "1", "--module-depth", "1"])?;
    let allowed = run(&[
        "junit.xml",
        "--max-seconds",
        "5",
        "--allowlist",
        "allowlist.yaml",
    ])?;
    let rejected = run(&["junit.xml", "--max-seconds", "5"])?;
    let missing = run(&["absent.xml"])?;
    let unpaired = run(&["junit.xml", "--allowlist", "allowlist.yaml"])?;
    let negative = run(&["junit.xml", "--max-seconds", "-1"])?;

    // Assert
    assert!(report.status.success());
    let report = String::from_utf8(report.stdout)?;
    assert!(
        report.contains("Slowest tests (top 1 of 2):\n    7.000 s  agentty  app::tests::slow\n")
    );
    assert!(report.contains("Modules at depth 1"));
    assert!(allowed.status.success());
    assert!(String::from_utf8(allowed.stdout)?.contains("1 over, 1 allowlisted, 0 new"));
    assert!(!rejected.status.success());
    assert!(String::from_utf8(rejected.stdout)?.contains("agentty  app::tests::slow"));
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stdout)?.contains("absent.xml"));
    assert!(!unpaired.status.success());
    assert!(!negative.status.success());

    Ok(())
}
