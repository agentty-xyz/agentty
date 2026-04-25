//! Theme-aware semantic color helpers for Agentty's terminal UI.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};

use ratatui::style::{Color, Style};

use super::icon::Icon;
use crate::domain::session::{ReviewRequestState, Status};
use crate::domain::theme::ColorTheme;

static ACTIVE_THEME: AtomicU8 = AtomicU8::new(theme_index(ColorTheme::Current));
static ACTIVE_THEME_SCOPE_LOCK: Mutex<()> = Mutex::new(());

/// Scoped guard that keeps tests and temporary render checks serialized while
/// they override the process-wide active color theme.
pub(crate) struct ActiveThemeScope {
    previous_theme: ColorTheme,
    _lock_guard: MutexGuard<'static, ()>,
}

impl Drop for ActiveThemeScope {
    fn drop(&mut self) {
        set_active_theme_unlocked(self.previous_theme);
    }
}

/// Shared semantic color tokens for the terminal UI.
pub mod palette {
    use ratatui::style::Color;

    use super::{active_palette, token_color};

    /// Primary accent color used for focused UI elements and titles.
    #[must_use]
    pub fn accent() -> Color {
        token_color(|palette| palette.accent)
    }

    /// Brighter accent used for secondary emphasis.
    #[must_use]
    pub fn accent_soft() -> Color {
        token_color(|palette| palette.accent_soft)
    }

    /// Subtle border and separator color.
    #[must_use]
    pub fn border() -> Color {
        token_color(|palette| palette.border)
    }

    /// Error/danger color for destructive or failed states.
    #[must_use]
    pub fn danger() -> Color {
        token_color(|palette| palette.danger)
    }

    /// Softer danger tone used for graded severity scales.
    #[must_use]
    pub fn danger_soft() -> Color {
        token_color(|palette| palette.danger_soft)
    }

    /// Informational color used for neutral-highlight states.
    #[must_use]
    pub fn info() -> Color {
        token_color(|palette| palette.info)
    }

    /// Color used for question-status emphasis.
    #[must_use]
    pub fn question() -> Color {
        token_color(|palette| palette.question)
    }

    /// Base surface color for bars and selected rows.
    #[must_use]
    pub fn surface() -> Color {
        token_color(|palette| palette.surface)
    }

    /// Subtle blue-gray surface used behind clarification prompt blocks so the
    /// block reads as a recessed inset rather than an elevated panel.
    #[must_use]
    pub fn surface_clarification() -> Color {
        token_color(|palette| palette.surface_clarification)
    }

    /// Subtle danger-tinted surface used behind removed diff lines.
    #[must_use]
    pub fn surface_danger() -> Color {
        token_color(|palette| palette.surface_danger)
    }

    /// Elevated surface color for table headers.
    #[must_use]
    pub fn surface_elevated() -> Color {
        token_color(|palette| palette.surface_elevated)
    }

    /// Subtle success-tinted surface used behind added diff lines.
    #[must_use]
    pub fn surface_success() -> Color {
        token_color(|palette| palette.surface_success)
    }

    /// Dark surface used to dim background content behind modal overlays.
    #[must_use]
    pub fn surface_overlay() -> Color {
        token_color(|palette| palette.surface_overlay)
    }

    /// Primary readable text color.
    #[must_use]
    pub fn text() -> Color {
        token_color(|palette| palette.text)
    }

    /// Muted text color for secondary copy.
    #[must_use]
    pub fn text_muted() -> Color {
        token_color(|palette| palette.text_muted)
    }

    /// Extra-muted text color for placeholders and hints.
    #[must_use]
    pub fn text_subtle() -> Color {
        token_color(|palette| palette.text_subtle)
    }

    /// Success color for positive states.
    #[must_use]
    pub fn success() -> Color {
        token_color(|palette| palette.success)
    }

