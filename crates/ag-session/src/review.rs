//! Pure focused-review parsing helpers shared across session frontends.

use std::fmt;
use std::str::FromStr;

use ag_protocol::TurnPrompt;

/// Durable state of one focused-review generation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusedReviewStatus {
    /// Review generation is still running.
    Pending,
    /// Review generation completed and persisted its markdown.
    Ready,
    /// Review generation completed without usable markdown.
    Failed,
}

impl fmt::Display for FocusedReviewStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "Pending",
            Self::Ready => "Ready",
            Self::Failed => "Failed",
        })
    }
}

impl FromStr for FocusedReviewStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Pending" => Ok(Self::Pending),
            "Ready" => Ok(Self::Ready),
            "Failed" => Ok(Self::Failed),
            _ => Err(format!("Unknown focused review status: {value}")),
        }
    }
}

/// Builds the agent-facing `/apply` prompt from focused-review suggestions.
///
/// The prompt explicitly asks the agent to verify each suggestion against the
/// current code before making changes, then apply only suggestions that remain
/// correct and relevant.
pub fn build_apply_review_prompt(suggestions: &str) -> TurnPrompt {
    let suggestions = suggestions.trim();
    let fence = ag_agent::diff_fence(suggestions);
    let fenced_suggestions = format!("{fence}text\n{suggestions}\n{fence}");
    let prompt = APPLY_REVIEW_PROMPT_TEMPLATE
        .trim_end()
        .replace("{{ fenced_suggestions }}", &fenced_suggestions);

    TurnPrompt::from_text(prompt)
}

/// Extracts actionable suggestion content from focused-review markdown.
///
/// Returns `None` when the `### Suggestions` section is missing, empty, or
/// reports `- None` with optional trailing punctuation.
#[must_use]
pub fn review_suggestions(review_text: &str) -> Option<String> {
    let suggestions_header = "### Suggestions";
    let header_start = review_text.find(suggestions_header)?;
    let content_start = header_start + suggestions_header.len();
    let content = &review_text[content_start..];
    let section_end = content.find("\n### ").unwrap_or(content.len());
    let suggestions = content[..section_end].trim();

    if suggestions.is_empty() || is_no_suggestions_sentinel(suggestions) {
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

const APPLY_REVIEW_PROMPT_TEMPLATE: &str = include_str!("template/apply_review_prompt.md");

/// Returns whether a suggestions section contains only the required `None`
/// sentinel plus optional trailing punctuation.
fn is_no_suggestions_sentinel(suggestions: &str) -> bool {
    suggestions.strip_prefix("- None").is_some_and(|suffix| {
        suffix
            .chars()
            .all(|character| character.is_ascii_punctuation())
    })
}

#[cfg(test)]
#[path = "review_test.rs"]
mod tests;
