//! Process-level contracts for the cataloged coverage hooks.

#![cfg(unix)]

use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::{fs, io};

use tempfile::TempDir;

#[path = "support/hook_catalog.rs"]
mod hook_catalog;

/// Records each Cargo invocation, writes a fresh report on request, and exits
/// with the configured status.
const CARGO_STUB: &str = r#"#!/bin/sh
case "$1" in
llvm-cov)
  printf '%s\0' "$@" > coverage-arguments
  printf '%s\n' "${RUSTFLAGS-unset}" "${CARGO_LLVM_COV_TARGET_DIR-unset}" > coverage-environment
  report=
  while [ "$#" -gt 0 ]; do
    if [ "$1" = --output-path ]; then report=$2; fi
    shift
  done
  if [ -n "${AGENTTY_TEST_WRITE_REPORT:-}" ]; then
    mkdir -p "$(dirname "$report")"
    printf 'SF:fresh\n' > "$report"
  fi
  exit "${AGENTTY_TEST_COVERAGE_EXIT:-0}";;
run)
  printf '%s\0' "$@" > check-arguments
  printf '%s\n' "${RUSTFLAGS-unset}" "${CARGO_LLVM_COV_TARGET_DIR-unset}" > check-environment
  exit "${AGENTTY_TEST_CHECK_EXIT:-0}";;
esac
exit 99
"#;

const FOCUSED_REPORT: &str = "target/coverage-focused.lcov";

struct Fixture {
    directory: TempDir,
    entry: String,
}

impl Fixture {
    fn new(hook: &str) -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let cargo = directory.path().join("bin/cargo");
        fs::create_dir(directory.path().join("bin"))?;
        fs::write(&cargo, CARGO_STUB)?;
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))?;
        let entry = hook_catalog::entry(hook)?;

        Ok(Self { directory, entry })
    }

    fn run(&self, environment: &[(&str, &str)]) -> io::Result<Output> {
        let mut command = Command::new("sh");
        command
            .args(["-c", &self.entry])
            .current_dir(self.directory.path())
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .envs(environment.iter().copied());
        for name in [
            "AGENTTY_COVERAGE_PATHS",
            "AGENTTY_TEST_FILTER",
            "AGENTTY_TEST_PACKAGES",
            "CARGO_LLVM_COV_TARGET_DIR",
            "RUSTFLAGS",
        ] {
            if !environment.iter().any(|(key, _)| *key == name) {
                command.env_remove(name);
            }
        }

        command.output()
    }

    fn arguments(&self, name: &str) -> Result<Vec<String>, Box<dyn Error>> {
        let bytes = fs::read(self.path(name))?;

        Ok(bytes
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8(argument.to_vec()))
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn lines(&self, name: &str) -> io::Result<Vec<String>> {
        Ok(fs::read_to_string(self.path(name))?
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
}

fn values_after<'arguments>(arguments: &'arguments [String], flag: &str) -> Vec<&'arguments str> {
    arguments
        .windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
        .collect()
}

#[test]
fn coverage_hook_checks_its_fresh_report_without_coverage_build_settings()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new("coverage")?;
    fs::write(fixture.path("coverage.lcov"), "stale")?;

    // Act
    let output = fixture.run(&[
        ("AGENTTY_TEST_WRITE_REPORT", "1"),
        ("RUSTFLAGS", "-C ambient"),
    ])?;

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let coverage = fixture.arguments("coverage-arguments")?;
    assert!(coverage.iter().any(|argument| argument == "--workspace"));
    assert!(coverage.iter().any(|argument| argument == "--lcov"));
    assert_eq!(values_after(&coverage, "--profile"), ["ci"]);
    assert_eq!(values_after(&coverage, "--output-path"), ["coverage.lcov"]);
    assert!(
        !coverage
            .iter()
            .any(|argument| argument.starts_with("--fail-under"))
    );
    let coverage_environment = fixture.lines("coverage-environment")?;
    assert!(
        coverage_environment[0].starts_with("-C ambient -C llvm-args=-runtime-counter-relocation")
    );
    assert_eq!(coverage_environment[1], "target/llvm-cov-target");
    assert_eq!(
        fixture.arguments("check-arguments")?,
        [
            "run",
            "-q",
            "--locked",
            "-p",
            "ag-xtask",
            "--",
            "check-coverage",
            "coverage.lcov"
        ]
    );
    assert_eq!(fixture.lines("check-environment")?, ["-C ambient", "unset"]);
    assert_eq!(
        fs::read_to_string(fixture.path("coverage.lcov"))?,
        "SF:fresh\n"
    );

    Ok(())
}

