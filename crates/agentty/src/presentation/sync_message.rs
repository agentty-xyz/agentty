//! User-visible synchronization result formatting.

const SYNC_SUCCESS_HEADER: &str = "Successfully synchronized with its upstream.";

/// Builds a sync completion message using markdown headers and spacing between
/// pull, push, and conflict blocks.
pub(crate) fn format_sync_success_message(
    pulled_summary: &str,
    pulled_titles: &str,
    pushed_summary: &str,
    pushed_titles: &str,
    conflict_summary: &str,
) -> String {
    let pull_section = sync_success_section(&format!("## 1. {pulled_summary}"), pulled_titles);
    let push_section = sync_success_section(&format!("## 2. {pushed_summary}"), pushed_titles);
    let conflict_section = sync_success_section(&format!("## 3. {conflict_summary}"), "");

    [
        SYNC_SUCCESS_HEADER,
        &pull_section,
        &push_section,
        &conflict_section,
    ]
    .join("\n\n")
}

/// Builds one markdown sync section with a title and optional details.
fn sync_success_section(title: &str, details: &str) -> String {
    let mut lines = Vec::with_capacity(2);

    lines.push(title.to_string());

    if !details.is_empty() {
        lines.push(details.to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_sync_success_message_includes_markdown_sections() {
        // Arrange
        let pulled_summary = "2 commits pulled";
        let pulled_titles = "  - Add audit log indexing\n  - Fix merge conflict prompt wording";
        let pushed_summary = "1 commit pushed";
        let pushed_titles = "  - Polish sync popup alignment";
        let conflict_summary = "conflicts fixed: src/lib.rs";

        // Act
        let formatted_message = format_sync_success_message(
            pulled_summary,
            pulled_titles,
            pushed_summary,
            pushed_titles,
            conflict_summary,
        );

        // Assert
        assert!(formatted_message.starts_with(
            "Successfully synchronized with its upstream.\n\n## 1. 2 commits pulled\n  - Add \
             audit log indexing\n",
        ));
        assert!(
            formatted_message
                .contains("\n\n## 2. 1 commit pushed\n  - Polish sync popup alignment",)
        );
        assert!(formatted_message.contains("\n\n## 3. conflicts fixed: src/lib.rs"));
    }
}