    /// Softer success tone used for graded severity scales.
    #[must_use]
    pub fn success_soft() -> Color {
        token_color(|palette| palette.success_soft)
    }

    /// Warning color for in-progress and caution states.
    #[must_use]
    pub fn warning() -> Color {
        token_color(|palette| palette.warning)
    }

    /// Softer warning tone used for graded severity scales.
    #[must_use]
    pub fn warning_soft() -> Color {
        token_color(|palette| palette.warning_soft)
    }

    /// Returns the active theme's complete palette for tests and diagnostics.
    #[must_use]
    pub fn active() -> super::ThemePalette {
        active_palette()
    }
}

/// Complete set of semantic colors resolved for one UI theme.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThemePalette {
    /// Primary accent color used for focused UI elements and titles.
    pub accent: Color,
    /// Brighter accent used for secondary emphasis.
    pub accent_soft: Color,
    /// Subtle border and separator color.
    pub border: Color,
    /// Error/danger color for destructive or failed states.
    pub danger: Color,
    /// Softer danger tone used for graded severity scales.
    pub danger_soft: Color,
    /// Informational color used for neutral-highlight states.
    pub info: Color,
    /// Color used for question-status emphasis.
    pub question: Color,
    /// Base surface color for bars and selected rows.
    pub surface: Color,
    /// Subtle blue-gray surface used behind clarification prompt blocks.
    pub surface_clarification: Color,
    /// Subtle danger-tinted surface used behind removed diff lines.
    pub surface_danger: Color,
    /// Elevated surface color for table headers.
    pub surface_elevated: Color,
    /// Subtle success-tinted surface used behind added diff lines.
    pub surface_success: Color,
    /// Dark surface used to dim background content behind modal overlays.
    pub surface_overlay: Color,
    /// Primary readable text color.
    pub text: Color,
    /// Muted text color for secondary copy.
    pub text_muted: Color,
    /// Extra-muted text color for placeholders and hints.
    pub text_subtle: Color,
    /// Success color for positive states.
    pub success: Color,
    /// Softer success tone used for graded severity scales.
    pub success_soft: Color,
    /// Warning color for in-progress and caution states.
    pub warning: Color,
    /// Softer warning tone used for graded severity scales.
    pub warning_soft: Color,
}

const CURRENT_PALETTE: ThemePalette = ThemePalette {
    accent: Color::Cyan,
    accent_soft: Color::LightCyan,
    border: Color::DarkGray,
    danger: Color::Red,
    danger_soft: Color::LightRed,
    info: Color::LightBlue,
    question: Color::LightMagenta,
    surface: Color::DarkGray,
    surface_clarification: Color::Rgb(28, 38, 48),
    surface_danger: Color::Rgb(48, 24, 24),
    surface_elevated: Color::Gray,
    surface_success: Color::Rgb(18, 44, 26),
    surface_overlay: Color::Black,
    text: Color::White,
    text_muted: Color::Gray,
    text_subtle: Color::DarkGray,
    success: Color::Green,
    success_soft: Color::LightGreen,
    warning: Color::Yellow,
    warning_soft: Color::LightYellow,
};

/// Green phosphor terminal palette with restrained surfaces and soft contrast.
const HACKER_PALETTE: ThemePalette = ThemePalette {
    accent: Color::Rgb(77, 222, 105),
    accent_soft: Color::Rgb(47, 176, 80),
    border: Color::Rgb(74, 132, 78),
    danger: Color::Rgb(238, 89, 89),
    danger_soft: Color::Rgb(196, 80, 80),
    info: Color::Rgb(113, 143, 107),
    question: Color::Rgb(157, 241, 171),
    surface: Color::Rgb(6, 18, 8),
    surface_clarification: Color::Rgb(12, 28, 14),
    surface_danger: Color::Rgb(44, 18, 18),
    surface_elevated: Color::Rgb(10, 32, 12),
    surface_success: Color::Rgb(12, 38, 17),
    surface_overlay: Color::Black,
    text: Color::Rgb(205, 245, 211),
    text_muted: Color::Rgb(126, 159, 120),
    text_subtle: Color::Rgb(54, 88, 58),
    success: Color::Rgb(77, 222, 105),
    success_soft: Color::Rgb(47, 176, 80),
    warning: Color::Rgb(213, 199, 104),
    warning_soft: Color::Rgb(230, 222, 139),
};