#[test]
fn focused_hook_forwards_packages_filters_and_paths_literally() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new("coverage-focused")?;
    let filter = "package(=ag-git) and test($(touch should-not-exist))";
    fs::write(fixture.path("local.rs"), "")?;

    // Act
    let output = fixture.run(&[
        ("AGENTTY_TEST_WRITE_REPORT", "1"),
        ("AGENTTY_TEST_PACKAGES", "ag-git\t ag-forge\n"),
        ("AGENTTY_TEST_FILTER", filter),
        ("AGENTTY_COVERAGE_PATHS", " crates/ag-git/src\n*.rs "),
        ("CARGO_LLVM_COV_TARGET_DIR", "target/relocated"),
    ])?;

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let coverage = fixture.arguments("coverage-arguments")?;
    assert_eq!(values_after(&coverage, "-p"), ["ag-git", "ag-forge"]);
    assert!(!coverage.iter().any(|argument| argument == "--workspace"));
    assert_eq!(values_after(&coverage, "--profile"), ["local"]);
    assert_eq!(values_after(&coverage, "--output-path"), [FOCUSED_REPORT]);
    assert!(
        coverage
            .iter()
            .any(|argument| argument == "--no-tests=fail")
    );
    assert_eq!(coverage[coverage.len() - 2..], ["-E", filter]);
    assert!(!fixture.path("should-not-exist").exists());
    assert_eq!(
        fixture.lines("coverage-environment")?[1],
        "target/relocated"
    );
    let check = fixture.arguments("check-arguments")?;
    assert_eq!(
        check[6..],
        [
            "check-coverage",
            FOCUSED_REPORT,
            "--path",
            "crates/ag-git/src",
            "--path",
            "*.rs"
        ]
    );
    assert_eq!(
        fixture.lines("check-environment")?,
        ["unset", "target/relocated"]
    );

    Ok(())
}

#[test]
fn focused_hook_defaults_to_all_tests_and_every_reported_file() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new("coverage-focused")?;

    // Act
    let output = fixture.run(&[
        ("AGENTTY_TEST_WRITE_REPORT", "1"),
        ("AGENTTY_TEST_PACKAGES", "ag-xtask"),
    ])?;

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let coverage = fixture.arguments("coverage-arguments")?;
    assert!(!coverage.iter().any(|argument| argument == "-E"));
    assert_eq!(
        fixture.lines("coverage-environment")?[1],
        "target/llvm-cov-target"
    );
    assert_eq!(
        fixture.arguments("check-arguments")?[6..],
        ["check-coverage", FOCUSED_REPORT]
    );

    Ok(())
}

#[test]
fn focused_hook_requires_package_names_before_running_coverage() -> Result<(), Box<dyn Error>> {
    for packages in [None, Some(""), Some(" \t\n ")] {
        // Arrange
        let fixture = Fixture::new("coverage-focused")?;
        let environment: Vec<_> = packages
            .map(|packages| ("AGENTTY_TEST_PACKAGES", packages))
            .into_iter()
            .collect();

        // Act
        let output = fixture.run(&environment)?;

        // Assert
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Set AGENTTY_TEST_PACKAGES"));
        assert!(!fixture.path("coverage-arguments").exists());
    }

    Ok(())
}

#[test]
fn generation_and_check_failures_propagate_and_stale_reports_never_pass()
-> Result<(), Box<dyn Error>> {
    for (hook, report) in [
        ("coverage", "coverage.lcov"),
        ("coverage-focused", FOCUSED_REPORT),
    ] {
        for (write_report, coverage_exit, check_exit, expected_exit, checked) in [
            ("1", "42", "0", 42, false),
            ("", "0", "0", 1, false),
            ("1", "0", "7", 7, true),
        ] {
            // Arrange
            let fixture = Fixture::new(hook)?;
            fs::create_dir(fixture.path("target"))?;
            fs::write(fixture.path(report), "stale")?;

            // Act
            let output = fixture.run(&[
                ("AGENTTY_TEST_PACKAGES", "ag-xtask"),
                ("AGENTTY_TEST_WRITE_REPORT", write_report),
                ("AGENTTY_TEST_COVERAGE_EXIT", coverage_exit),
                ("AGENTTY_TEST_CHECK_EXIT", check_exit),
            ])?;

            // Assert
            assert_eq!(output.status.code(), Some(expected_exit), "{hook}");
            assert_eq!(fixture.path("check-arguments").exists(), checked, "{hook}");
            assert_ne!(
                fs::read_to_string(fixture.path(report)).ok().as_deref(),
                Some("stale"),
                "{hook}"
            );
        }
    }

    Ok(())
}
