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
                .map(FocusedReviewSuggestion::to_markdown)
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
    /// Source evidence and rationale, when a finding can be anchored.
    #[schemars(
        description = "Source evidence for this finding. Quote exact code from the reviewed diff, \
                       identify the old or new side, and explain its trigger, impact, and \
                       correction. Use null only when no reliable source anchor is available."
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<FocusedReviewEvidence>,
    /// Severity assigned from the focused-review severity policy.
    #[schemars(
        title = "severity",
        description = "Finding severity: high for correctness, security, data-loss, or \
                       build-breaking risks; medium for concrete reliability, maintainability, \
                       performance, or workflow risks."
    )]
    pub severity: FocusedReviewSeverity,
}

impl FocusedReviewSuggestion {
    /// Applies a host-verified range and reconciles duplicate primary prose
    /// citations. Supporting references to other lines retain their locations.
    /// Pass zeroes when the evidence cannot be anchored.
    pub fn resolve_evidence_range(&mut self, start_line: u32, end_line: u32) {
        let Some(evidence) = &mut self.evidence else {
            return;
        };
        let claimed_line = evidence.start_line;
        evidence.start_line = start_line;
        evidence.end_line = end_line;
        self.details = self.details_with_location(claimed_line);
    }

    /// Labels missing source anchors and distinguishes unverified supporting
    /// references from the primary evidence location.
    fn to_markdown(&self) -> String {
        let Some(evidence) = &self.evidence else {
            return format!(
                "- [{}] (unanchored): {}",
                self.severity,
                self.details.trim()
            );
        };
        let location = if evidence.start_line == 0 {
            format!("`{}` (unanchored)", evidence.path)
        } else {
            format!("`{}:{}`", evidence.path, evidence.start_line)
        };
        let side = match evidence.side {
            FocusedReviewSide::New => "",
            FocusedReviewSide::Old => " (before change)",
        };

        format!(
            "- [{}]: {location}{side}: {} Trigger: {} Impact: {} Correction: {} Supporting \
             references are unverified.",
            self.severity,
            self.details.trim(),
            evidence.trigger.trim(),
            evidence.impact.trim(),
            evidence.correction.trim(),
        )
    }

    /// Reconciles citations matching the original primary path and line.
    fn details_with_location(&self, claimed_line: u32) -> String {
        let details = self.details.trim();
        let Some(evidence) = &self.evidence else {
            return details.to_string();
        };
        if evidence.path.is_empty() || claimed_line == 0 {
            return details.to_string();
        }
        let prefix = format!("{}:", evidence.path);
        let mut rendered = String::new();
        let mut copied = 0;
        for (start, _) in details.match_indices(&prefix) {
            if start < copied
                || details[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| {
                        character.is_alphanumeric()
                            || matches!(character, '/' | '\\' | '_' | '-' | '.')
                    })
            {
                continue;
            }
            let suffix = &details[start + prefix.len()..];
            let mut length = suffix.bytes().take_while(u8::is_ascii_digit).count();
            if suffix[..length].parse::<u32>().ok() != Some(claimed_line) {
                continue;
            }
            // Discard unverified columns and ranges along with stale line
            // numbers.
            while suffix
                .as_bytes()
                .get(length)
                .is_some_and(|separator| matches!(separator, b':' | b'-'))
            {
                let digits = suffix[length + 1..]
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .count();
                if digits == 0 {
                    break;
                }
                length += 1 + digits;
            }
            rendered.push_str(&details[copied..start]);
            rendered.push_str(&evidence.path);
            if evidence.start_line > 0 {
                rendered.push(':');
                rendered.push_str(&evidence.start_line.to_string());
            }
            copied = start + prefix.len() + length;
        }
        rendered.push_str(&details[copied..]);

        rendered
    }
}

/// Source citation and actionable rationale for one focused-review finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FocusedReviewEvidence {
    /// Concrete correction that addresses the reported risk.
    pub correction: String,
    /// Inclusive source line, resolved by the host; zero means unanchored.
    pub end_line: u32,
    /// Exact source snippet without unified-diff markers.
    pub existing_code: String,
    /// Practical consequence when the trigger occurs.
    pub impact: String,
    /// Repository-root-relative POSIX path on the selected side of the diff.
    pub path: String,
    /// Version of the source used by the citation.
    pub side: FocusedReviewSide,
    /// First source line, resolved by the host; zero means unanchored.
    pub start_line: u32,
    /// Concrete input, state, or sequence that exposes the risk.
    pub trigger: String,
}

/// Version of a changed file cited by a focused-review finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FocusedReviewSide {
    /// Source before the change, including deleted code.
    Old,
    /// Source after the change, including added code.
    New,
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
#[path = "review_test.rs"]
mod tests;
