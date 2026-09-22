//! Domain model for the terminal mouse-capture preference.

/// Whether the terminal reports mouse wheel and drag events to Agentty.
///
/// Enabled capture gives Agentty wheel scrolling and scrollbar dragging;
/// disabled capture leaves clicks with the terminal so native click-drag text
/// selection works without modifier keys.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseSupport {
    /// Terminal mouse capture is on (the default).
    #[default]
    Enabled,
    /// Terminal mouse capture is off.
    Disabled,
}

impl MouseSupport {
    /// Builds the preference from a settings-page switch value.
    #[must_use]
    pub fn from_enabled(is_enabled: bool) -> Self {
        if is_enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }

    /// Returns whether terminal mouse capture should be on.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// Returns the persisted wire value for this preference.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "true",
            Self::Disabled => "false",
        }
    }

    /// Parses a persisted preference value.
    ///
    /// Returns `None` for unknown values so callers can fall back to
    /// [`MouseSupport::default`].
    #[must_use]
    pub fn parse_persisted(value: &str) -> Option<Self> {
        value.parse::<bool>().ok().map(Self::from_enabled)
    }
}

#[cfg(test)]
#[path = "mouse_test.rs"]
mod tests;
