//! Session execution settings and usage statistics shared by agent transports.

use std::fmt;
use std::str::FromStr;

/// Preferred amount of detail in user-facing session responses.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ResponseStyle {
    /// Keep responses compact while retaining essential results and caveats.
    Concise,
    /// Balance brevity with enough context to understand and verify the result.
    #[default]
    Balanced,
    /// Explain decisions, trade-offs, effects, and verification thoroughly.
    Detailed,
}

impl ResponseStyle {
    /// Stable selector ordering.
    pub const ALL: [Self; 3] = [Self::Concise, Self::Balanced, Self::Detailed];

    /// Stable persisted identifier for this response style.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Concise => "concise",
            Self::Balanced => "balanced",
            Self::Detailed => "detailed",
        }
    }

    /// User-visible selector label for this response style.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Concise => "Concise",
            Self::Balanced => "Balanced",
            Self::Detailed => "Detailed",
        }
    }

    /// User-visible explanation of this response style.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Concise => "Compact answers with essential results, caveats, and verification.",
            Self::Balanced => "Enough context to understand and verify without exhaustive detail.",
            Self::Detailed => "Thorough decisions, trade-offs, effects, and verification.",
        }
    }

    /// Agent-facing guidance for this response style.
    pub(crate) const fn prompt_instruction(self) -> &'static str {
        match self {
            Self::Concise => {
                "Lead with the outcome. Keep the final answer compact, omit routine narration, and \
                 retain essential caveats and verification."
            }
            Self::Balanced => {
                "Lead with the outcome. Provide enough context to understand decisions and verify \
                 the result without exhaustive detail."
            }
            Self::Detailed => {
                "Lead with the outcome. Explain decisions, trade-offs, affected behavior, and \
                 verification thoroughly without repetition."
            }
        }
    }
}

impl fmt::Display for ResponseStyle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ResponseStyle {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "concise" => Ok(Self::Concise),
            "balanced" => Ok(Self::Balanced),
            "detailed" => Ok(Self::Detailed),
            _ => Err(format!("Unknown response style: {value}")),
        }
    }
}

/// Response-speed preference applied to one agent session.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SpeedMode {
    /// Use the provider's standard routing and pricing.
    #[default]
    Normal,
    /// Request the provider's higher-cost low-latency routing.
    Fast,
}

impl SpeedMode {
    /// Stable selector ordering.
    pub const ALL: [Self; 2] = [Self::Normal, Self::Fast];

    /// Stable persisted identifier for this speed mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Fast => "fast",
        }
    }

    /// User-visible selector label for this speed mode.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Fast => "Fast",
        }
    }

    /// User-visible explanation of this speed mode.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Normal => "Standard response speed and provider pricing.",
            Self::Fast => "Faster responses at a higher provider cost.",
        }
    }

    /// Codex app-server service-tier value for this speed mode.
    pub const fn codex_service_tier(self) -> &'static str {
        match self {
            Self::Normal => "default",
            Self::Fast => "fast",
        }
    }

    /// Whether Claude Code should enable its `fastMode` setting.
    pub const fn claude_fast_mode(self) -> bool {
        matches!(self, Self::Fast)
    }
}

impl fmt::Display for SpeedMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SpeedMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "normal" => Ok(Self::Normal),
            "fast" => Ok(Self::Fast),
            _ => Err(format!("Unknown speed mode: {value}")),
        }
    }
}

/// Known availability of a session worktree diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SessionDiffState {
    /// Diff availability could not be determined, so callers should preserve
    /// access to diagnostic diff output.
    #[default]
    Unknown,
    /// The latest successful diff refresh returned no content.
    Empty,
    /// The latest successful diff refresh returned content.
    Present,
}

