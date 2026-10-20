use super::clippy_arguments;

#[test]
fn selected_packages_are_linted_individually_and_no_selection_lints_the_workspace() {
    // Arrange
    let packages = vec!["agentty".to_string(), "ag-git".to_string()];
    let strict = ["--", "-D", "warnings"];
    // Act
    let selected = clippy_arguments(Some(&packages), false).expect("selected packages");
    let empty = clippy_arguments(Some(&[]), false).expect("empty selection");
    let workspace = clippy_arguments(None, false).expect("whole workspace");
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
            clippy_arguments(selection.as_deref(), true).expect("Agentty may be affected"),
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
    assert_eq!(clippy_arguments(Some(&unrelated), true), None);
}
