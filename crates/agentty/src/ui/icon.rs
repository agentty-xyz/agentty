use std::fmt;

/// Stable loader glyph painted by the Tachyonfx session-output effect.
pub(crate) const TACHYON_LOADER_GLYPH: &str = "▌▌▌";
/// Display width of [`TACHYON_LOADER_GLYPH`] in terminal cells.
pub(crate) const TACHYON_LOADER_WIDTH: u16 = 3;
/// Stable queued-action glyph painted by the calm pulse effect.
pub(crate) const QUEUED_ACTION_GLYPH: &str = "≡";
/// Display width of [`QUEUED_ACTION_GLYPH`] in terminal cells.
pub(crate) const QUEUED_ACTION_WIDTH: u16 = 1;

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
    /// A queued-action symbol (≡).
    QueuedAction,
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
            Icon::QueuedAction => QUEUED_ACTION_GLYPH,
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
#[path = "icon_test.rs"]
mod tests;
