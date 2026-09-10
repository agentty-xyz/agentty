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
}

impl ColorTheme {
    /// All selectable color themes in settings display order.
    pub const ALL: [Self; 3] = [Self::Current, Self::Green, Self::DarkHorizon];

    /// Returns the persisted wire value for this theme.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Green => "green",
            Self::DarkHorizon => "dark_horizon",
        }
    }

    /// Returns the human-readable theme name shown in the settings page.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "Agentty Default",
            Self::Green => "Agentty Green",
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
            "green" => Some(Self::Green),
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
#[path = "theme_test.rs"]
mod tests;
