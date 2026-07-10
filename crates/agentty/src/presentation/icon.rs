use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Stable loader glyph painted by the Tachyonfx session-output effect.
pub(crate) const TACHYON_LOADER_GLYPH: &str = "▌▌▌";
/// Display width of [`TACHYON_LOADER_GLYPH`] in terminal cells.
pub(crate) const TACHYON_LOADER_WIDTH: u16 = 3;

/// A collection of icons used throughout the terminal UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    /// A downward arrow symbol (↓).
    ArrowDown,
    /// An upward arrow symbol (↑).
    ArrowUp,
    /// A check mark symbol (✓).
    Check,
    /// A cross mark symbol (✗).
    Cross,
    /// A git branch symbol (●).
    GitBranch,
    /// A pending status symbol (·).
    Pending,
    /// A compact loader symbol animated by Tachyonfx after render.
    TachyonLoader,
    /// A stable loader glyph animated by shared Tachyonfx effects after render.
    Spinner,
    /// A warning symbol (!).
    Warn,
}

impl Icon {
    /// Returns the stable `Spinner` loader icon used before Tachyonfx painting.
    pub fn current_spinner() -> Self {
        Icon::Spinner
    }

    /// Returns the current spinner frame index derived from wall-clock time
    /// for Tachyon loader animation timing.
    pub fn current_spinner_frame() -> usize {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        Self::spinner_frame_from_millis(now)
    }

    /// Returns the spinner frame index for a millisecond timestamp.
    pub fn spinner_frame_from_millis(timestamp_millis: u128) -> usize {
        (timestamp_millis / 100) as usize
    }

    /// Returns the string representation of the icon.
    pub fn as_str(self) -> &'static str {
        match self {
            Icon::ArrowDown => "↓",
            Icon::ArrowUp => "↑",
            Icon::Check => "✓",
            Icon::Cross => "✗",
            Icon::GitBranch => "●",
            Icon::Pending => "·",
            Icon::TachyonLoader | Icon::Spinner => TACHYON_LOADER_GLYPH,
            Icon::Warn => "!",
        }
    }
}

impl fmt::Display for Icon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        // Arrange & Act & Assert
        assert_eq!(Icon::ArrowDown.as_str(), "↓");
        assert_eq!(Icon::ArrowUp.as_str(), "↑");
        assert_eq!(Icon::Check.as_str(), "✓");
        assert_eq!(Icon::Cross.as_str(), "✗");
        assert_eq!(Icon::GitBranch.as_str(), "●");
        assert_eq!(Icon::Pending.as_str(), "·");
        assert_eq!(Icon::TachyonLoader.as_str(), TACHYON_LOADER_GLYPH);
        assert_eq!(Icon::Warn.as_str(), "!");
    }

    #[test]
    fn test_current_spinner() {
        // Arrange & Act
        let icon = Icon::current_spinner();

        // Assert
        assert!(matches!(icon, Icon::Spinner));
        assert_eq!(icon.as_str(), TACHYON_LOADER_GLYPH);
    }

    #[test]
    fn test_spinner_frame_from_millis() {
        // Arrange, Act, Assert
        assert_eq!(Icon::spinner_frame_from_millis(0), 0);
        assert_eq!(Icon::spinner_frame_from_millis(99), 0);
        assert_eq!(Icon::spinner_frame_from_millis(100), 1);
    }

    #[test]
    fn test_spinner_uses_tachyon_loader_glyph() {
        // Arrange & Act & Assert
        assert_eq!(Icon::Spinner.as_str(), TACHYON_LOADER_GLYPH);
    }

    #[test]
    fn test_display_matches_as_str() {
        // Arrange
        let icons = [
            Icon::ArrowDown,
            Icon::ArrowUp,
            Icon::Check,
            Icon::Cross,
            Icon::GitBranch,
            Icon::Pending,
            Icon::TachyonLoader,
            Icon::Spinner,
            Icon::Warn,
        ];

        // Act & Assert
        for icon in icons {
            assert_eq!(format!("{icon}"), icon.as_str());
        }
    }
}
