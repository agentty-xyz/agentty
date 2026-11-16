//! Cargo argument selection unit tests for affected packages and PTY targets.

use super::cargo_arguments;

#[test]
fn selected_packages_are_linted_individually_and_no_selection_lints_the_workspace() {
    // Arrange
    let packages = vec!["agentty".to_string(), "ag-git".to_string()];
    let strict = ["--", "-D", "warnings"];
    // Act
    let selected = cargo_arguments(Some(&packages), false, false).expect("selected packages");
    let empty = cargo_arguments(Some(&[]), false, false).expect("empty selection");
    let workspace = cargo_arguments(None, false, false).expect("whole workspace");
    // Assert
    assert_eq!(
        selected,
        [
            "clippy",
            "-q",
            "--locked",
            "--all-targets",
            "--all-features",
            "-p",
            "agentty",
            "-p",
            "ag-git",
            "--",
            "-D",
            "warnings"
        ]
    );
    for arguments in [&empty, &workspace] {
        assert!(arguments.iter().any(|argument| argument == "--workspace"));
        assert!(!arguments.iter().any(|argument| argument == "-p"));
        assert!(arguments.ends_with(&strict.map(String::from)));
    }
}

#[test]
fn explicit_pty_targets_preserve_features_and_skip_only_known_unrelated_packages() {
    // Arrange
    let selections = [None, Some(Vec::new()), Some(vec!["agentty".to_string()])];
    let unrelated = ["ag-xtask".to_string()];

    // Act, Assert
    for selection in selections {
        assert_eq!(
            cargo_arguments(selection.as_deref(), true, false).expect("Agentty may be affected"),
            [
                "clippy",
                "-q",
                "--locked",
                "-p",
                "agentty",
                "--test",
                "e2e",
                "--test",
                "protocol_compliance_e2e",
                "--test",
                "showcase",
                "--",
                "-D",
                "warnings"
            ]
        );
    }
    assert_eq!(cargo_arguments(Some(&unrelated), true, false), None);
}

#[test]
fn compile_checks_keep_the_lint_selection_without_lint_flags() {
    // Arrange
    let packages = vec!["agentty".to_string()];
    let unrelated = ["ag-xtask".to_string()];

    // Act
    let selected = cargo_arguments(Some(&packages), false, true).expect("selected packages");
    let workspace = cargo_arguments(None, false, true).expect("whole workspace");
    let pty = cargo_arguments(Some(&packages), true, true).expect("Agentty is affected");

    // Assert
    assert_eq!(
        selected,
        [
            "check",
            "-q",
            "--locked",
            "--all-targets",
            "--all-features",
            "-p",
            "agentty"
        ]
    );
    assert_eq!(
        workspace,
        [
            "check",
            "-q",
            "--locked",
            "--all-targets",
            "--all-features",
            "--workspace"
        ]
    );
    assert_eq!(
        pty,
        [
            "check",
            "-q",
            "--locked",
            "-p",
            "agentty",
            "--test",
            "e2e",
            "--test",
            "protocol_compliance_e2e",
            "--test",
            "showcase"
        ]
    );
    assert_eq!(cargo_arguments(Some(&unrelated), true, true), None);
}
