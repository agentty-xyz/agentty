//! Maps generic execution preferences into provider transport values.

use ag_contracts::ReasoningLevel;

/// Returns the Codex reasoning-effort identifier for this level.
pub(crate) fn codex(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh => "xhigh",
        ReasoningLevel::Max => "max",
    }
}

/// Returns the Antigravity `--effort` value for this level.
///
/// Antigravity accepts `low`, `medium`, and `high`, so higher generic
/// reasoning levels map to its highest supported value.
pub(crate) fn antigravity(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High | ReasoningLevel::XHigh | ReasoningLevel::Max => "high",
    }
}

/// Returns the Claude `--effort` value for this level.
///
/// Maps `XHigh` and `Max` to `"max"`, which is currently only supported on
/// `claude-opus-5`. The Claude CLI enforces this
/// restriction and will surface an error for other models.
pub(crate) fn claude(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Max => "max",
    }
}

#[cfg(test)]
#[path = "reasoning_test.rs"]
mod tests;
