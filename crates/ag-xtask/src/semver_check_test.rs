//! Published-crate selection and crates.io baseline unit tests.

use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Output};

use super::{Baseline, Outcome, publishable_libraries};

const METADATA: &str = r#"{"packages": [
    {"name": "core", "publish": null, "targets": [{"kind": ["lib"]}]},
    {"name": "app", "publish": null, "targets": [{"kind": ["lib"]}, {"kind": ["bin"]}]},
    {"name": "tool", "publish": null, "targets": [{"kind": ["bin"]}]},
    {"name": "internal", "publish": [], "targets": [{"kind": ["lib"]}]},
    {"name": "mirrored", "publish": ["crates-io"], "targets": [{"kind": ["lib"]}]}
]}"#;

fn lookup(code: i32, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn publishable_libraries_skip_unpublished_binary_only_and_excluded_packages() {
    // Arrange
    let exclude = vec!["app".to_string()];
    // Act
    let names = publishable_libraries(METADATA.as_bytes(), &exclude);
    // Assert
    assert_eq!(names, Ok(vec!["core".to_string(), "mirrored".to_string()]));
}

#[test]
fn publishable_libraries_reject_an_unknown_exclusion() {
    // Arrange
    let exclude = vec!["missing".to_string()];
    // Act
    let names = publishable_libraries(METADATA.as_bytes(), &exclude);
    // Assert
    assert_eq!(
        names,
        Err("Excluded package missing is not a workspace package".to_string())
    );
}

#[test]
fn publishable_libraries_reject_malformed_metadata() {
    // Arrange
    let exclude = Vec::new();
    // Act
    let unparsable = publishable_libraries(b"not json", &exclude);
    let without_packages = publishable_libraries(b"{}", &exclude);
    // Assert
    assert!(unparsable.is_err_and(|error| error.starts_with("Could not parse cargo metadata")));
    assert_eq!(
        without_packages,
        Err("cargo metadata has no package list".to_string())
    );
}

#[test]
fn a_successful_lookup_has_a_baseline() {
    // Arrange
    let output = lookup(0, "");
    // Act
    let baseline = Baseline::from_lookup("core", &output);
    // Assert
    assert_eq!(baseline, Ok(Baseline::Published));
}

#[test]
fn a_crate_crates_io_does_not_list_has_no_baseline() {
    // Arrange
    let output = lookup(
        101,
        "error: could not find `core` in registry `https://github.com/rust-lang/crates.io-index`",
    );
    // Act
    let baseline = Baseline::from_lookup("core", &output);
    // Assert
    assert_eq!(baseline, Ok(Baseline::Missing));
}

#[test]
fn other_lookup_failures_are_errors() {
    // Arrange
    let output = lookup(101, "error: failed to query the index: timed out\n");
    // Act
    let baseline = Baseline::from_lookup("core", &output);
    // Assert
    assert_eq!(
        baseline,
        Err(
            "Could not look up core on crates.io: error: failed to query the index: timed out"
                .to_string()
        )
    );
}

#[test]
fn a_successful_run_passes() {
    // Arrange
    let report = "    Building core v1.0.0 (current)\n     Summary no semver update required\n";
    // Act
    let outcome = Outcome::from_report("core", true, report);
    // Assert
    assert_eq!(outcome, Outcome::Passed);
}

#[test]
fn a_rustdoc_failure_after_the_baseline_build_starts_is_the_baselines() {
    // Arrange
    let report = "    Building core v1.0.0 (current)\n       Built [ 1.0s] (current)\n    \
                  Building core v1.0.0 (baseline)\nerror: failed to build rustdoc for crate core \
                  v1.0.0\n";
    // Act
    let outcome = Outcome::from_report("core", false, report);
    // Assert
    assert_eq!(outcome, Outcome::BrokenBaseline);
}

#[test]
fn a_rustdoc_failure_before_the_baseline_build_is_the_workspace_crates() {
    // Arrange
    let report = "    Building core v1.0.0 (current)\nerror: failed to build rustdoc for crate \
                  core v1.0.0\n    Building other v1.0.0 (baseline)\n";
    // Act
    let outcome = Outcome::from_report("core", false, report);
    // Assert
    assert_eq!(outcome, Outcome::BrokenCurrent);
}

#[test]
fn a_failure_without_a_rustdoc_error_is_a_breaking_change() {
    // Arrange
    let report = "    Building core v1.0.0 (baseline)\n--- failure enum_variant_added ---\n     \
                  Summary semver requires new major version\n";
    // Act
    let outcome = Outcome::from_report("core", false, report);
    // Assert
    assert_eq!(outcome, Outcome::Breaking);
}
