use crate::setting::SettingName;

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
