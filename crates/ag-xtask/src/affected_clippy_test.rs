use super::clippy_arguments;

#[test]
fn selected_packages_are_linted_individually_and_no_selection_lints_the_workspace() {
    // Arrange
    let packages = vec!["agentty".to_string(), "ag-git".to_string()];
    let strict = ["--", "-D", "warnings"];
    // Act
    let selected = clippy_arguments(Some(&packages));
    let empty = clippy_arguments(Some(&[]));
    let workspace = clippy_arguments(None);
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