/// Sets the process-wide active color theme used by semantic palette tokens.
pub fn set_active_theme(theme: ColorTheme) {
    let _lock_guard = ACTIVE_THEME_SCOPE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    set_active_theme_unlocked(theme);
}

/// Temporarily sets the process-wide active theme until the returned guard is
/// dropped.
///
/// The guard serializes callers that need deterministic palette reads while
/// holding a temporary theme override.
#[must_use]
pub(crate) fn scoped_active_theme(theme: ColorTheme) -> ActiveThemeScope {
    let lock_guard = ACTIVE_THEME_SCOPE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous_theme = active_theme();
    set_active_theme_unlocked(theme);

    ActiveThemeScope {
        previous_theme,
        _lock_guard: lock_guard,
    }
}

/// Returns the common border style for framed panels and table widgets.
#[must_use]
pub fn border_style() -> Style {
    Style::default().fg(palette::border())
}

/// Returns a stable cache discriminator for the active color theme.
#[must_use]
pub(crate) fn active_theme_cache_version() -> u64 {
    u64::from(ACTIVE_THEME.load(Ordering::Relaxed))
}

/// Returns the terminal color used for one session status label.
#[must_use]
pub fn status_color(status: Status) -> Color {
    match status {
        Status::New => palette::text_muted(),
        Status::InProgress => palette::warning(),
        Status::Review | Status::AgentReview => palette::info(),
        Status::Question => palette::question(),
        Status::Queued => palette::accent_soft(),
        Status::Rebasing | Status::Merging => palette::accent(),
        Status::Done => palette::success(),
        Status::Canceled => palette::danger(),
    }
}

/// Returns the icon used for one session status indicator.
#[must_use]
pub fn status_icon(status: Status) -> Icon {
    match status {
        Status::New | Status::Review | Status::Question | Status::Queued => Icon::Pending,
        Status::InProgress | Status::AgentReview | Status::Rebasing | Status::Merging => {
            Icon::current_spinner()
        }
        Status::Done => Icon::Check,
        Status::Canceled => Icon::Cross,
    }
}

/// Returns the terminal color for a forge indicator suffix.
///
/// The color reflects the review-request lifecycle state when one is linked,
/// or a soft accent when only a published branch exists (`None`).
#[must_use]
pub fn forge_indicator_color(state: Option<ReviewRequestState>) -> Color {
    match state {
        Some(ReviewRequestState::Open) => palette::warning(),
        Some(ReviewRequestState::Merged) => palette::success(),
        Some(ReviewRequestState::Closed) => palette::danger(),
        None => palette::accent_soft(),
    }
}

/// Returns the complete color palette for the active theme.
#[must_use]
fn active_palette() -> ThemePalette {
    match active_theme() {
        ColorTheme::Hacker => HACKER_PALETTE,
        ColorTheme::Current => CURRENT_PALETTE,
    }
}

/// Returns the active theme represented by the process-wide theme atom.
#[must_use]
fn active_theme() -> ColorTheme {
    match ACTIVE_THEME.load(Ordering::Relaxed) {
        1 => ColorTheme::Hacker,
        _ => ColorTheme::Current,
    }
}

/// Stores a theme value after callers have already handled serialization.
fn set_active_theme_unlocked(theme: ColorTheme) {
    ACTIVE_THEME.store(theme_index(theme), Ordering::Relaxed);
}

/// Applies one semantic color selector to the active palette.
#[must_use]
fn token_color(selector: impl FnOnce(ThemePalette) -> Color) -> Color {
    selector(active_palette())
}

