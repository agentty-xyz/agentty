//! Pure focused-review parsing helpers shared by UI and runtime behavior.

/// Extracts actionable suggestion content from focused-review markdown.
///
/// Returns `None` when the `### Suggestions` section is missing, empty, or
/// explicitly reports `- None`.
#[must_use]
pub fn review_suggestions(review_text: &str) -> Option<String> {
    let suggestions_header = "### Suggestions";
    let header_start = review_text.find(suggestions_header)?;
    let content_start = header_start + suggestions_header.len();
    let content = &review_text[content_start..];
    let section_end = content.find("\n### ").unwrap_or(content.len());
    let suggestions = content[..section_end].trim();

    if suggestions.is_empty() || suggestions == "- None" {
        return None;
    }

    Some(suggestions.to_string())
}

/// Returns whether focused-review markdown contains suggestions that `/apply`
/// can act on.
#[must_use]
pub fn has_actionable_review_suggestions(review_text: Option<&str>) -> bool {
    review_text.and_then(review_suggestions).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_review_suggestions_returns_suggestions_content() {
        // Arrange
        let review_text = "\
### Summary

- Good shape.

### Suggestions

- Fix the typo in `README.md:10`.";

        // Act
        let suggestions = review_suggestions(review_text);

        // Assert
        assert_eq!(
            suggestions,
            Some("- Fix the typo in `README.md:10`.".to_string())
        );
    }

    #[test]
    fn test_review_suggestions_returns_none_for_no_suggestions() {
        // Arrange
        let review_text = "\
### Summary

- Good shape.

### Suggestions

- None";

        // Act
        let suggestions = review_suggestions(review_text);

        // Assert
        assert_eq!(suggestions, None);
    }

    #[test]
    fn test_review_suggestions_returns_none_when_section_missing() {
        // Arrange
        let review_text = "\
### Summary

- Good shape overall.";

        // Act
        let suggestions = review_suggestions(review_text);

        // Assert
        assert_eq!(suggestions, None);
    }

    #[test]
    fn test_review_suggestions_stops_at_next_heading() {
        // Arrange
        let review_text = "\
### Summary

- Good shape.

### Suggestions

- Fix the typo in `README.md:10`.

### Project Impact

- Great work overall.";

        // Act
        let suggestions = review_suggestions(review_text);

        // Assert
        assert_eq!(
            suggestions,
            Some("- Fix the typo in `README.md:10`.".to_string())
        );
    }

    #[test]
    fn test_review_suggestions_returns_none_for_empty_section() {
        // Arrange
        let review_text = "\
### Suggestions

### Project Impact

- None";

        // Act
        let suggestions = review_suggestions(review_text);

        // Assert
        assert_eq!(suggestions, None);
    }

    #[test]
    fn test_has_actionable_review_suggestions_detects_suggestions_section() {
        // Arrange
        let review_with_suggestions = "## Review\n### Suggestions\n- Fix typo\n### Notes";
        let review_without_suggestions = "## Review\n### Suggestions\n- None\n### Notes";

        // Act
        let with_suggestions = has_actionable_review_suggestions(Some(review_with_suggestions));
        let without_suggestions =
            has_actionable_review_suggestions(Some(review_without_suggestions));
        let missing_header = has_actionable_review_suggestions(Some("## Review"));

        // Assert
        assert!(with_suggestions);
        assert!(!without_suggestions);
        assert!(!missing_header);
    }
}
