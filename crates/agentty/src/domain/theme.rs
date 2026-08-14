//! Domain model for user-selectable terminal color themes.

use std::fmt;

/// Terminal color themes available in settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum ColorTheme {
    /// The default Agentty terminal palette shown as `Agentty Default`.
    #[default]
    Current,
    /// A green-on-dark palette shown as `Agentty Green`.
    Green,
    /// A warm dark palette inspired by the Horizon editor theme.
    DarkHorizon,
    /// A muted navy palette shown as `Agentty Dark`.
    AgenttyDark,
}

impl ColorTheme {
    /// All selectable color themes in settings display order.
    pub const ALL: [Self; 4] = [
        Self::Current,
        Self::Green,
        Self::DarkHorizon,
        Self::AgenttyDark,
    ];

    /// Returns the persisted wire value for this theme.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Green => "green",
            Self::DarkHorizon => "dark_horizon",
            Self::AgenttyDark => "agentty_dark",
        }
    }

    /// Returns the human-readable theme name shown in the settings page.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "Agentty Default",
            Self::Green => "Agentty Green",
            Self::DarkHorizon => "Dark Horizon",
            Self::AgenttyDark => "Agentty Dark",
        }
    }

    /// Parses a persisted theme value.
    ///
    /// Returns `None` for unknown values so callers can fall back to
    /// [`ColorTheme::default`].
    #[must_use]
    pub fn parse_persisted(value: &str) -> Option<Self> {
        match value {
            "current" => Some(Self::Current),
            "green" => Some(Self::Green),
            "dark_horizon" => Some(Self::DarkHorizon),
            "agentty_dark" => Some(Self::AgenttyDark),
            _ => None,
        }
    }

    /// Returns the next theme in settings selector order.
    #[must_use]
    pub fn next(self) -> Self {
        let current_index = Self::ALL
            .iter()
            .position(|theme| *theme == self)
            .unwrap_or(0);
        let next_index = (current_index + 1) % Self::ALL.len();

        Self::ALL[next_index]
    }
}

impl fmt::Display for ColorTheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_persisted_returns_current_theme() {
        // Arrange
        let stored_value = "current";

        // Act
        let theme = ColorTheme::parse_persisted(stored_value);

        // Assert
        assert_eq!(theme, Some(ColorTheme::Current));
    }

    #[test]
    fn parse_persisted_returns_green_theme() {
        // Arrange
        let stored_value = "green";

        // Act
        let theme = ColorTheme::parse_persisted(stored_value);

        // Assert
        assert_eq!(theme, Some(ColorTheme::Green));
    }

    #[test]
    fn parse_persisted_rejects_unknown_theme() {
        // Arrange
        let stored_value = "unknown";

        // Act
        let theme = ColorTheme::parse_persisted(stored_value);

        // Assert
        assert_eq!(theme, None);
    }

    #[test]
    fn next_cycles_between_available_themes() {
        // Arrange
        let current_theme = ColorTheme::Current;
        let green_theme = ColorTheme::Green;
        let dark_horizon_theme = ColorTheme::DarkHorizon;
        let agentty_dark_theme = ColorTheme::AgenttyDark;

        // Act
        let next_theme = current_theme.next();
        let after_green_theme = green_theme.next();
        let after_dark_horizon_theme = dark_horizon_theme.next();
        let wrapped_theme = agentty_dark_theme.next();

        // Assert
        assert_eq!(next_theme, ColorTheme::Green);
        assert_eq!(after_green_theme, ColorTheme::DarkHorizon);
        assert_eq!(after_dark_horizon_theme, ColorTheme::AgenttyDark);
        assert_eq!(wrapped_theme, ColorTheme::Current);
    }

    #[test]
    fn parse_persisted_returns_dark_horizon_theme() {
        // Arrange
        let stored_value = "dark_horizon";

        // Act
        let theme = ColorTheme::parse_persisted(stored_value);

        // Assert
        assert_eq!(theme, Some(ColorTheme::DarkHorizon));
    }

    #[test]
    fn agentty_dark_has_persisted_value_and_label() {
        // Arrange
        let stored_value = "agentty_dark";

        // Act
        let theme = ColorTheme::parse_persisted(stored_value);
        let persisted_value = ColorTheme::AgenttyDark.as_str();
        let label = ColorTheme::AgenttyDark.label();

        // Assert
        assert_eq!(theme, Some(ColorTheme::AgenttyDark));
        assert_eq!(persisted_value, stored_value);
        assert_eq!(label, "Agentty Dark");
    }
}