/// Token and diff usage statistics associated with one agent session or
/// isolated prompt.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SessionStats {
    /// Added diff lines currently attributed to the session worktree.
    pub added_lines: u64,
    /// Deleted diff lines currently attributed to the session worktree.
    pub deleted_lines: u64,
    /// Availability derived from the latest worktree diff refresh.
    pub diff_state: SessionDiffState,
    /// Input/prompt tokens consumed by this session.
    pub input_tokens: u64,
    /// Output/response tokens produced by this session.
    pub output_tokens: u64,
}

impl SessionStats {
    /// Returns whether the UI should advertise access to the session diff.
    ///
    /// Unknown state retains the shortcut so a subsequent diff attempt can
    /// surface the underlying Git diagnostic instead of hiding it.
    pub fn should_show_diff(&self) -> bool {
        self.diff_state != SessionDiffState::Empty
    }

    /// Counts added and deleted lines in one git patch while ignoring file
    /// header markers such as `+++` and `---`.
    pub fn line_change_counts(diff: &str) -> (u64, u64) {
        diff.lines()
            .fold((0_u64, 0_u64), |(added_lines, deleted_lines), line| {
                if line.starts_with('+') && !line.starts_with("+++") {
                    return (added_lines.saturating_add(1), deleted_lines);
                }

                if line.starts_with('-') && !line.starts_with("---") {
                    return (added_lines, deleted_lines.saturating_add(1));
                }

                (added_lines, deleted_lines)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_style_round_trips_persisted_values() {
        // Arrange, Act, Assert
        for response_style in ResponseStyle::ALL {
            assert_eq!(
                response_style.as_str().parse::<ResponseStyle>(),
                Ok(response_style)
            );
            assert_eq!(response_style.to_string(), response_style.as_str());
        }
    }

    #[test]
    fn response_style_rejects_unknown_persisted_value() {
        // Arrange, Act
        let result = "verbose".parse::<ResponseStyle>();

        // Assert
        assert_eq!(result, Err("Unknown response style: verbose".to_string()));
    }

    #[test]
    fn response_style_exposes_stable_labels_and_descriptions() {
        // Arrange, Act
        let values = ResponseStyle::ALL
            .map(|response_style| (response_style.name(), response_style.description()));

        // Assert
        assert_eq!(
            values,
            [
                (
                    "Concise",
                    "Compact answers with essential results, caveats, and verification."
                ),
                (
                    "Balanced",
                    "Enough context to understand and verify without exhaustive detail."
                ),
                (
                    "Detailed",
                    "Thorough decisions, trade-offs, effects, and verification."
                ),
            ]
        );
    }

    #[test]
    fn speed_mode_round_trips_persisted_values() {
        // Arrange, Act, Assert
        for speed_mode in SpeedMode::ALL {
            assert_eq!(speed_mode.as_str().parse::<SpeedMode>(), Ok(speed_mode));
            assert_eq!(speed_mode.to_string(), speed_mode.as_str());
        }
    }

    #[test]
    fn speed_mode_maps_provider_settings() {
        // Arrange, Act, Assert
        assert_eq!(SpeedMode::Normal.codex_service_tier(), "default");
        assert!(!SpeedMode::Normal.claude_fast_mode());
        assert_eq!(SpeedMode::Fast.codex_service_tier(), "fast");
        assert!(SpeedMode::Fast.claude_fast_mode());
    }

    #[test]
    fn speed_mode_rejects_unknown_persisted_value() {
        // Arrange, Act
        let result = "turbo".parse::<SpeedMode>();

        // Assert
        assert_eq!(result, Err("Unknown speed mode: turbo".to_string()));
    }

    #[test]
    fn should_show_diff_hides_only_known_empty_diffs() {
        // Arrange
        let unknown = SessionStats::default();
        let empty = SessionStats {
            diff_state: SessionDiffState::Empty,
            ..SessionStats::default()
        };
        let present = SessionStats {
            diff_state: SessionDiffState::Present,
            ..SessionStats::default()
        };

        // Act, Assert
        assert!(unknown.should_show_diff());
        assert!(!empty.should_show_diff());
        assert!(present.should_show_diff());
    }
}
