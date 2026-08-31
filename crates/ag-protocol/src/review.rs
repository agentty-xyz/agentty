//! Structured focused-review response contract and display formatting.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Structured result returned by a focused-review utility prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "FocusedReview",
    description = "Structured focused-review result. Project impact describes the overall effect \
                   of the diff, while suggestions contains only actionable high- or \
                   medium-severity findings."
)]
pub struct FocusedReview {
    /// Concise statements describing the overall effect of the reviewed diff.
    #[schemars(
        title = "project_impact",
        description = "Concise statements about the diff's effects on behavior, reliability, \
                       maintainability, performance, security, or developer workflow. Use an \
                       empty array when there is no notable impact."
    )]
    pub project_impact: Vec<String>,
    /// Actionable high- or medium-severity findings from the reviewed diff.
    #[schemars(
        title = "suggestions",
        description = "Actionable findings scoped to the reviewed diff, ordered by severity with \
                       high severity first. Use an empty array when there are no suggestions."
    )]
    pub suggestions: Vec<FocusedReviewSuggestion>,
}

impl FocusedReview {
    /// Formats the structured review for the terminal session transcript.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let project_impact = markdown_bullets(&self.project_impact);
        let suggestions = if self.suggestions.is_empty() {
            "- None".to_string()
        } else {
            self.suggestions
                .iter()
                .map(|suggestion| {
                    format!("- [{}]: {}", suggestion.severity, suggestion.details.trim())
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "## Review\n\n### Project Impact\n\n{project_impact}\n\n### \
             Suggestions\n\n{suggestions}"
        )
    }
}

/// One actionable focused-review finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "FocusedReviewSuggestion",
    description = "One actionable high- or medium-severity finding scoped to the reviewed diff."
)]
pub struct FocusedReviewSuggestion {
    /// Concise explanation of the issue and its practical impact.
    #[schemars(
        title = "details",
        description = "Concise issue details, including relevant repository-root-relative file \
                       and line references when available, plus the practical impact."
    )]
    pub details: String,
    /// Severity assigned from the focused-review severity policy.
    #[schemars(
        title = "severity",
        description = "Finding severity: high for correctness, security, data-loss, or \
                       build-breaking risks; medium for concrete reliability, maintainability, \
                       performance, or workflow risks."
    )]
    pub severity: FocusedReviewSeverity,
}

/// Supported severity levels for actionable focused-review findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(
    title = "FocusedReviewSeverity",
    description = "Supported severity for an actionable focused-review finding."
)]
pub enum FocusedReviewSeverity {
    /// Correctness, security, data-loss, or build-breaking risk.
    High,
    /// Concrete reliability, maintainability, performance, or workflow risk.
    Medium,
}

impl fmt::Display for FocusedReviewSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::High => "High",
            Self::Medium => "Medium",
        })
    }
}

/// Formats one project-impact list, using the stable empty sentinel when no
/// impact was reported.
fn markdown_bullets(items: &[String]) -> String {
    if items.is_empty() {
        return "- None".to_string();
    }

    items
        .iter()
        .map(|item| format!("- {}", item.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_review_formats_structured_fields_as_markdown() {
        // Arrange
        let review = FocusedReview {
            project_impact: vec!["Improves review reliability.".to_string()],
            suggestions: vec![
                FocusedReviewSuggestion {
                    details: "Fix the stale cache check.".to_string(),
                    severity: FocusedReviewSeverity::High,
                },
                FocusedReviewSuggestion {
                    details: "Deduplicate the parsing path.".to_string(),
                    severity: FocusedReviewSeverity::Medium,
                },
            ],
        };

        // Act
        let markdown = review.to_markdown();

        // Assert
        assert_eq!(
            markdown,
            "## Review\n\n### Project Impact\n\n- Improves review reliability.\n\n### \
             Suggestions\n\n- [High]: Fix the stale cache check.\n- [Medium]: Deduplicate the \
             parsing path."
        );
    }

    #[test]
    fn focused_review_formats_empty_arrays_with_none_sentinels() {
        // Arrange
        let review = FocusedReview {
            project_impact: Vec::new(),
            suggestions: Vec::new(),
        };

        // Act
        let markdown = review.to_markdown();

        // Assert
        assert_eq!(
            markdown,
            "## Review\n\n### Project Impact\n\n- None\n\n### Suggestions\n\n- None"
        );
    }

    #[test]
    fn focused_review_severity_serializes_as_lowercase() {
        // Arrange
        let severity = FocusedReviewSeverity::High;

        // Act
        let serialized = serde_json::to_string(&severity).expect("severity should serialize");
        let deserialized = serde_json::from_str::<FocusedReviewSeverity>(&serialized)
            .expect("severity should deserialize");

        // Assert
        assert_eq!(serialized, "\"high\"");
        assert_eq!(deserialized, severity);
    }
}
