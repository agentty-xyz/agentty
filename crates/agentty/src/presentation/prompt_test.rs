use crate::domain::file_entry::FileEntry;
use crate::presentation::prompt::PromptAtMentionState;

/// Builds one file or directory entry for description-request tests.
fn entry(path: &str, is_dir: bool) -> FileEntry {
    FileEntry {
        is_dir,
        path: path.to_string(),
    }
}

#[test]
fn take_description_requests_returns_visible_supported_files_once() {
    // Arrange
    let mut state = PromptAtMentionState::new(vec![
        entry("src", true),
        entry("src/lib.rs", false),
        entry("src/notes.md", false),
        entry("src/tool.py", false),
        entry("src/zeta.rs", false),
    ]);

    // Act
    let first_requests = state.take_description_requests("", 4);
    let repeated_requests = state.take_description_requests("", 4);

    // Assert
    assert_eq!(
        first_requests,
        vec!["src/lib.rs".to_string(), "src/tool.py".to_string()]
    );
    assert_eq!(repeated_requests, [] as [String; 0]);
    assert_eq!(state.file_description("src/lib.rs"), None);
}

#[test]
fn take_description_requests_skips_already_requested_paths_in_a_new_window() {
    // Arrange
    let mut state = PromptAtMentionState::new(vec![
        entry("alpha.rs", false),
        entry("beta.rs", false),
        entry("gamma.rs", false),
    ]);
    state.take_description_requests("", 2);
    state.selected_index = 2;

    // Act
    let requests = state.take_description_requests("", 2);

    // Assert
    assert_eq!(requests, vec!["gamma.rs".to_string()]);
}

#[test]
fn apply_file_descriptions_exposes_loaded_descriptions() {
    // Arrange
    let mut state = PromptAtMentionState::new(vec![entry("main.rs", false)]);
    state.take_description_requests("", 10);

    // Act
    state.apply_file_descriptions([
        (
            "main.rs".to_string(),
            Some("Binary entry point.".to_string()),
        ),
        ("missing.py".to_string(), None),
    ]);

    // Assert
    assert_eq!(
        state.file_description("main.rs"),
        Some("Binary entry point.")
    );
    assert_eq!(state.file_description("missing.py"), None);
    assert_eq!(state.file_description("unknown.rs"), None);
}
