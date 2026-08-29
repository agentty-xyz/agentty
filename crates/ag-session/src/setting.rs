//! Domain model for persisted application setting keys.

use std::fmt;

/// Whether temporary read-only research waves start without plan approval.
pub const DEFAULT_AUTO_APPROVE_ORCHESTRATION_RESEARCH: bool = true;
/// Default number of orchestration children allowed to run concurrently.
pub const DEFAULT_ORCHESTRATION_PARALLELISM: u8 = 3;
/// Maximum orchestration concurrency exposed by the settings selector.
pub const MAX_ORCHESTRATION_PARALLELISM: u8 = 8;

/// Stable keys used in the `setting` and `project_setting` tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingName {
    /// Persists the active project selection.
    ActiveProjectId,
    /// Persists the active list tab selection.
    ActiveTab,
    /// Persists whether research-only orchestration waves start immediately.
    AutoApproveOrchestrationResearch,
    /// Persists the provider that owns the fast-model default.
    DefaultFastAgent,
    /// Persists the project or global fast-model selection.
    DefaultFastModel,
    /// Persists the reasoning level paired with the fast-model default.
    DefaultFastReasoningLevel,
    /// Persists the response speed paired with the fast-model default.
    DefaultFastSpeedMode,
    /// Persists the provider that owns the review-model default.
    DefaultReviewAgent,
    /// Persists the project or global review-model selection.
    DefaultReviewModel,
    /// Persists the reasoning level paired with the review-model default.
    DefaultReviewReasoningLevel,
    /// Persists the response speed paired with the review-model default.
    DefaultReviewSpeedMode,
    /// Persists the default response style for newly created sessions.
    DefaultResponseStyle,
    /// Persists the provider that owns the smart-model default.
    DefaultSmartAgent,
    /// Persists the project or global smart-model selection.
    DefaultSmartModel,
    /// Persists the reasoning level paired with the smart-model default.
    DefaultSmartReasoningLevel,
    /// Persists the response speed paired with the smart-model default.
    DefaultSmartSpeedMode,
    /// Persists whether generated session commits append the Agentty coauthor
    /// trailer.
    IncludeCoauthoredByAgentty,
    /// Persists the configured launch-configuration override.
    LaunchConfiguration,
    /// Persists whether the last used model should become the default.
    LastUsedModelAsDefault,
    /// Persists how many orchestration children may run at once.
    OrchestrationParallelism,
    /// Persists the active terminal color theme.
    Theme,
}

impl SettingName {
    /// Returns the persisted key string for one setting.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ActiveProjectId => "ActiveProjectId",
            Self::ActiveTab => "ActiveTab",
            Self::AutoApproveOrchestrationResearch => "AutoApproveOrchestrationResearch",
            Self::DefaultFastAgent => "DefaultFastAgent",
            Self::DefaultFastModel => "DefaultFastModel",
            Self::DefaultFastReasoningLevel => "DefaultFastReasoningLevel",
            Self::DefaultFastSpeedMode => "DefaultFastSpeedMode",
            Self::DefaultReviewAgent => "DefaultReviewAgent",
            Self::DefaultReviewModel => "DefaultReviewModel",
            Self::DefaultReviewReasoningLevel => "DefaultReviewReasoningLevel",
            Self::DefaultReviewSpeedMode => "DefaultReviewSpeedMode",
            Self::DefaultResponseStyle => "DefaultResponseStyle",
            Self::DefaultSmartAgent => "DefaultSmartAgent",
            Self::DefaultSmartModel => "DefaultSmartModel",
            Self::DefaultSmartReasoningLevel => "DefaultSmartReasoningLevel",
            Self::DefaultSmartSpeedMode => "DefaultSmartSpeedMode",
            Self::IncludeCoauthoredByAgentty => "IncludeCoauthoredByAgentty",
            Self::LaunchConfiguration => "LaunchConfiguration",
            Self::LastUsedModelAsDefault => "LastUsedModelAsDefault",
            Self::OrchestrationParallelism => "OrchestrationParallelism",
            Self::Theme => "Theme",
        }
    }
}

impl fmt::Display for SettingName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensures every setting key keeps its persisted wire value.
    #[test]
    fn test_as_str_returns_persisted_keys() {
        // Arrange
        let settings = [
            (SettingName::ActiveProjectId, "ActiveProjectId"),
            (SettingName::ActiveTab, "ActiveTab"),
            (
                SettingName::AutoApproveOrchestrationResearch,
                "AutoApproveOrchestrationResearch",
            ),
            (SettingName::DefaultFastAgent, "DefaultFastAgent"),
            (SettingName::DefaultFastModel, "DefaultFastModel"),
            (
                SettingName::DefaultFastReasoningLevel,
                "DefaultFastReasoningLevel",
            ),
            (SettingName::DefaultFastSpeedMode, "DefaultFastSpeedMode"),
            (SettingName::DefaultReviewAgent, "DefaultReviewAgent"),
            (SettingName::DefaultReviewModel, "DefaultReviewModel"),
            (
                SettingName::DefaultReviewReasoningLevel,
                "DefaultReviewReasoningLevel",
            ),
            (
                SettingName::DefaultReviewSpeedMode,
                "DefaultReviewSpeedMode",
            ),
            (SettingName::DefaultResponseStyle, "DefaultResponseStyle"),
            (SettingName::DefaultSmartAgent, "DefaultSmartAgent"),
            (SettingName::DefaultSmartModel, "DefaultSmartModel"),
            (
                SettingName::DefaultSmartReasoningLevel,
                "DefaultSmartReasoningLevel",
            ),
            (SettingName::DefaultSmartSpeedMode, "DefaultSmartSpeedMode"),
            (
                SettingName::IncludeCoauthoredByAgentty,
                "IncludeCoauthoredByAgentty",
            ),
            (SettingName::LaunchConfiguration, "LaunchConfiguration"),
            (
                SettingName::LastUsedModelAsDefault,
                "LastUsedModelAsDefault",
            ),
            (
                SettingName::OrchestrationParallelism,
                "OrchestrationParallelism",
            ),
            (SettingName::Theme, "Theme"),
        ];

        // Act & Assert
        for (setting_name, expected_key) in settings {
            assert_eq!(setting_name.as_str(), expected_key);
        }
    }

    /// Ensures the display output stays aligned with the persisted key.
    #[test]
    fn test_display_matches_as_str() {
        // Arrange
        let settings = [
            SettingName::ActiveProjectId,
            SettingName::ActiveTab,
            SettingName::AutoApproveOrchestrationResearch,
            SettingName::DefaultFastAgent,
            SettingName::DefaultFastModel,
            SettingName::DefaultFastReasoningLevel,
            SettingName::DefaultFastSpeedMode,
            SettingName::DefaultReviewAgent,
            SettingName::DefaultReviewModel,
            SettingName::DefaultReviewReasoningLevel,
            SettingName::DefaultReviewSpeedMode,
            SettingName::DefaultResponseStyle,
            SettingName::DefaultSmartAgent,
            SettingName::DefaultSmartModel,
            SettingName::DefaultSmartReasoningLevel,
            SettingName::DefaultSmartSpeedMode,
            SettingName::IncludeCoauthoredByAgentty,
            SettingName::LaunchConfiguration,
            SettingName::LastUsedModelAsDefault,
            SettingName::OrchestrationParallelism,
            SettingName::Theme,
        ];

        // Act & Assert
        for setting_name in settings {
            assert_eq!(setting_name.to_string(), setting_name.as_str());
        }
    }
}
