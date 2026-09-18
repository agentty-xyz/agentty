use super::preserve_description;

#[test]
fn additions_preserve_remote_text_and_whitespace() {
    // Arrange
    let current = "## Notes\n  Keep #123 and https://example.com.  \n";
    let candidate = format!("{current}\nNew detail");

    // Act
    let updated = preserve_description(current, &candidate);

    // Assert
    assert_eq!(updated, format!("{current}\n\nNew detail"));
}

#[test]
fn remote_markers_and_content_are_never_removed() {
    // Arrange
    let current = "Notes\n\n<!-- agentty-generated:v1:b04b403424d6d509 -->\nKeep this \
                   user-authored deployment note.\n<!-- /agentty-generated -->";

    // Act
    let unchanged = preserve_description(current, current);
    let updated = preserve_description(current, "Notes\nNew detail");

    // Assert
    assert_eq!(unchanged, current);
    assert_eq!(updated, format!("{current}\n\nNew detail"));
}

#[test]
fn duplicate_candidate_lines_preserve_their_multiplicity() {
    // Arrange
    let current = "Notes";

    // Act
    let updated = preserve_description(current, "Notes\nNotes");

    // Assert
    assert_eq!(updated, "Notes\n\nNotes");
}
