use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    ArrowDown,
    ArrowUp,
    Check,
    Cross,
    GitBranch,
    Pending,
    Spinner(usize),
    Warn,
}

impl Icon {
    pub fn current_spinner() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Icon::Spinner((now / 100) as usize)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Icon::ArrowDown => "↓",
            Icon::ArrowUp => "↑",
            Icon::Check => "✓",
            Icon::Cross => "✗",
            Icon::GitBranch => "●",
            Icon::Pending => "·",
            Icon::Spinner(frame) => SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
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
        assert_eq!(Icon::Warn.as_str(), "!");
    }

    #[test]
    fn test_current_spinner() {
        // Arrange & Act
        let icon = Icon::current_spinner();

        // Assert
        assert!(matches!(icon, Icon::Spinner(_)));
    }

    #[test]
    fn test_spinner_frames() {
        // Arrange & Act & Assert
        assert_eq!(Icon::Spinner(0).as_str(), "⠋");
        assert_eq!(Icon::Spinner(1).as_str(), "⠙");
        assert_eq!(Icon::Spinner(9).as_str(), "⠏");
    }

    #[test]
    fn test_spinner_wraps() {
        // Arrange & Act & Assert
        assert_eq!(Icon::Spinner(10).as_str(), Icon::Spinner(0).as_str());
        assert_eq!(Icon::Spinner(15).as_str(), Icon::Spinner(5).as_str());
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
            Icon::Spinner(3),
            Icon::Warn,
        ];

        // Act & Assert
        for icon in icons {
            assert_eq!(format!("{icon}"), icon.as_str());
        }
    }
}
