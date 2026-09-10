//! Shared review-request helpers used by app workflows.

/// Parsed commit-message metadata used to populate a new review request.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestCommitMessage {
    /// Optional body copied from the commit description.
    pub(crate) body: Option<String>,
    /// Title copied from the first non-empty commit-message line.
    pub(crate) title: String,
}

/// Parses a session-branch commit message into review-request metadata.
pub(crate) fn parse_review_request_commit_message(
    commit_message: &str,
) -> Option<ReviewRequestCommitMessage> {
    let mut lines = commit_message.lines();
    let title = lines
        .find(|line| !line.trim().is_empty())?
        .trim()
        .to_string();
    let description = lines.collect::<Vec<_>>().join("\n");
    let description = description.trim();

    Some(ReviewRequestCommitMessage {
        body: (!description.is_empty()).then(|| description.to_string()),
        title,
    })
}

#[cfg(test)]
#[path = "review_request_test.rs"]
mod tests;
