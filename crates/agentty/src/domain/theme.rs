//! Domain model for user-selectable terminal color themes.

use std::fmt;

/// Terminal color themes available in settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum ColorTheme {
    /// The default Agentty terminal palette shown as `Agentty Default`.
    #[default]
    Current,
    /// A green-on-dark palette shown as `Agentty Green`.
    Hacker,
    /// A warm dark palette inspired by the Horizon editor theme.
    DarkHorizon,
}

impl ColorTheme {
    /// All selectable color themes in settings display order.
    pub const ALL: [Self; 3] = [Self::Current, Self::Hacker, Self::DarkHorizon];

    /// Returns the persisted wire value for this theme.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Hacker => "hacker",
            Self::DarkHorizon => "dark_horizon",
        }
    }

    /// Returns the human-readable theme name shown in the settings page.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "Agentty Default",
            Self::Hacker => "Agentty Green",
            Self::DarkHorizon => "Dark Horizon",
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
            "hacker" => Some(Self::Hacker),
            "dark_horizon" => Some(Self::DarkHorizon),
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
    fn parse_persisted_returns_hacker_theme() {
        // Arrange
        let stored_value = "hacker";

        // Act
        let theme = ColorTheme::parse_persisted(stored_value);

        // Assert
        assert_eq!(theme, Some(ColorTheme::Hacker));
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
        let hacker_theme = ColorTheme::Hacker;
        let dark_horizon_theme = ColorTheme::DarkHorizon;

        // Act
        let next_theme = current_theme.next();
        let after_hacker_theme = hacker_theme.next();
        let wrapped_theme = dark_horizon_theme.next();

        // Assert
        assert_eq!(next_theme, ColorTheme::Hacker);
        assert_eq!(after_hacker_theme, ColorTheme::DarkHorizon);
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
}
