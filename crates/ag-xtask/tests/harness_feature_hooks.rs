//! Process-level contracts for the `ag-harness` reduced-feature hooks.

#![cfg(unix)]

use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::{fs, io};

use tempfile::TempDir;

#[path = "support/hook_catalog.rs"]
mod hook_catalog;

/// Logs each Cargo invocation with the settings that select its features
/// and coverage build, writes a fresh report on request, and exits with the
/// configured status.
const CARGO_STUB: &str = r#"#!/bin/sh
{
  printf '%s\t%s\t%s' "${CARGO_RESOLVER_FEATURE_UNIFICATION-unset}" "${RUSTFLAGS-unset}" \
    "${CARGO_LLVM_COV_TARGET_DIR-unset}"
  printf '\t%s' "$@"
  printf '\n'
} >> invocations
case "$1" in
clippy) exit "${AGENTTY_TEST_CLIPPY_EXIT:-0}";;
llvm-cov)
  if [ "$2" = report ]; then
    report=
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --output-path ]; then report=$2; fi
      shift
    done
    if [ -n "${AGENTTY_TEST_WRITE_REPORT:-}" ]; then
      mkdir -p "$(dirname "$report")"
      printf 'SF:fresh\n' > "$report"
    fi
    exit 0
  fi
  if [ "$2" = nextest ]; then exit "${AGENTTY_TEST_COVERAGE_EXIT:-0}"; fi
  exit 0;;
run) exit "${AGENTTY_TEST_CHECK_EXIT:-0}";;
esac
exit 99
"#;

const REPORT: &str = "target/coverage-ag-harness-features.lcov";

/// One logged Cargo invocation.
struct Invocation {
    arguments: Vec<String>,
    target_directory: String,
    rustflags: String,
    unification: String,
}

impl Invocation {
    fn has(&self, argument: &str) -> bool {
        self.arguments.iter().any(|candidate| candidate == argument)
    }

    /// Returns the value of `--features`, or an empty string without it.
    fn features(&self) -> &str {
        self.arguments
            .windows(2)
            .find(|pair| pair[0] == "--features")
            .map_or("", |pair| pair[1].as_str())
    }
}

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
            "CARGO_LLVM_COV_TARGET_DIR",
            "CARGO_RESOLVER_FEATURE_UNIFICATION",
            "RUSTFLAGS",
        ] {
            if !environment.iter().any(|(key, _)| *key == name) {
                command.env_remove(name);
            }
        }

        command.output()
    }

    fn invocations(&self) -> Result<Vec<Invocation>, Box<dyn Error>> {
        let Ok(log) = fs::read_to_string(self.path("invocations")) else {
            return Ok(Vec::new());
        };

        log.lines()
            .map(|line| {
                let mut fields = line.split('\t').map(str::to_string);
                let mut field = || fields.next().ok_or("truncated invocation");
                let unification = field()?;
                let rustflags = field()?;
                let target_directory = field()?;

                Ok(Invocation {
                    arguments: fields.collect(),
                    target_directory,
                    rustflags,
                    unification,
                })
            })
            .collect()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
}

#[test]
fn feature_lint_builds_no_features_and_each_gating_feature_alone_unified_selectively()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new("clippy-ag-harness-features")?;

    // Act
    let output = fixture.run(&[("CARGO_RESOLVER_FEATURE_UNIFICATION", "workspace")])?;

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = fixture.invocations()?;
    for invocation in &invocations {
        assert_eq!(invocation.arguments[0], "clippy");
        assert_eq!(invocation.unification, "selected");
        assert!(invocation.has("--no-default-features"));
        assert!(invocation.has("--all-targets"));
        assert_eq!(
            invocation.arguments[invocation.arguments.len() - 2..],
            ["-D", "warnings"]
        );
    }
    assert_eq!(
        invocations
            .iter()
            .map(Invocation::features)
            .collect::<Vec<_>>(),
        [
            "",
            "native-sandbox",
            "otel",
            "providers-compat",
            "providers-openai",
            "sqlite"
        ]
    );

    Ok(())
}

