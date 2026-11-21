//! Published-crate selection and crates.io baseline unit tests.

use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Output};

use super::{Baseline, publishable_libraries};

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