/// Returns the stable numeric storage for one theme in the active-theme atom.
const fn theme_index(theme: ColorTheme) -> u8 {
    match theme {
        ColorTheme::Current => 0,
        ColorTheme::Hacker => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_color_returns_muted_text_for_new() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = status_color(Status::New);

        // Assert
        assert_eq!(color, palette::text_muted());
    }

    #[test]
    fn status_color_returns_success_for_done() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = status_color(Status::Done);

        // Assert
        assert_eq!(color, palette::success());
    }

    #[test]
    fn status_color_returns_danger_for_canceled() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = status_color(Status::Canceled);

        // Assert
        assert_eq!(color, palette::danger());
    }

    #[test]
    fn status_color_returns_warning_for_in_progress() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = status_color(Status::InProgress);

        // Assert
        assert_eq!(color, palette::warning());
    }

    #[test]
    fn status_color_returns_info_for_review() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = status_color(Status::Review);

        // Assert
        assert_eq!(color, palette::info());
    }

    #[test]
    fn status_color_returns_accent_for_merging() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = status_color(Status::Merging);

        // Assert
        assert_eq!(color, palette::accent());
    }

    #[test]
    fn status_color_uses_hacker_palette_when_active() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Hacker);

        // Act
        let color = status_color(Status::Merging);

        // Assert
        assert_eq!(color, HACKER_PALETTE.accent);
    }

    #[test]
    fn active_palette_returns_hacker_terminal_green_tones() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Hacker);

        // Act
        let palette = palette::active();

        // Assert
        assert_eq!(palette.surface, Color::Rgb(6, 18, 8));
        assert_eq!(palette.surface_elevated, Color::Rgb(10, 32, 12));
        assert_eq!(palette.border, Color::Rgb(74, 132, 78));
        assert_eq!(palette.text, Color::Rgb(205, 245, 211));
        assert_eq!(palette.text_muted, Color::Rgb(126, 159, 120));
        assert_eq!(palette.accent, Color::Rgb(77, 222, 105));
    }

    #[test]
    fn border_style_uses_active_palette_border_color() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Hacker);

        // Act
        let style = border_style();

        // Assert
        assert_eq!(style.fg, Some(HACKER_PALETTE.border));
    }

    #[test]
    fn status_icon_returns_check_for_done() {
        // Arrange / Act
        let icon = status_icon(Status::Done);

        // Assert
        assert!(matches!(icon, Icon::Check));
    }

    #[test]
    fn status_icon_returns_cross_for_canceled() {
        // Arrange / Act
        let icon = status_icon(Status::Canceled);

        // Assert
        assert!(matches!(icon, Icon::Cross));
    }

    #[test]
    fn status_icon_returns_pending_for_new() {
        // Arrange / Act
        let icon = status_icon(Status::New);

        // Assert
        assert!(matches!(icon, Icon::Pending));
    }

    #[test]
    fn forge_indicator_color_returns_warning_for_open() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = forge_indicator_color(Some(ReviewRequestState::Open));

        // Assert
        assert_eq!(color, palette::warning());
    }

    #[test]
    fn forge_indicator_color_returns_success_for_merged() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = forge_indicator_color(Some(ReviewRequestState::Merged));

        // Assert
        assert_eq!(color, palette::success());
    }

    #[test]
    fn forge_indicator_color_returns_danger_for_closed() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = forge_indicator_color(Some(ReviewRequestState::Closed));

        // Assert
        assert_eq!(color, palette::danger());
    }

    #[test]
    fn forge_indicator_color_returns_accent_soft_for_published_only() {
        // Arrange
        let _theme_scope = scoped_active_theme(ColorTheme::Current);

        // Act
        let color = forge_indicator_color(None);

        // Assert
        assert_eq!(color, palette::accent_soft());
    }
}