#[test]
fn feature_lint_stops_at_the_first_failing_build() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new("clippy-ag-harness-features")?;

    // Act
    let output = fixture.run(&[("AGENTTY_TEST_CLIPPY_EXIT", "3")])?;

    // Assert
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(fixture.invocations()?.len(), 1);

    Ok(())
}

#[test]
fn feature_coverage_merges_every_reduced_build_and_checks_without_build_settings()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new("coverage-ag-harness-features")?;

    // Act
    let output = fixture.run(&[
        ("AGENTTY_TEST_WRITE_REPORT", "1"),
        ("CARGO_RESOLVER_FEATURE_UNIFICATION", "workspace"),
        ("RUSTFLAGS", "-C ambient"),
    ])?;

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = fixture.invocations()?;
    let (check, coverage) = invocations.split_last().ok_or("no invocations")?;
    for invocation in coverage {
        assert_eq!(invocation.arguments[0], "llvm-cov");
        assert_eq!(invocation.unification, "selected");
        assert!(
            invocation
                .rustflags
                .starts_with("-C ambient -C llvm-args=-runtime-counter-relocation")
        );
        assert_eq!(invocation.target_directory, "target/llvm-cov-target");
    }
    assert_eq!(coverage[0].arguments[1..], ["clean", "--workspace"]);
    let runs = &coverage[1..coverage.len() - 1];
    for run in runs {
        assert_eq!(run.arguments[1], "nextest");
        assert!(run.has("--no-report"));
        assert!(run.has("--no-default-features"));
        assert!(run.has("--lib") && run.has("--tests"));
    }
    assert_eq!(
        runs.iter().map(Invocation::features).collect::<Vec<_>>(),
        [
            "",
            "providers-openai",
            "providers-compat",
            "providers-compat,providers-openai"
        ]
    );
    let report = &coverage[coverage.len() - 1];
    assert_eq!(report.arguments[1], "report");
    assert!(report.has("--lcov"));
    assert_eq!(
        check.arguments,
        [
            "run",
            "-q",
            "--locked",
            "-p",
            "ag-xtask",
            "--",
            "check-coverage",
            REPORT,
            "--path",
            "crates/ag-harness/"
        ]
    );
    assert_eq!(check.unification, "unset");
    assert_eq!(check.rustflags, "-C ambient");
    assert_eq!(check.target_directory, "unset");
    assert_eq!(fs::read_to_string(fixture.path(REPORT))?, "SF:fresh\n");

    Ok(())
}

#[test]
fn feature_coverage_failures_propagate_and_stale_reports_never_pass() -> Result<(), Box<dyn Error>>
{
    for (write_report, coverage_exit, check_exit, expected_exit, checked) in [
        ("1", "42", "0", 42, false),
        ("", "0", "0", 1, false),
        ("1", "0", "7", 7, true),
    ] {
        // Arrange
        let fixture = Fixture::new("coverage-ag-harness-features")?;
        fs::create_dir(fixture.path("target"))?;
        fs::write(fixture.path(REPORT), "stale")?;

        // Act
        let output = fixture.run(&[
            ("AGENTTY_TEST_WRITE_REPORT", write_report),
            ("AGENTTY_TEST_COVERAGE_EXIT", coverage_exit),
            ("AGENTTY_TEST_CHECK_EXIT", check_exit),
        ])?;

        // Assert
        assert_eq!(output.status.code(), Some(expected_exit));
        let invocations = fixture.invocations()?;
        assert_eq!(
            invocations
                .iter()
                .any(|invocation| invocation.arguments[0] == "run"),
            checked
        );
        assert_ne!(
            fs::read_to_string(fixture.path(REPORT)).ok().as_deref(),
            Some("stale")
        );
    }

    Ok(())
}
