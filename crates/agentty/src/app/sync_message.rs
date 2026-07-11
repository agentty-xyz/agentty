//! Application result formatting for synchronization summaries.

const SYNC_SUCCESS_HEADER: &str = "Successfully synchronized with its upstream.";

/// Builds synchronization completion copy with separated markdown sections.
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

fn sync_success_section(title: &str, details: &str) -> String {
    let mut lines = Vec::with_capacity(2);

    lines.push(title.to_string());

    if !details.is_empty() {
        lines.push(details.to_string());
    }

    lines.join("\n")
}
