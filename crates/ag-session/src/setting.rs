//! Domain model for persisted application setting keys.

use std::fmt;

/// Stable keys used in the `setting` and `project_setting` tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingName {
    /// Persists the active project selection.
    ActiveProjectId,
    /// Persists the active list tab selection.
    ActiveTab,
    /// Persists the provider that owns the fast-model default.
    DefaultFastAgent,
    /// Persists the project or global fast-model selection.
    DefaultFastModel,
    /// Persists the harness model last chosen through `/model`, which new
    /// Harness sessions start on.
    DefaultHarnessModel,
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
    /// Distinguishes registered projects from non-repository launch contexts.
    ProjectCatalogVisible,
    /// Records dismissal of the one-time project discovery suggestion.
    ProjectDiscoveryPromptSeen,
    /// Persists the random installation ID attached to telemetry events.
    TelemetryInstallationId,
    /// Persists the active terminal color theme.
    Theme,
}

impl SettingName {
    /// Returns the persisted key string for one setting.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ActiveProjectId => "ActiveProjectId",
            Self::ActiveTab => "ActiveTab",
            Self::DefaultFastAgent => "DefaultFastAgent",
            Self::DefaultFastModel => "DefaultFastModel",
            Self::DefaultHarnessModel => "DefaultHarnessModel",
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
            Self::ProjectCatalogVisible => "ProjectCatalogVisible",
            Self::ProjectDiscoveryPromptSeen => "ProjectDiscoveryPromptSeen",
            Self::TelemetryInstallationId => "TelemetryInstallationId",
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
#[path = "setting_test.rs"]
mod tests;
