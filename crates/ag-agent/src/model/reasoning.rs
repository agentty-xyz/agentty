//! Maps generic execution preferences into provider transport values.

use ag_contracts::ReasoningLevel;
use ag_session::AgentModel;

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

/// Returns the Antigravity `--effort` value `model` accepts for this level,
/// or `None` when `model` rejects `--effort`.
///
/// The Antigravity CLI validates effort per model. Gemini 4 Hydrogen adds
/// `xhigh`, GPT-OSS 120B accepts only `medium`, and the orchestrator accepts
/// no effort. Other models accept `low`, `medium`, and `high`, so higher
/// generic levels map to each model's highest accepted value.
pub(crate) fn antigravity(model: &str, level: ReasoningLevel) -> Option<&'static str> {
    match model.parse::<AgentModel>() {
        Ok(AgentModel::GeminiProOrchestrator) => None,
        Ok(AgentModel::GptOss120b) => Some("medium"),
        Ok(AgentModel::Gemini4Hydrogen) => Some(match level {
            ReasoningLevel::Low => "low",
            ReasoningLevel::Medium => "medium",
            ReasoningLevel::High => "high",
            ReasoningLevel::XHigh | ReasoningLevel::Max => "xhigh",
        }),
        _ => Some(match level {
            ReasoningLevel::Low => "low",
            ReasoningLevel::Medium => "medium",
            ReasoningLevel::High | ReasoningLevel::XHigh | ReasoningLevel::Max => "high",
        }),
    }
}

/// Returns the Claude `--effort` value for this level.
///
/// Maps `XHigh` and `Max` to `"max"`, which is currently only supported on
/// `claude-opus-5-5`. The Claude CLI enforces this
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
