//! Domain model for persisted application setting keys.

use std::fmt;

/// Stable keys used in the `setting` and `project_setting` tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingName {
    /// Persists the active project selection.
    ActiveProjectId,
    /// Persists the selected reasoning-effort level.
    ReasoningLevel,
    /// Persists the provider that owns the fast-model default.
    DefaultFastAgent,
    /// Persists the project or global fast-model selection.
    DefaultFastModel,
    /// Persists the provider that owns the review-model default.
    DefaultReviewAgent,
    /// Persists the project or global review-model selection.
    DefaultReviewModel,
    /// Persists the provider that owns the smart-model default.
    DefaultSmartAgent,
    /// Persists the project or global smart-model selection.
    DefaultSmartModel,
    /// Persists whether generated session commits append the Agentty coauthor
    /// trailer.
    IncludeCoauthoredByAgentty,
    /// Persists the configured open-command override.
    OpenCommand,
    /// Persists whether the last used model should become the default.
    LastUsedModelAsDefault,
    /// Persists the active terminal color theme.
    Theme,
}

impl SettingName {
    /// Returns the persisted key string for one setting.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ActiveProjectId => "ActiveProjectId",
            Self::ReasoningLevel => "ReasoningLevel",
            Self::DefaultFastAgent => "DefaultFastAgent",
            Self::DefaultFastModel => "DefaultFastModel",
            Self::DefaultReviewAgent => "DefaultReviewAgent",
            Self::DefaultReviewModel => "DefaultReviewModel",
            Self::DefaultSmartAgent => "DefaultSmartAgent",
            Self::DefaultSmartModel => "DefaultSmartModel",
            Self::IncludeCoauthoredByAgentty => "IncludeCoauthoredByAgentty",
            Self::OpenCommand => "OpenCommand",
            Self::LastUsedModelAsDefault => "LastUsedModelAsDefault",
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
            (SettingName::ReasoningLevel, "ReasoningLevel"),
            (SettingName::DefaultFastAgent, "DefaultFastAgent"),
            (SettingName::DefaultFastModel, "DefaultFastModel"),
            (SettingName::DefaultReviewAgent, "DefaultReviewAgent"),
            (SettingName::DefaultReviewModel, "DefaultReviewModel"),
            (SettingName::DefaultSmartAgent, "DefaultSmartAgent"),
            (SettingName::DefaultSmartModel, "DefaultSmartModel"),
            (
                SettingName::IncludeCoauthoredByAgentty,
                "IncludeCoauthoredByAgentty",
            ),
            (SettingName::OpenCommand, "OpenCommand"),
            (
                SettingName::LastUsedModelAsDefault,
                "LastUsedModelAsDefault",
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
            SettingName::ReasoningLevel,
            SettingName::DefaultFastAgent,
            SettingName::DefaultFastModel,
            SettingName::DefaultReviewAgent,
            SettingName::DefaultReviewModel,
            SettingName::DefaultSmartAgent,
            SettingName::DefaultSmartModel,
            SettingName::IncludeCoauthoredByAgentty,
            SettingName::OpenCommand,
            SettingName::LastUsedModelAsDefault,
            SettingName::Theme,
        ];

        // Act & Assert
        for setting_name in settings {
            assert_eq!(setting_name.to_string(), setting_name.as_str());
        }
    }
}
